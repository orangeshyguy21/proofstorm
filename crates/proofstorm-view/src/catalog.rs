//! Shared catalog discovery contracts for agents and the browser.
use proofstorm_core::{
    CatalogFeature, CatalogOrigin, ComponentKind, LinkKind, ReleaseChannel, SupportLifecycle,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const fn default_case_insensitive() -> bool {
    true
}
const fn default_catalog_list_limit() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogDependencyFilter {
    pub link_kind: LinkKind,
    pub implementation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogListRequest {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default = "default_case_insensitive")]
    pub case_insensitive: bool,
    #[serde(default)]
    pub origins: BTreeSet<CatalogOrigin>,
    #[serde(default)]
    pub scan: bool,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub implementations: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub kinds: BTreeSet<ComponentKind>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub features_all: BTreeSet<CatalogFeature>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub release_channels: BTreeSet<ReleaseChannel>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub support_lifecycles: BTreeSet<SupportLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<CatalogDependencyFilter>,
    #[serde(default = "default_catalog_list_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    /// Opaque continuation token returned by a prior call with identical filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl Default for CatalogListRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            regex: false,
            case_insensitive: true,
            origins: BTreeSet::new(),
            scan: false,
            fields: Vec::new(),
            implementations: BTreeSet::new(),
            kinds: BTreeSet::new(),
            features_all: BTreeSet::new(),
            release_channels: BTreeSet::new(),
            support_lifecycles: BTreeSet::new(),
            dependency: None,
            limit: default_catalog_list_limit(),
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CatalogPage {
    pub api_version: String,
    pub catalog_digest: String,
    pub matched_count: usize,
    pub items: Vec<serde_json::Value>,
    pub next_cursor: Option<String>,
}
