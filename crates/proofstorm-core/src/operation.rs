use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    NodeStart,
    NodeStop,
    NodeRestart,
    ComponentStart,
    ComponentStop,
    ComponentRestart,
    // Legacy workflow kinds remain readable in immutable journals. Executable
    // actions use ComponentExecLive for all peer/channel interactions.
    BootstrapLiquidity,
    PeerConnect,
    PeerDisconnect,
    ChannelOpen,
    ChannelPolicySet,
    ChannelClose,
    ChannelForceClose,
    ChannelRebalance,
    NetworkPartition,
    NetworkDelay,
    NetworkLoss,
    NetworkHeal,
    // Historical only; initialization and funding now use ComponentExecLive.
    WalletInitialize,
    // Historical only; wallet observations now use ComponentExecLive.
    WalletBalance,
    // Historical only.
    WalletFund,
    // Historical journal kinds; executable wallet workflows have been retired.
    WalletInvoice,
    WalletPay,
    // Historical journal entries only; claims now use native execution.
    WalletQuoteClaim,
    // Historical journal entries only; recovery now uses native execution.
    WalletMeltQuoteRefresh,
    // Historical only; minting and self-swap are separate native executions.
    WalletRoundTrip,
    // Historical wallet accounting receipt.
    ConservationOracle,
    ReachabilityOracle,
    ComponentForensics,
    ComponentExecLive,
    PrivateTransfer,
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
pub struct CellOperation {
    /// Immutable configuration captured atomically at admission.
    #[serde(default)]
    pub revision_digest: String,
    pub id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub experiment_id: String,
    pub session_id: String,
    pub principal_id: String,
    /// Monotonic across this cell, including operations from other actors and runs.
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
