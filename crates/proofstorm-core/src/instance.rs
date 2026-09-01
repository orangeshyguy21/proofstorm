use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ComponentKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabInstance {
    pub id: String,
    pub workspace_id: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub instance_key: String,
    pub resource_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstancePhase {
    #[default]
    Pending,
    Ready,
    Closing,
    Closed,
    CleanupBlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentStatus {
    pub id: String,
    pub kind: ComponentKind,
    pub ready: bool,
    pub service: String,
    pub ports: BTreeMap<String, u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InventoryEntry {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TeardownReceipt {
    pub instance_id: String,
    pub instance_namespace: String,
    pub inventory_digest: String,
    pub verified_absent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabInstanceStatus {
    pub instance: LabInstance,
    pub phase: InstancePhase,
    pub instance_namespace: String,
    pub components: Vec<ComponentStatus>,
    pub inventory: Vec<InventoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<TeardownReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
