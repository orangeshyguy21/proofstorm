use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    NodeStart,
    NodeStop,
    NodeRestart,
    BootstrapLiquidity,
    PeerConnect,
    PeerDisconnect,
    ChannelOpen,
    ChannelClose,
    ChannelForceClose,
    ChannelRebalance,
    NetworkPartition,
    NetworkDelay,
    NetworkLoss,
    NetworkHeal,
    WalletInitialize,
    WalletBalance,
    WalletFund,
    WalletInvoice,
    WalletPay,
    WalletQuoteClaim,
    WalletRoundTrip,
    ConservationOracle,
    ReachabilityOracle,
    NativeExec,
    ComponentLogs,
    AuthenticationConformance,
    AuthenticationProtectedSpend,
    AuthenticationReplay,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabOperation {
    pub id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub principal_id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub capability: crate::Capability,
    pub resource_name: String,
    pub request_digest: String,
    pub request: Value,
    pub phase: OperationPhase,
    pub accepted_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<OperationArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationArtifact {
    pub media_type: String,
    pub digest: String,
    pub byte_length: u32,
    pub content: Value,
}
