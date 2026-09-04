use std::collections::BTreeMap;

use kube::CustomResource;
use proofstorm_core::{Capability, ComponentStatus, InventoryEntry, LabSpec, ResolvedLock};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(CustomResource, Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "proofstorm.dev",
    version = "v1alpha1",
    kind = "ProofstormLab",
    plural = "proofstormlabs",
    shortname = "pslab",
    namespaced,
    status = "ProofstormLabStatus"
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofstormLabSpec {
    pub workspace_id: String,
    pub instance_id: String,
    /// Stable opaque identity used to derive the instance namespace.
    pub instance_key: String,
    /// Digest of the immutable resolved lab revision.
    pub revision_digest: String,
    pub lock: ResolvedLock,
    pub lab: LabSpec,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum LabPhase {
    #[default]
    Pending,
    Ready,
    Closing,
    CleanupBlocked,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeardownReceipt {
    pub instance_id: String,
    pub instance_namespace: String,
    pub inventory_digest: String,
    pub verified_absent: bool,
    pub checked_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofstormLabStatus {
    pub phase: LabPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Immutable lab revision against which this status was observed.
    pub observed_revision_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_protocol_probe_lease: Option<String>,
    #[serde(default)]
    pub components: Vec<ComponentStatus>,
    #[serde(default)]
    pub inventory: Vec<InventoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<TeardownReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(CustomResource, Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "proofstorm.dev",
    version = "v1alpha1",
    kind = "ProofstormLabAction",
    plural = "proofstormlabactions",
    shortname = "psaction",
    namespaced,
    status = "ProofstormLabActionStatus"
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofstormLabActionSpec {
    pub lab_name: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub instance_key: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub principal_id: String,
    pub sequence: u64,
    pub operation_id: String,
    pub request_digest: String,
    pub capability: Capability,
    pub accepted_at_unix: i64,
    #[schemars(with = "LabActionSchema")]
    pub action: LabAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "snake_case")]
pub enum LabAction {
    NodeStart(NodeControlAction),
    NodeStop(NodeControlAction),
    NodeRestart(NodeControlAction),
    BootstrapLiquidity(BootstrapLiquidityAction),
    PeerConnect(PeerConnectAction),
    PeerDisconnect(PeerDisconnectAction),
    ChannelOpen(ChannelOpenAction),
    ChannelClose(ChannelCloseAction),
    ChannelForceClose(ChannelCloseAction),
    ChannelRebalance(ChannelRebalanceAction),
    NetworkPartition(NetworkPartitionAction),
    NetworkHeal(NetworkHealAction),
    WalletInitialize(WalletInitializeAction),
    WalletBalance(WalletBalanceAction),
    WalletFund(WalletFundAction),
    WalletInvoice(WalletInvoiceAction),
    WalletPay(WalletPayAction),
    WalletQuoteClaim(WalletQuoteClaimAction),
    WalletRoundTrip(WalletRoundTripAction),
    ConservationOracle(ConservationOracleAction),
    ReachabilityOracle(ReachabilityOracleAction),
    NativeExec(NativeExecAction),
    ComponentLogs(ComponentLogsAction),
    AuthenticationConformance(AuthenticationConformanceAction),
    AuthenticationProtectedSpend(AuthenticationProtectedSpendAction),
    AuthenticationReplay(AuthenticationReplayAction),
}

// Kubernetes structural schemas cannot merge the different `kind` constants
// generated for an internally tagged Rust enum. This schema admits only the
// union of known action fields; serde still enforces the exact kind/parameter
// pairing before proofstormd can reconcile an action.
#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct LabActionSchema {
    kind: LabActionKindSchema,
    parameters: LabActionParametersSchema,
}

#[derive(JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum LabActionKindSchema {
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

#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct LabActionParametersSchema {
    component: Option<String>,
    target_component: Option<String>,
    chain: Option<String>,
    mint_lightning: Option<String>,
    payer_lightning: Option<String>,
    funding_sat: Option<u64>,
    channel_sat: Option<u64>,
    push_sat: Option<u64>,
    from_lightning: Option<String>,
    to_lightning: Option<String>,
    lightning: Option<String>,
    channel_id: Option<String>,
    outgoing_channel_id: Option<String>,
    incoming_channel_id: Option<String>,
    max_fee_sat: Option<u64>,
    from_component: Option<String>,
    to_component: Option<String>,
    partition_operation_id: Option<String>,
    wallet: Option<String>,
    recipient_wallet: Option<String>,
    recipient_mint: Option<String>,
    mint: Option<String>,
    identity_provider: Option<String>,
    session_secret: Option<String>,
    source_operation_id: Option<String>,
    quote_id: Option<String>,
    mint_quote_id: Option<String>,
    amount_sat: Option<u64>,
    timeout_seconds: Option<u32>,
    expected_sat: Option<u64>,
    tolerance_sat: Option<u64>,
    service: Option<String>,
    attempts: Option<u32>,
    script: Option<String>,
    tail_lines: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeControlAction {
    pub component: String,
}

/// A bounded read of one component's own container log.
///
/// The controller fulfills this directly rather than rendering a Job: lab
/// workloads deliberately carry no Kubernetes credentials, and the log must
/// stay readable when the component is unready, crash-looping, or stopped,
/// which is exactly when it is worth reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentLogsAction {
    pub component: String,
    pub tail_lines: u32,
}

/// Run the fixed positive and negative OIDC/CAT/BAT baseline without exposing
/// the disposable identity credential or issued bearer material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticationConformanceAction {
    pub mint: String,
    pub identity_provider: String,
}

/// Mint and spend a BAT while retaining the spent token inside the lab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticationProtectedSpendAction {
    pub mint: String,
    pub identity_provider: String,
}

/// Replay a previously spent BAT after a restart, then prove fresh BAT use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticationReplayAction {
    pub mint: String,
    pub identity_provider: String,
    /// Controller-derived Secret for the source operation's opaque session.
    pub session_secret: String,
    pub source_operation_id: String,
}

