//! Catalog-derived qualification plans and strict execution evidence.
//! This maintainer crate is shared by CI and the owned acceptance runner; it is
//! not linked into the installed application.
mod planner;
mod receipt;

pub use planner::{catalog, plan};
pub use receipt::{ImageEvidence, Receipt, aggregate, verify_receipts};

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Only an explicit, nonempty list of ordinary documentation paths can reduce
/// the compatibility matrix. Unknown files and a failed/empty comparison select
/// every compatibility case; upstream stress remains an explicit opt-in.
#[must_use]
pub fn documentation_only(paths: &[String]) -> bool {
    !paths.is_empty()
        && paths.iter().all(|path| {
            matches!(
                path.as_str(),
                "README.md" | "CHANGELOG.md" | "CONTRIBUTING.md" | "scripts/CHECKS.md"
            ) || (path.starts_with("docs/")
                && std::path::Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext == "md")
                && !path.contains(".."))
        })
}

#[cfg(test)]
mod policy_tests {
    use super::documentation_only;
    #[test]
    fn omissions_require_an_explicit_documentation_only_comparison() {
        assert!(documentation_only(&[
            "README.md".into(),
            "docs/usage.md".into()
        ]));
        for paths in [
            vec![],
            vec!["docs/app.js".into()],
            vec!["README.md".into(), "Cargo.toml".into()],
            vec!["unknown.md".into()],
        ] {
            assert!(!documentation_only(&paths));
        }
    }
}

/// The exact checkout and hosted run whose evidence is being collected.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub revision: String,
    pub run_id: String,
    pub attempt: u64,
}

impl Identity {
    /// Reject ambiguous or path-shaped identities before constructing artifacts.
    ///
    /// # Errors
    /// Returns an error for a malformed revision, run ID or attempt.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.revision.len() == 40
                && self.revision.bytes().all(|b| b.is_ascii_hexdigit())
                && !self.run_id.is_empty()
                && self.run_id.bytes().all(|b| b.is_ascii_digit())
                && self.attempt > 0,
            "invalid qualification run identity"
        );
        Ok(())
    }
}

/// One catalog identity, including the exact source used by installation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub implementation: String,
    pub version: String,
    pub image: String,
    pub source: String,
}

/// Inputs to the shared real mint/wallet round-trip scenario.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MintRoundtrip {
    pub mint: Component,
    pub wallet: Component,
    pub lightning: Component,
    pub storage: String,
}

/// Executable scenario, kept typed so a plan cannot inject shell commands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scenario {
    Image {
        component: Component,
    },
    Lightning {
        component: Component,
    },
    Mint {
        configuration: Box<MintRoundtrip>,
    },
    Gate {
        name: String,
        versions: BTreeMap<String, String>,
    },
}

/// Required assertions and immutable inputs for one native execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub platform: String,
    pub scenario: Scenario,
    pub components: Vec<Component>,
    pub claims: BTreeSet<String>,
    pub required: bool,
    pub reason: String,
}

/// Required compatibility checks are separate from opt-in upstream stress tests.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Documentation,
    Compatibility,
    Full,
}

/// A complete, deterministic obligation set for both native architectures.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub format_version: u32,
    pub identity: Identity,
    pub mode: Mode,
    pub catalog_digests: BTreeMap<String, String>,
    pub obligations: BTreeMap<String, BTreeSet<String>>,
    pub cases: Vec<Case>,
}

impl Plan {
    /// Content identity includes scenario selection and the hosted run attempt.
    #[must_use]
    pub fn digest(&self) -> String {
        proofstorm_core::digest_json(self)
    }

    /// Look up one exact case; identifiers are never interpreted as paths.
    ///
    /// # Errors
    /// Returns an error when the case is absent from the plan.
    pub fn case(&self, id: &str) -> Result<&Case> {
        self.cases
            .iter()
            .find(|case| case.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown qualification case {id}"))
    }

    /// Validate coverage and bind all inputs to the current compiled catalog.
    ///
    /// # Errors
    /// Rejects malformed identities and any plan differing from this source.
    pub fn validate(&self) -> Result<()> {
        self.identity.validate()?;
        ensure!(
            self.format_version == 2,
            "unknown qualification plan format"
        );
        let expected = plan(self.identity.clone(), self.mode)?;
        ensure!(
            self == &expected,
            "qualification plan differs from this source/catalog"
        );
        Ok(())
    }
}
