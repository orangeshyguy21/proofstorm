//! Versioned build recipes. A build retains this complete declaration at admission.
use crate::{CatalogFeature, digest_json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CANDIDATE_BUILD_MAX_CPU_MILLICORES: u32 = 4000;
pub const CANDIDATE_BUILD_MAX_DEADLINE_SECONDS: u32 = 3600;
pub const CDK_MINT_IMPLEMENTATIONS: [&str; 1] = ["cdk"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateBuildProfile {
    pub id: String,
    pub version: u32,
    pub repository: String,
    pub baseline: String,
    pub dockerfile: String,
    pub platforms: BTreeSet<String>,
    pub features: BTreeSet<CatalogFeature>,
    /// Runtime presets served by this artifact. Empty in historical profiles,
    /// which remain restricted to the implementation requested at admission.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub catalog_implementations: BTreeSet<String>,
    pub notes: Vec<String>,
    /// Trusted, versioned preparation recipe copied into the immutable build record.
    pub prepare: String,
    pub cpu_limit_millicores: u32,
    pub memory_limit_mib: u32,
    pub deadline_seconds: u32,
}

impl CandidateBuildProfile {
    #[must_use]
    pub fn digest(&self) -> String {
        digest_json(self)
    }
}

#[must_use]
pub fn candidate_build_profile(implementation: &str) -> Option<CandidateBuildProfile> {
    let (repository, baseline, dockerfile) = match implementation {
        "cdk" => ("cashubtc/cdk", "0.18.1", "Dockerfile.ldk-node"),
        "cdk-cli-wallet" => ("cashubtc/cdk", "0.18.1", "Proofstorm.candidate.Dockerfile"),
        "nutshell" | "nutshell-wallet" => ("cashubtc/nutshell", "0.21.0", "Dockerfile"),
        "cocod-wallet" => (
            "cashubtc/coco",
            "0.0.17-dev.44e5101c",
            "Proofstorm.candidate.Dockerfile",
        ),
        _ => return None,
    };
    let mut features = BTreeSet::new();
    if matches!(implementation, "cdk" | "nutshell") {
        features.insert(CatalogFeature::MintManagementRpc);
    }
    if matches!(
        implementation,
        "nutshell" | "nutshell-wallet" | "cdk-cli-wallet" | "cocod-wallet"
    ) {
        features.insert(CatalogFeature::NativeCliEntrypoints);
    }
    let (prepare, notes) = match implementation {
        "cdk-cli-wallet" | "cocod-wallet" => {
            let recipe = if implementation == "cdk-cli-wallet" { include_str!("candidate_profiles/cdk-wallet-v1.Dockerfile") } else { include_str!("candidate_profiles/coco-wallet-v1.Dockerfile") };
            (format!("cat > /workspace/{dockerfile} <<'PROOFSTORM_RECIPE_EOF'\n{recipe}\nPROOFSTORM_RECIPE_EOF\n"), vec!["Compiles the native wallet from the frozen source with its lockfile; checks command version and help. Build success does not establish runtime compatibility.".into()])
        }
        "nutshell" | "nutshell-wallet" => (format!("sed -i '/RUN poetry install --without dev --no-root/i RUN poetry remove breez-sdk-spark --lock && pip install --no-cache-dir breez-sdk-spark==0.17.0' /workspace/Dockerfile\ncat >> /workspace/Dockerfile <<'PROOFSTORM_MANAGEMENT_EOF'\n{}\nPROOFSTORM_MANAGEMENT_EOF\n", include_str!("candidate_profiles/nutshell-management-v1.Dockerfile")), vec!["Removes breez-sdk-spark from the Poetry lock and installs 0.17.0 separately for historical platform wheels. Installs source console commands and checks cashu/mint-cli help.".into()]),
        _ => (format!("sh -s -- /workspace/{dockerfile} <<'PROOFSTORM_WORKSPACE_EOF'\n{}\nPROOFSTORM_WORKSPACE_EOF\ncat > /tmp/management-prefix <<'PROOFSTORM_MANAGEMENT_EOF'\n{}\nPROOFSTORM_MANAGEMENT_EOF\ncat /tmp/management-prefix /workspace/{dockerfile} > /tmp/management-dockerfile\ncat >> /tmp/management-dockerfile <<'PROOFSTORM_MANAGEMENT_EOF'\n{}\nPROOFSTORM_MANAGEMENT_EOF\nmv /tmp/management-dockerfile /workspace/{dockerfile}", include_str!("candidate_profiles/cdk-mint-workspace-v6.sh"), include_str!("candidate_profiles/cdk-management-v4.Dockerfile"), include_str!("candidate_profiles/cdk-mint-runtime-v7.Dockerfile")), vec!["Builds the CDK mint image using upstream Dockerfile.ldk-node with LDK and PostgreSQL enabled alongside the default backends. Cell links and configuration select linked and embedded backends; PostgreSQL servers stay separate. Uses the full frozen workspace, bindings and lockfiles; requires locked Cargo dependencies. Builds cdk-mint-cli before the daemon with two management-client Cargo jobs and one daemon Cargo job to bound memory, then adds wget and CA certificates required by readiness and Bitcoin dependency checks. Checks daemon, management client and wget commands before publishing.".into()]),
    };
    // A cold ARM64 release build of the baseline CDK CLI exceeded the original
    // two-core, 30-minute budget. Keep that budget in saved v1 records; new Rust
    // profiles receive more CPU and a longer deadline without raising memory.
    let rust_build = repository == "cashubtc/cdk";
    Some(CandidateBuildProfile {
        cpu_limit_millicores: if rust_build {
            CANDIDATE_BUILD_MAX_CPU_MILLICORES
        } else {
            2000
        },
        memory_limit_mib: 4096,
        deadline_seconds: if rust_build {
            CANDIDATE_BUILD_MAX_DEADLINE_SECONDS
        } else {
            1800
        },
        id: if CDK_MINT_IMPLEMENTATIONS.contains(&implementation) {
            "cdk-mint-source".into()
        } else {
            format!("{implementation}-source")
        },
        catalog_implementations: if CDK_MINT_IMPLEMENTATIONS.contains(&implementation) {
            CDK_MINT_IMPLEMENTATIONS.map(str::to_owned).into()
        } else {
            BTreeSet::new()
        },
        version: match implementation {
            "cdk" => 9,
            "cdk-cli-wallet" => 3,
            "nutshell" | "nutshell-wallet" => 2,
            _ => 1,
        },
        repository: repository.into(),
        baseline: baseline.into(),
        dockerfile: dockerfile.into(),
        platforms: BTreeSet::from(["linux/amd64".into(), "linux/arm64".into()]),
        features,
        notes,
        prepare,
    })
}
