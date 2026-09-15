use proofstorm_core::CandidateBuildPhase;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateBuildReceipt {
    pub record_resource_uri: String,
    pub source_resolved: bool,
    pub compatibility_basis: String,
    pub candidate_id: String,
    pub base_version: String,
    /// Use the exact implementation/version in a cell component; `catalog_entry_read` supplies its configuration contract.
    pub catalog_entry: CandidateCatalogSelector,
    /// All runtime presets supplied by the same build and exact image.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_entries: Vec<CandidateCatalogSelector>,
    pub commit_sha: String,
    pub phase: CandidateBuildPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub logs_resource_uri: String,
    /// Configured build transformations to disclose alongside source identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_profile_notes: Vec<String>,
    pub timed_out: bool,
    pub next_tool: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateCatalogSelector {
    pub implementation: String,
    pub version: String,
}
