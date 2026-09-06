//! Shared, browser-compatible environment contracts. No Kubernetes or `SQLite` dependency.
#![allow(
    clippy::missing_errors_doc,
    reason = "query validation returns a static explanation"
)]
use proofstorm_core::{
    ComponentConditionReason, ComponentConditionState, ComponentConditionType, ComponentKind,
    InstancePhase, LabOperation, LinkKind, OperationKind, OperationPhase, Session,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentQuery {
    pub instance_id: Option<String>,
    pub cursor: String,
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    pub session_cursor: String,
    pub activity_cursor: String,
    pub component_cursor: String,
    pub link_cursor: String,
}
impl Default for EnvironmentQuery {
    fn default() -> Self {
        Self {
            instance_id: None,
            cursor: String::new(),
            limit: 20,
            session_cursor: String::new(),
            activity_cursor: String::new(),
            component_cursor: String::new(),
            link_cursor: String::new(),
        }
    }
}
impl EnvironmentQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=50).contains(&self.limit)
            || [
                &self.cursor,
                &self.session_cursor,
                &self.activity_cursor,
                &self.component_cursor,
                &self.link_cursor,
            ]
            .iter()
            .any(|s| s.len() > 128)
            || self
                .instance_id
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 128)
        {
            return Err("limit must be 1..=50; IDs and cursors must be at most 128 bytes");
        }
        if (self.instance_id.is_none()
            && (!self.session_cursor.is_empty()
                || !self.activity_cursor.is_empty()
                || !self.component_cursor.is_empty()
                || !self.link_cursor.is_empty()))
            || (self.instance_id.is_some() && !self.cursor.is_empty())
        {
            return Err(
                "section cursors require instance_id; the lab cursor requires an environment page",
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnvironmentView {
    pub api_version: String,
    pub workspace_id: String,
    pub scope: String,
    pub observation_started_at_unix: i64,
    pub observation_finished_at_unix: i64,
    pub labs: Page<EnvironmentLab>,
    pub coverage: Coverage,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Coverage {
    pub topology: String,
    pub activity: String,
    pub resource_demand: String,
    pub resource_usage: String,
    pub protocol_traffic: String,
    pub attached_clients: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnvironmentLab {
    pub id: String,
    pub handle: Option<LabHandle>,
    pub revision_digest: Option<String>,
    pub journal_read_at_unix: i64,
    pub last_recorded_activity_at_unix: Option<i64>,
    pub runtime: RuntimeObservation,
    pub components: Page<ComponentView>,
    pub links: Page<LinkView>,
    pub resources: Option<ResourceDemand>,
    pub resource_error: Option<String>,
    pub sessions: Page<SessionView>,
    pub activity: Page<Activity>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    Available,
    Stale,
    Missing,
    Unavailable,
    NotMaterialized,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeObservation {
    pub state: ObservationState,
    pub fetched_at_unix: i64,
    pub source_updated_at_unix: Option<i64>,
    pub resource_version: Option<String>,
    pub generation: Option<i64>,
    pub observed_generation: Option<i64>,
    pub phase: Option<InstancePhase>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ComponentView {
    pub id: String,
    pub kind: ComponentKind,
    pub implementation: String,
    pub version: Option<String>,
    pub ready: Option<bool>,
    pub conditions: Vec<ConditionView>,
    pub endpoints: Vec<Endpoint>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConditionView {
    pub condition_type: ComponentConditionType,
    pub state: ComponentConditionState,
    pub reason: ComponentConditionReason,
    pub last_transition_unix: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinkView {
    pub id: String,
    pub kind: LinkKind,
    pub from: String,
    pub to: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionView {
    pub session: Session,
    pub overlapping_session_count: i64,
}

pub type Quantities = BTreeMap<String, String>;
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Endpoint {
    pub component: String,
    pub name: String,
    pub transport: String,
    pub cluster_host: String,
    pub port: i32,
    pub local_connection_supported: bool,
    pub local_authentication: Option<Authentication>,
    pub access_context: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResourceDemand {
    pub workloads: Vec<WorkloadDemand>,
    pub storage: Vec<StorageDemand>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkloadDemand {
    pub name: String,
    pub component: Option<String>,
    pub replicas: i32,
    pub containers: Vec<ContainerDemand>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContainerDemand {
    pub name: String,
    pub init: bool,
    pub requests: Quantities,
    pub limits: Quantities,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageDemand {
    pub name: String,
    pub workload: Option<String>,
    pub replicas: i32,
    pub component: Option<String>,
    pub requests: Quantities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LabHandlePhase {
    Open,
    Closing,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LabHandle {
    pub name: String,
    pub generation: u32,
    pub owner: String,
    pub config_digest: String,
    pub phase: LabHandlePhase,
    pub instance_id: String,
}
impl LabHandle {
    #[must_use]
    pub fn run_id(&self) -> String {
        format!("run-{}", self.instance_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    None,
    Basic,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Activity {
    pub id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub phase: OperationPhase,
    pub accepted_at_unix: i64,
    pub completed_at_unix: Option<i64>,
    pub artifact_digest: Option<String>,
    pub session_id: String,
    pub run_id: String,
    pub native_exit_code: Option<i64>,
    pub native_timed_out: Option<bool>,
    pub cleanup_verified: Option<bool>,
    pub principal_id: String,
    pub components: Vec<String>,
}
impl From<LabOperation> for Activity {
    fn from(op: LabOperation) -> Self {
        Self {
            run_id: op.experiment_id,
            native_exit_code: op
                .artifact
                .as_ref()
                .and_then(|a| a.content["exit_code"].as_i64()),
            native_timed_out: op
                .artifact
                .as_ref()
                .and_then(|a| a.content["timed_out"].as_bool()),
            cleanup_verified: op
                .artifact
                .as_ref()
                .and_then(|a| a.content["cleanup_verified"].as_bool()),
            session_id: op.session_id,
            principal_id: op.principal_id,
            components: [
                "/component",
                "/wallet",
                "/mint",
                "/chain",
                "/from_lightning",
                "/to_lightning",
                "/payer_lightning",
                "/mint_lightning",
                "/target_component",
                "/from_component",
                "/to_component",
                "/lightning",
                "/recipient_wallet",
                "/recipient_mint",
                "/transfer/component",
                "/transfer/destinationComponent",
            ]
            .into_iter()
            .filter_map(|path| {
                op.request
                    .pointer(path)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
            id: op.id,
            sequence: op.sequence,
            kind: op.kind,
            phase: op.phase,
            accepted_at_unix: op.accepted_at_unix,
            completed_at_unix: op.completed_at_unix,
            artifact_digest: op.artifact.map(|a| a.digest),
        }
    }
}

/// Health of the background receipt collector owned by the web server.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ObserverStatus {
    pub state: String,
    pub last_attempt_at_unix: Option<i64>,
    pub last_success_at_unix: Option<i64>,
    pub recorded_operations: u64,
    pub error: Option<String>,
}