/// Secret-free result emitted by the fixed authentication conformance driver.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the wire contract records independent conformance observations, not interchangeable state"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationConformanceResult {
    pub contract: String,
    pub mint: String,
    pub identity_provider: String,
    pub advertised_nut21: bool,
    pub advertised_nut22: bool,
    pub invalid_oidc_password_rejected: bool,
    pub missing_cat_rejected: bool,
    pub invalid_cat_code: Option<u32>,
    pub missing_bat_rejected: bool,
    pub invalid_bat_code: Option<u32>,
    pub oidc_login: bool,
    pub claims_match: bool,
    pub mint_accepted_cat: bool,
    pub bat_issued: bool,
    pub bat_dleq: bool,
    pub bat_max_code: Option<u32>,
    pub rate_limit_code: Option<u32>,
    pub conformant: bool,
    pub failure_stage: Option<AuthenticationConformanceFailureStage>,
    pub failure_status: Option<u16>,
    pub failure_protocol_code: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationConformanceFailureStage {
    MintInfo,
    AuthAdvertisement,
    AuthPolicy,
    OidcDiscovery,
    InvalidOidcPassword,
    MissingCat,
    InvalidCat,
    MissingBat,
    InvalidBat,
    OidcLogin,
    OidcClaims,
    BatMaximum,
    BatIssuance,
    BatSignature,
    CatRateLimit,
}

/// Secret-free result of minting and spending a valid BAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationProtectedSpendResult {
    pub contract: String,
    pub mint: String,
    pub identity_provider: String,
    pub bat_count: u32,
    pub bat_dleq: bool,
    pub protected_request: bool,
    pub session_operation_id: Option<String>,
    pub conformant: bool,
    pub failure_stage: Option<AuthenticationSessionFailureStage>,
    pub failure_status: Option<u16>,
    pub failure_protocol_code: Option<u32>,
}

/// Secret-free replay and fresh-authentication result after a mint restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationReplayResult {
    pub contract: String,
    pub mint: String,
    pub identity_provider: String,
    pub source_operation_id: String,
    pub spent_bat_replay_code: Option<u32>,
    pub fresh_bat_count: u32,
    pub fresh_bat_dleq: bool,
    pub protected_request: bool,
    pub conformant: bool,
    pub failure_stage: Option<AuthenticationSessionFailureStage>,
    pub failure_status: Option<u16>,
    pub failure_protocol_code: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationSessionFailureStage {
    OidcLogin,
    BatIssuance,
    BatSignature,
    ProtectedRequest,
    SpentBatReplay,
}

/// An unrestricted shell program executed in a component's locked image.
///
/// The Kubernetes renderer, rather than the caller, owns namespace placement,
/// credentials, volumes, service account, and pod security.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeExecAction {
    pub component: String,
    /// Component whose service metadata is exposed to the native command.
    /// Defaults to the execution component at the MCP boundary.
    pub target_component: String,
    pub script: String,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapLiquidityAction {
    pub chain: String,
    pub mint_lightning: String,
    pub payer_lightning: String,
    pub funding_sat: u64,
    pub channel_sat: u64,
    pub push_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerConnectAction {
    pub from_lightning: String,
    pub to_lightning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerDisconnectAction {
    pub from_lightning: String,
    pub to_lightning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelOpenAction {
    pub chain: String,
    pub from_lightning: String,
    pub to_lightning: String,
    pub channel_sat: u64,
    pub push_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelCloseAction {
    pub chain: String,
    pub from_lightning: String,
    pub to_lightning: String,
    /// Opaque Proofstorm handle returned by channel creation.
    pub channel_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelRebalanceAction {
    pub lightning: String,
    /// Opaque Proofstorm handle for the channel that sends the circular payment.
    pub outgoing_channel_id: String,
    /// Opaque Proofstorm handle for the channel that receives the circular payment.
    pub incoming_channel_id: String,
    pub amount_sat: u64,
    pub max_fee_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPartitionAction {
    pub from_component: String,
    pub to_component: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkHealAction {
    /// Durable operation ID of the partition being healed.
    pub partition_operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletInitializeAction {
    pub wallet: String,
    pub mint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletBalanceAction {
    pub wallet: String,
    pub mint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletFundAction {
    pub wallet: String,
    pub mint: String,
    pub payer_lightning: String,
    pub amount_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletInvoiceAction {
    pub wallet: String,
    pub mint: String,
    pub amount_sat: u64,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletPayAction {
    pub wallet: String,
    pub mint: String,
    pub recipient_wallet: String,
    pub recipient_mint: String,
    pub mint_quote_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletQuoteClaimAction {
    pub wallet: String,
    pub mint: String,
    pub mint_quote_id: String,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WalletRoundTripAction {
    pub wallet: String,
    pub mint: String,
    pub payer_lightning: String,
    pub amount_sat: u64,
    pub tolerance_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConservationOracleAction {
    pub wallet: String,
    pub mint: String,
    pub expected_sat: u64,
    pub tolerance_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReachabilityOracleAction {
    pub from_component: String,
    pub to_component: String,
    /// Logical service advertised by the destination component adapter.
    pub service: String,
    pub timeout_seconds: u32,
    pub attempts: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum ActionPhase {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

fn preserved_object_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "object",
        "x-kubernetes-preserve-unknown-fields": true
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProofstormLabActionStatus {
    pub phase: ActionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "preserved_object_schema")]
    pub artifact: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "preserved_object_schema")]
    pub error: Option<BTreeMap<String, Value>>,
}
