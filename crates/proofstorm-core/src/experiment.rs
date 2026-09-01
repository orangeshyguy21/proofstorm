use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentPhase {
    #[default]
    Active,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    pub id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub owner_principal_id: String,
    pub phase: ExperimentPhase,
    pub created_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_unix: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeasePhase {
    #[default]
    Active,
    Released,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExperimentLease {
    pub id: String,
    pub workspace_id: String,
    pub experiment_id: String,
    pub instance_id: String,
    pub principal_id: String,
    pub phase: LeasePhase,
    pub acquired_at_unix: i64,
    pub expires_at_unix: i64,
    pub max_actions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at_unix: Option<i64>,
}
