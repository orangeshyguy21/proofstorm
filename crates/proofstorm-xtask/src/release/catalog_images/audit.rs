//! Read-only release discovery. Proposals never mutate catalog pins or receipts.
use anyhow::{Context, Result, bail, ensure};
use proofstorm_core::{CatalogResponse, default_catalog, release_policy::release_policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, process::Command};

const PROJECTS: &[(&str, &[&str])] = &[
    ("bitcoin/bitcoin", &["bitcoin-core"]),
    ("ElementsProject/lightning", &["cln"]),
    ("lightningnetwork/lnd", &["lnd"]),
    ("cashubtc/nutshell", &["nutshell", "nutshell-wallet"]),
    (
        "cashubtc/cdk",
        &["cdk", "cdk-ldk", "cdk-bdk", "cdk-cli-wallet"],
    ),
];

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn releases(repository: &str) -> Result<Vec<Release>> {
    let mut releases = Vec::new();
    for page in 1..=20 {
        let url =
            format!("https://api.github.com/repos/{repository}/releases?per_page=100&page={page}");
        let response = Command::new("curl")
            .args([
                "-q",
                "--fail",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--max-time",
                "30",
                "--max-filesize",
                "8388608",
                "--header",
                "Accept: application/vnd.github+json",
                "--user-agent",
                "Proofstorm-release-audit",
                &url,
            ])
            .output()
            .context("read official release feed")?;
        ensure!(
            response.status.success(),
            "release feed unavailable for {repository}; catalog unchanged"
        );
        ensure!(
            response.stdout.len() <= 8 * 1024 * 1024,
            "release page too large"
        );
        let batch: Vec<Release> =
            serde_json::from_slice(&response.stdout).context("invalid release feed")?;
        let complete = batch.len() < 100;
        releases.extend(batch);
        if complete {
            return Ok(releases);
        }
    }
    bail!("release feed pagination limit exceeded for {repository}; no partial proposal")
}

fn proposal(implementation: &str, releases: &[Release], catalog: &CatalogResponse) -> Value {
    let policy = release_policy(implementation).expect("audited project has a release policy");
    let versions = policy.proposed_window(
        releases
            .iter()
            .filter(|release| !release.draft && !release.prerelease)
            .map(|release| release.tag_name.as_str()),
    );
    let versions = versions
        .iter()
        .map(|version| version.strip_prefix('v').unwrap_or(version))
        .collect::<Vec<_>>();
    let desired = versions.iter().copied().collect::<BTreeSet<_>>();
    let complete_window = versions.len() == policy.families;
    let supported = catalog
        .entries
        .iter()
        .filter(|entry| entry.id == implementation && entry.support_lifecycle.is_supported())
        .map(|entry| entry.version.as_str())
        .collect::<BTreeSet<_>>();
    json!({
        "implementation":implementation, "family_limit":policy.families,
        "minimum_release":policy.minimum_release.map(|v| format!("{}.{}.{}", v.major, v.minor, v.patch)),
        "supported_versions":supported, "proposed_versions":versions,
        "qualify":desired.difference(&supported).collect::<Vec<_>>(),
        "retire_after_qualification":if complete_window { supported.difference(&desired).copied().collect::<Vec<_>>() } else { vec![] },
        "complete_window":complete_window,
        "qualification_required":true
    })
}

pub(super) fn run() -> Result<()> {
    let mut projects = Vec::new();
    for (repository, implementations) in PROJECTS {
        let releases = releases(repository)?;
        projects.push(json!({
            "repository":repository,
            "release_url":format!("https://github.com/{repository}/releases"),
            "implementations":implementations.iter().map(|id| proposal(id, &releases, default_catalog())).collect::<Vec<_>>()
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"format_version":1,"read_only":true,"projects":projects})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdk_audit_does_not_backfill_the_window_below_eighteen() {
        let mut releases: Vec<Release> = serde_json::from_value(json!([
            {"tag_name":"v0.17.7","draft":false,"prerelease":false},
            {"tag_name":"v0.18.1","draft":false,"prerelease":false}
        ]))
        .unwrap();
        for implementation in ["cdk", "cdk-ldk", "cdk-bdk", "cdk-cli-wallet"] {
            let result = proposal(implementation, &releases, default_catalog());
            assert_eq!(result["minimum_release"], "0.18.0");
            assert_eq!(result["family_limit"], 2);
            assert_eq!(result["proposed_versions"], json!(["0.18.1"]));
            assert_eq!(result["qualify"], json!([]));
            assert_eq!(result["retire_after_qualification"], json!([]));
        }
        releases.push(Release {
            tag_name: "v0.19.0".into(),
            draft: false,
            prerelease: false,
        });
        let result = proposal("cdk", &releases, default_catalog());
        assert_eq!(result["proposed_versions"], json!(["0.19.0", "0.18.1"]));
        assert_eq!(result["qualify"], json!(["0.19.0"]));
        assert_eq!(result["retire_after_qualification"], json!([]));
    }

    #[test]
    fn publication_order_and_prerelease_flags_cannot_advance_the_window() {
        let releases: Vec<Release> = serde_json::from_value(json!([
            {"tag_name":"v0.22.0-beta","draft":true,"prerelease":false},
            {"tag_name":"v0.22.0-beta","draft":false,"prerelease":true},
            {"tag_name":"v0.23.0-beta.rc1","draft":false,"prerelease":false},
            {"tag_name":"v0.19.3-beta","draft":false,"prerelease":false},
            {"tag_name":"v0.21.3-beta","draft":false,"prerelease":false},
            {"tag_name":"v0.20.4-beta","draft":false,"prerelease":false}
        ]))
        .unwrap();
        let result = proposal("lnd", &releases, default_catalog());
        assert_eq!(
            result["proposed_versions"],
            json!(["0.21.3-beta", "0.20.4-beta", "0.19.3-beta"])
        );
        assert_eq!(result["qualify"], json!(["0.19.3-beta"]));
        assert_eq!(result["retire_after_qualification"], json!([]));
        let missing = proposal("lnd", &[], default_catalog());
        assert_eq!(missing["complete_window"], false);
        assert_eq!(missing["retire_after_qualification"], json!([]));
    }
}
