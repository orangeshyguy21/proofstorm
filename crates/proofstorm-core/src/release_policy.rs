//! Rolling support counts release families, not tags or publication dates.
use std::collections::BTreeMap;

/// The upstream versioning convention and number of supported release families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleasePolicy {
    pub families: usize,
    pub minimum_release: Option<ReleaseVersion>,
    scheme: Scheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Major,
    Calendar,
    PreOne,
    Lnd,
}

/// Numeric release identity. Upstream LND's normal `-beta` is eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Return the official rolling policy; experimental snapshots and infrastructure
/// helpers do not acquire a support promise by appearing in the catalog.
#[must_use]
pub fn release_policy(implementation: &str) -> Option<ReleasePolicy> {
    let (families, scheme, minimum_release) = match implementation {
        "bitcoin-core" => (3, Scheme::Major, None),
        "cln" => (3, Scheme::Calendar, None),
        "lnd" => (3, Scheme::Lnd, None),
        "nutshell" | "nutshell-wallet" => (2, Scheme::PreOne, None),
        "cdk" | "cdk-ldk" | "cdk-bdk" | "cdk-cli-wallet" => (
            2,
            Scheme::PreOne,
            Some(ReleaseVersion {
                major: 0,
                minor: 18,
                patch: 0,
            }),
        ),
        _ => return None,
    };
    Some(ReleasePolicy {
        families,
        minimum_release,
        scheme,
    })
}

impl ReleasePolicy {
    /// Parse stable release tags, including historical releases below the support
    /// floor. Callers reading feeds must also reject drafts/prereleases.
    #[must_use]
    pub fn parse(self, version: &str) -> Option<ReleaseVersion> {
        let version = version.strip_prefix('v').unwrap_or(version);
        let version = if self.scheme == Scheme::Lnd {
            version.strip_suffix("-beta").unwrap_or(version)
        } else {
            version
        };
        let parts = version.split('.').collect::<Vec<_>>();
        let valid_length = match self.scheme {
            Scheme::Major | Scheme::Calendar => matches!(parts.len(), 2 | 3),
            Scheme::PreOne | Scheme::Lnd => parts.len() == 3,
        };
        if !valid_length
            || parts
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        let result = ReleaseVersion {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts.get(2).map_or(Some(0), |part| part.parse().ok())?,
        };
        if self.scheme == Scheme::Calendar && !(1..=12).contains(&result.minor) {
            return None;
        }
        Some(result)
    }

    /// Eligibility for qualification or active support, independent of the rolling
    /// window. Historical versions remain parseable for archived locks.
    #[must_use]
    pub fn is_eligible(self, version: ReleaseVersion) -> bool {
        self.minimum_release
            .is_none_or(|minimum| version >= minimum)
    }

    #[must_use]
    pub fn family(self, version: ReleaseVersion) -> (u32, u32) {
        match self.scheme {
            Scheme::Calendar => (version.major, version.minor),
            Scheme::PreOne | Scheme::Lnd if version.major == 0 => (0, version.minor),
            _ => (version.major, 0),
        }
    }

    /// Select the newest patch in each of the newest families, newest first.
    /// This is a qualification proposal, never automatic catalog promotion.
    #[must_use]
    pub fn proposed_window<'a>(self, releases: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let mut families = BTreeMap::new();
        for tag in releases {
            let Some(version) = self.parse(tag) else {
                continue;
            };
            if !self.is_eligible(version) {
                continue;
            }
            let selected = families
                .entry(self.family(version))
                .or_insert((version, tag));
            if version > selected.0 {
                *selected = (version, tag);
            }
        }
        families
            .into_values()
            .rev()
            .take(self.families)
            .map(|(_, tag)| tag)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_count_families_and_sort_numerically() {
        for (implementation, tags, expected) in [
            (
                "bitcoin-core",
                vec!["v29.4", "v31.1", "v30.3", "v31.0", "v28.4"],
                vec!["v31.1", "v30.3", "v29.4"],
            ),
            (
                "cln",
                vec!["v26.06.2", "v25.12.1", "v26.06.7", "v26.04.1", "v25.09.3"],
                vec!["v26.06.7", "v26.04.1", "v25.12.1"],
            ),
            (
                "lnd",
                vec![
                    "v0.21.3-beta.rc1",
                    "v0.19.3-beta",
                    "v0.21.3-beta",
                    "v0.20.4-beta",
                    "v0.18.5-beta",
                ],
                vec!["v0.21.3-beta", "v0.20.4-beta", "v0.19.3-beta"],
            ),
            (
                "nutshell",
                vec!["0.20.3", "0.21.0", "0.19.2", "0.22.0rc1"],
                vec!["0.21.0", "0.20.3"],
            ),
            (
                "nutshell-wallet",
                vec!["0.9.0", "0.10.0", "0.10.2", "0.11.0-dev"],
                vec!["0.10.2", "0.9.0"],
            ),
            (
                "cdk-cli-wallet",
                vec!["v0.18.0", "v0.17.6", "v0.17.7", "v0.16.1"],
                vec!["v0.18.0"],
            ),
        ] {
            assert_eq!(
                release_policy(implementation)
                    .unwrap()
                    .proposed_window(tags),
                expected
            );
        }
    }

    #[test]
    fn cdk_starts_at_eighteen_then_rolls_over_two_families() {
        for implementation in ["cdk", "cdk-ldk", "cdk-bdk", "cdk-cli-wallet"] {
            let policy = release_policy(implementation).unwrap();
            let historical = policy.parse("v0.17.7").unwrap();
            assert!(!policy.is_eligible(historical));
            assert!(policy.proposed_window(["v0.17.7", "v0.16.1"]).is_empty());
            let mut releases = vec!["v0.17.7", "v0.18.0"];
            assert_eq!(policy.proposed_window(releases.clone()), ["v0.18.0"]);
            releases.extend(["v0.18.2", "v0.19.0"]);
            assert_eq!(
                policy.proposed_window(releases.clone()),
                ["v0.19.0", "v0.18.2"]
            );
            releases.push("v0.20.0");
            assert_eq!(policy.proposed_window(releases), ["v0.20.0", "v0.19.0"]);
        }
    }

    #[test]
    fn excludes_snapshots_and_handles_the_one_point_zero_transition() {
        let policy = release_policy("cdk-cli-wallet").unwrap();
        assert_eq!(
            policy.proposed_window(["0.18.0", "1.0.0", "1.2.0", "2.0.0-rc.1"]),
            ["1.2.0", "0.18.0"]
        );
        for value in [
            "",
            "0.21",
            "0.21.0-dev.abc",
            "0.21.0+build",
            "0.-21.0",
            "0.21.0.0",
            "0.21.0-beta",
        ] {
            assert!(policy.parse(value).is_none(), "{value}");
        }
        assert!(release_policy("cln").unwrap().parse("26.13.1").is_none());
        assert!(release_policy("cocod-wallet").is_none());
    }
}
