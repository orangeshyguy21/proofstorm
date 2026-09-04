use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Patch, PatchParams},
};
use proofstorm_core::{
    AuthenticationProtocol, Capability, CatalogDependencySupport, CatalogEntry, CatalogFeature,
    CatalogSupportMatrix, ComponentKind, ComponentSpec, ComponentStatus, ControlClass,
    DraftMutation, EVIDENCE_API_VERSION, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance, Experiment, ExperimentLease, ExperimentPhase,
    InstancePhase, InventoryEntry, LabInstance, LabInstanceStatus, LabOperation, LabSpec, LinkKind,
    LinkSpec, MAX_NETWORK_DELAY_MS, MAX_NETWORK_JITTER_MS, MAX_NETWORK_LOSS_BASIS_POINTS,
    NetworkFaultBackend, NetworkFaultDirection, NetworkFaultFeature, OperationArtifact,
    OperationKind, OperationPhase, PublishedRevision, ReleaseChannel, SupportLifecycle,
    TeardownReceipt as CoreTeardownReceipt, ValidateLabRequest, ValidationReport,
    WalletQuoteDirection, WalletQuoteObservation, WalletQuoteObservationInput,
    WalletQuoteObservationRole, default_catalog, digest_json, network_policy_fault_backend,
    validate_lab, wallet_quote_observations_from_artifact,
};
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionPhase, AuthenticationConformanceAction,
    AuthenticationProtectedSpendAction, AuthenticationReplayAction, BootstrapLiquidityAction,
    ChannelCloseAction, ChannelOpenAction, ChannelRebalanceAction, ComponentLogsAction,
    ConservationOracleAction, LabAction, LabPhase, NativeExecAction, NetworkHealAction,
    NetworkPartitionAction, NodeControlAction, PeerConnectAction, PeerDisconnectAction,
    ProofstormLab, ProofstormLabAction, ProofstormLabActionSpec, ProofstormLabSpec,
    ReachabilityOracleAction, WalletBalanceAction, WalletFundAction, WalletInitializeAction,
    WalletInvoiceAction, WalletPayAction, WalletQuoteClaimAction, WalletRoundTripAction,
    component_ports,
};
use proofstorm_store::{Draft, DraftDiff, Store, StoreError, Workspace};
use rmcp::{
    ErrorData, Json, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        ListResourceTemplatesResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, ResourceContents, ResourceTemplate,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDraftRequest {
    pub draft_id: String,
    pub lab: LabSpec,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadDraftRequest {
    pub draft_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntryRequest {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfigSchemaRequest {
    pub id: String,
    pub version: String,
    /// RFC 6901 JSON Pointer. Empty reads the complete configuration schema.
    #[serde(default)]
    pub pointer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntrySummary {
    pub id: String,
    pub kind: ComponentKind,
    pub version: String,
    pub preferred: bool,
    pub adapter_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_action_adapter_version: Option<String>,
    pub config_version: String,
    pub config_schema_digest: String,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogListResponse {
    pub api_version: String,
    pub catalog_digest: String,
    pub items: Vec<CatalogEntrySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntryDetail {
    pub id: String,
    pub kind: ComponentKind,
    pub description: String,
    pub adapter_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_action_adapter_version: Option<String>,
    pub version: String,
    pub preferred: bool,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
    pub config_version: String,
    pub config_schema_digest: String,
    pub features: BTreeSet<CatalogFeature>,
    pub compatible_dependencies: Vec<CatalogDependencySupport>,
    pub support_matrix: CatalogSupportMatrix,
    pub image: String,
    pub source_digest: String,
    pub allowed_control: Vec<ControlClass>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfigSchemaResponse {
    pub id: String,
    pub version: String,
    pub config_version: String,
    pub config_schema_digest: String,
    pub pointer: String,
    pub fragment: bool,
    pub schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub referenced_schemas: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditDraftRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub lab: LabSpec,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftMutationResult {
    pub draft_id: String,
    pub version: u64,
    pub component_count: u32,
    pub link_count: u32,
    pub valid: bool,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MutateComponentRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub component: ComponentSpec,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveComponentRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub component_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MutateLinkRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub link: LinkSpec,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneDraftRequest {
    pub source_draft_id: String,
    pub target_draft_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiffDraftRequest {
    pub from_draft_id: String,
    pub to_draft_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishDraftRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub idempotency_key: String,
    /// Explicitly embed the complete published lab and resolved lock.
    #[serde(default)]
    pub include_revision: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishDraftResponse {
    pub workspace_id: String,
    pub digest: String,
    pub lock_digest: String,
    pub component_count: u32,
    pub revision_included: bool,
    /// Schema-opaque bulk lab document, present only after explicit opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lab: Option<serde_json::Value>,
    /// Schema-opaque bulk resolved lock, present only after explicit opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaterializeLabRequest {
    pub instance_id: String,
    pub revision_digest: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceRequest {
    pub instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabStatusSummary {
    pub instance_id: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub phase: InstancePhase,
    pub instance_namespace: String,
    pub ready_components: u32,
    pub total_components: u32,
    pub inventory_count: u32,
    pub inventory_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<CoreTeardownReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabComponentStatusListRequest {
    pub instance_id: String,
    #[serde(default = "default_status_list_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    /// Opaque continuation token returned by a prior component-status page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabComponentStatusListResponse {
    pub instance_id: String,
    pub revision_digest: String,
    pub components: Vec<ComponentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabInventoryListRequest {
    pub instance_id: String,
    #[serde(default = "default_status_list_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    /// Opaque continuation token returned by a prior inventory page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabInventoryListResponse {
    pub instance_id: String,
    pub inventory_digest: String,
    pub inventory: Vec<InventoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabWaitRequest {
    pub instance_id: String,
    /// Phase that ends the wait successfully. `ready` and `closed` are the
    /// normal materialization and teardown targets.
    pub target_phase: InstancePhase,
    /// Server-side wait bound in 1..=120 seconds.
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabWaitResult {
    pub instance_id: String,
    pub phase: InstancePhase,
    pub target_phase: InstancePhase,
    pub reached: bool,
    pub timed_out: bool,
    pub ready_components: u32,
    pub total_components: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<CoreTeardownReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationWaitRequest {
    pub operation_id: String,
    /// Server-side wait bound in 1..=120 seconds.
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationWaitResult {
    pub operation_id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub phase: OperationPhase,
    pub terminal: bool,
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<OperationArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeControlRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub component: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentLogsRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub component: String,
    /// Lines to read from the end of the component's current container log,
    /// between 1 and 2000. The artifact is additionally byte-bounded.
    pub tail_lines: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationConformanceRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub mint: String,
    pub identity_provider: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationProtectedSpendRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub mint: String,
    pub identity_provider: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationReplayRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub mint: String,
    pub identity_provider: String,
    pub source_operation_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentExecRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub component: String,
    /// Lab component whose native service endpoint should be exposed to the
    /// command. When omitted, the execution component is also the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_component: Option<String>,
    /// An unrestricted non-interactive shell program run by `/bin/sh` inside
    /// the component's pinned image. Native command failures are returned as
    /// an exit code in the terminal artifact.
    pub script: String,
    pub timeout_seconds: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BootstrapLiquidityRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub chain: String,
    pub mint_lightning: String,
    pub payer_lightning: String,
    pub funding_sat: u64,
    pub channel_sat: u64,
    pub push_sat: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PeerConnectRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub from_lightning: String,
    pub to_lightning: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PeerDisconnectRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub from_lightning: String,
    pub to_lightning: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChannelOpenRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub chain: String,
    pub from_lightning: String,
    pub to_lightning: String,
    pub channel_sat: u64,
    pub push_sat: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChannelCloseRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub chain: String,
    pub from_lightning: String,
    pub to_lightning: String,
    pub channel_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChannelRebalanceRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub lightning: String,
    pub outgoing_channel_id: String,
    pub incoming_channel_id: String,
    pub amount_sat: u64,
    pub max_fee_sat: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkPartitionRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub from_component: String,
    pub to_component: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkDelayRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub from_component: String,
    pub to_component: String,
    pub direction: NetworkFaultDirection,
    pub delay_ms: u32,
    pub jitter_ms: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkLossRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub from_component: String,
    pub to_component: String,
    pub direction: NetworkFaultDirection,
    pub loss_basis_points: u16,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkHealRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub partition_operation_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletInitializeRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletBalanceRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletFundRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub payer_lightning: String,
    pub amount_sat: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletInvoiceRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub amount_sat: u64,
    #[serde(default = "default_quote_timeout_seconds")]
    pub timeout_seconds: u32,
    pub idempotency_key: String,
}

const fn default_quote_timeout_seconds() -> u32 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletPayRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub recipient_wallet: String,
    pub recipient_mint: String,
    pub mint_quote_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteClaimRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub mint_quote_id: String,
    #[serde(default = "default_claim_timeout_seconds")]
    pub timeout_seconds: u32,
    pub idempotency_key: String,
}

const fn default_claim_timeout_seconds() -> u32 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletRoundTripRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub payer_lightning: String,
    pub amount_sat: u64,
    pub tolerance_sat: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConservationOracleRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub expected_sat: u64,
    pub tolerance_sat: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReachabilityOracleRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub from_component: String,
    pub to_component: String,
    /// Logical destination service, such as `http`, `rpc`, or `p2p`.
    pub service: String,
    #[serde(default = "default_probe_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_probe_attempts")]
    pub attempts: u32,
    pub idempotency_key: String,
}

const fn default_probe_timeout_seconds() -> u32 {
    2
}

const fn default_probe_attempts() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelOperationRequest {
    pub operation_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateExperimentRequest {
    pub experiment_id: String,
    pub instance_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRequest {
    pub experiment_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseExperimentRequest {
    pub experiment_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquireLeaseRequest {
    pub experiment_id: String,
    pub lease_id: String,
    pub duration_seconds: u32,
    pub max_actions: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLeaseRequest {
    pub lease_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionListRequest {
    pub experiment_id: String,
    #[serde(default)]
    pub after_sequence: u64,
    #[serde(default = "default_action_list_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionListResponse {
    pub actions: Vec<ActionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_sequence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionArtifactSummary {
    pub media_type: String,
    pub digest: String,
    pub byte_length: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionSummary {
    pub id: String,
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub capability: Capability,
    pub request_digest: String,
    pub phase: OperationPhase,
    pub accepted_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ActionArtifactSummary>,
}

impl From<&LabOperation> for ActionSummary {
    fn from(operation: &LabOperation) -> Self {
        Self {
            id: operation.id.clone(),
            instance_id: operation.instance_id.clone(),
            experiment_id: operation.experiment_id.clone(),
            lease_id: operation.lease_id.clone(),
            sequence: operation.sequence,
            kind: operation.kind,
            capability: operation.capability,
            request_digest: operation.request_digest.clone(),
            phase: operation.phase,
            accepted_at_unix: operation.accepted_at_unix,
            started_at_unix: operation.started_at_unix,
            completed_at_unix: operation.completed_at_unix,
            artifact: operation
                .artifact
                .as_ref()
                .map(|artifact| ActionArtifactSummary {
                    media_type: artifact.media_type.clone(),
                    digest: artifact.digest.clone(),
                    byte_length: artifact.byte_length,
                }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactExportRequest {
    pub experiment_id: String,
    /// Include full artifact bodies for conservation and reachability oracles.
    #[serde(default = "default_true")]
    pub include_oracle_artifacts: bool,
    /// Additional operation IDs whose already-sanitized artifacts should be included.
    #[serde(default)]
    pub artifact_operation_ids: Vec<String>,
    /// Explicitly embed the complete bulk evidence document. The default returns only its manifest.
    #[serde(default)]
    pub include_content: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceExportResponse {
    pub media_type: String,
    pub digest: String,
    pub byte_length: u32,
    pub workspace_id: String,
    pub experiment_id: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub journal_count: u32,
    pub artifact_count: u32,
    /// Stable MCP resource URI for reading the complete deterministic bundle.
    pub resource_uri: String,
    pub content_included: bool,
    /// Deliberately schema-opaque bulk content, present only after explicit opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSection {
    Revision,
    Lock,
    Journal,
    Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSectionReadRequest {
    pub experiment_id: String,
    /// Must match the selection used for the evidence manifest.
    #[serde(default = "default_true")]
    pub include_oracle_artifacts: bool,
    /// Must match the selection used for the evidence manifest.
    #[serde(default)]
    pub artifact_operation_ids: Vec<String>,
    pub section: EvidenceSection,
    /// RFC 6901 JSON Pointer within revision, lock, or one artifact. Empty reads that whole section.
    #[serde(default)]
    pub pointer: String,
    /// Required for artifact reads and ignored for other sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Journal sequence boundary; ignored for other sections.
    #[serde(default)]
    pub after_sequence: u64,
    /// Journal page size; ignored for other sections.
    #[serde(default = "default_evidence_section_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSectionReadResponse {
    pub evidence_digest: String,
    pub section: EvidenceSection,
    pub data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_sequence: Option<u64>,
}

const fn default_true() -> bool {
    true
}

const MAX_EVIDENCE_ACTIONS: u32 = 100;
const MAX_EXPLICIT_EVIDENCE_ARTIFACTS: usize = 16;
const MAX_EVIDENCE_ARTIFACTS: usize = 32;
const MAX_EVIDENCE_BUNDLE_BYTES: usize = 512 * 1024;

const fn default_evidence_section_limit() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteRequest {
    pub instance_id: String,
    pub wallet: String,
    pub mint: String,
    pub direction: WalletQuoteDirection,
    pub quote_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteStatusResponse {
    pub last_observation: WalletQuoteObservation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteListRequest {
    pub experiment_id: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_quote_list_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteListResponse {
    pub last_observations: Vec<WalletQuoteObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

const fn default_quote_list_limit() -> u32 {
    50
}

const fn default_action_list_limit() -> u32 {
    50
}

const fn default_catalog_list_limit() -> u32 {
    20
}

const fn default_status_list_limit() -> u32 {
    20
}

const MAX_CATALOG_LIST_LIMIT: u32 = 50;
const MAX_AGENT_RESPONSE_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofstormToolset {
    All,
    Design,
    Runtime,
    Evidence,
}

impl std::str::FromStr for ProofstormToolset {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "design" => Ok(Self::Design),
            "runtime" => Ok(Self::Runtime),
            "evidence" => Ok(Self::Evidence),
            _ => Err(format!(
                "invalid PROOFSTORM_TOOLSET {value:?}; expected all, design, runtime, or evidence"
            )),
        }
    }
}

impl ProofstormToolset {
    fn includes(self, tool: &str) -> bool {
        match self {
            Self::All => true,
            Self::Design => matches!(
                tool,
                "proofstorm_workspace_read"
                    | "proofstorm_catalog_list"
                    | "proofstorm_catalog_entry_read"
                    | "proofstorm_catalog_config_schema_read"
                    | "proofstorm_network_capabilities"
                    | "proofstorm_lab_create"
                    | "proofstorm_lab_read"
                    | "proofstorm_lab_edit"
                    | "proofstorm_component_add"
                    | "proofstorm_component_update"
                    | "proofstorm_component_remove"
                    | "proofstorm_link_add"
                    | "proofstorm_link_remove"
                    | "proofstorm_lab_clone"
                    | "proofstorm_lab_validate"
                    | "proofstorm_lab_diff"
                    | "proofstorm_lab_publish"
            ),
            Self::Runtime => !matches!(
                tool,
                "proofstorm_lab_create"
                    | "proofstorm_lab_edit"
                    | "proofstorm_component_add"
                    | "proofstorm_component_update"
                    | "proofstorm_component_remove"
                    | "proofstorm_link_add"
                    | "proofstorm_link_remove"
                    | "proofstorm_lab_clone"
                    | "proofstorm_lab_validate"
                    | "proofstorm_lab_diff"
                    | "proofstorm_lab_publish"
                    | "proofstorm_artifact_export"
                    | "proofstorm_evidence_section_read"
            ),
            Self::Evidence => matches!(
                tool,
                "proofstorm_workspace_read"
                    | "proofstorm_catalog_list"
                    | "proofstorm_catalog_entry_read"
                    | "proofstorm_catalog_config_schema_read"
                    | "proofstorm_lab_read"
                    | "proofstorm_lab_status"
                    | "proofstorm_lab_component_status_list"
                    | "proofstorm_lab_inventory_list"
                    | "proofstorm_lab_wait"
                    | "proofstorm_experiment_read"
                    | "proofstorm_lease_read"
                    | "proofstorm_operation_status"
                    | "proofstorm_operation_wait"
                    | "proofstorm_action_list"
                    | "proofstorm_artifact_export"
                    | "proofstorm_evidence_section_read"
                    | "proofstorm_action_status"
                    | "proofstorm_wallet_quote_status"
                    | "proofstorm_wallet_quote_list"
            ),
        }
    }
}

#[derive(Clone)]
pub struct ProofstormMcp {
    store: Store,
    workspace: String,
    principal: String,
    kubernetes: Option<KubernetesRuntime>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for ProofstormMcp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProofstormMcp")
            .field("workspace", &self.workspace)
            .field("principal", &self.principal)
            .finish_non_exhaustive()
    }
}

impl Default for ProofstormMcp {
    fn default() -> Self {
        let store = Store::memory().expect("create legacy in-memory store");
        let workspace = "local";
        let principal = "local";
        store
            .put_workspace(&Workspace {
                id: workspace.into(),
                name: workspace.into(),
            })
            .expect("seed legacy workspace");
        store
            .put_principal(principal)
            .expect("seed legacy principal");
        for capability in [Capability::CatalogRead, Capability::LabValidate] {
            store
                .grant(workspace, principal, capability)
                .expect("seed legacy grant");
        }
        Self::new(store, workspace, principal).expect("create legacy MCP session")
    }
}

impl ProofstormMcp {
    /// Create a session-scoped MCP gateway and filter its router from durable grants.
    ///
    /// # Errors
    ///
    /// Returns a store error if the principal's capability set cannot be read.
    pub fn new(
        store: Store,
        workspace: impl Into<String>,
        principal: impl Into<String>,
    ) -> Result<Self, StoreError> {
        let workspace = workspace.into();
        let principal = principal.into();
        let capabilities = store.capabilities(&workspace, &principal)?;
        let mut tool_router = Self::tool_router();
        for (tool, required) in tool_capabilities() {
            if !required
                .iter()
                .all(|capability| capabilities.contains(capability))
            {
                tool_router.disable_route(tool);
            }
        }
        Ok(Self {
            store,
            workspace,
            principal,
            kubernetes: None,
            tool_router,
        })
    }

    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }

    #[must_use]
    pub fn with_kubernetes(mut self, client: Client, control_namespace: impl Into<String>) -> Self {
        self.kubernetes = Some(KubernetesRuntime {
            client,
            control_namespace: control_namespace.into(),
        });
        self
    }

    #[must_use]
    pub fn with_toolset(mut self, toolset: ProofstormToolset) -> Self {
        for (tool, _) in tool_capabilities() {
            if !toolset.includes(tool) {
                self.tool_router.disable_route(tool);
            }
        }
        self
    }

    fn authorize(&self, capability: Capability) -> Result<(), ErrorData> {
        self.store
            .authorize(&self.workspace, &self.principal, capability)
            .map_err(store_error)
    }

    fn authorize_all(&self, capabilities: &[Capability]) -> Result<(), ErrorData> {
        for capability in capabilities {
            self.authorize(*capability)?;
        }
        Ok(())
    }

    async fn full_lab_status(&self, instance_id: &str) -> Result<LabInstanceStatus, ErrorData> {
        self.authorize(Capability::LabStatus)?;
        let instance = self
            .store
            .instance(&self.workspace, &self.principal, instance_id)
            .map_err(store_error)?;
        self.runtime()?.status(instance).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "evidence admission, selection, and final size checks stay visibly atomic"
    )]
    fn build_evidence_bundle(
        &self,
        request: &ArtifactExportRequest,
    ) -> Result<EvidenceBundle, ErrorData> {
        self.authorize_all(&[Capability::ExperimentRead, Capability::ArtifactRead])?;
        if request.artifact_operation_ids.len() > MAX_EXPLICIT_EVIDENCE_ARTIFACTS {
            return Err(coded_invalid_request(
                "evidence_artifact_limit",
                "at most 16 explicit artifact operation IDs may be requested",
            ));
        }
        let explicit = request
            .artifact_operation_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if explicit.len() != request.artifact_operation_ids.len() {
            return Err(coded_invalid_request(
                "evidence_artifact_duplicate",
                "artifact operation IDs must be unique",
            ));
        }
        let experiment = self
            .store
            .experiment(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        if experiment.phase != ExperimentPhase::Closed {
            return Err(coded_invalid_request(
                "evidence_experiment_active",
                "evidence export requires a closed experiment",
            ));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &experiment.instance_id,
                Capability::ArtifactRead,
            )
            .map_err(store_error)?;
        let actions = self
            .store
            .actions(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                0,
                MAX_EVIDENCE_ACTIONS,
            )
            .map_err(store_error)?;
        if actions.len() == MAX_EVIDENCE_ACTIONS as usize {
            let after = actions.last().map_or(0, |action| action.sequence);
            if !self
                .store
                .actions(
                    &self.workspace,
                    &self.principal,
                    &request.experiment_id,
                    after,
                    1,
                )
                .map_err(store_error)?
                .is_empty()
            {
                return Err(coded_invalid_request(
                    "evidence_action_limit",
                    "experiment has more than 100 actions and cannot be exported as one bundle",
                ));
            }
        }
        if actions.iter().any(|action| {
            matches!(
                action.phase,
                OperationPhase::Pending | OperationPhase::Running
            )
        }) {
            return Err(coded_invalid_request(
                "evidence_journal_incomplete",
                "all experiment actions must be terminal before evidence export",
            ));
        }
        let known_ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(unknown) = explicit.iter().find(|id| !known_ids.contains(id.as_str())) {
            return Err(coded_invalid_request(
                "evidence_artifact_unknown",
                format!("operation {unknown:?} is not in the experiment journal"),
            ));
        }
        let selected = actions
            .iter()
            .filter(|action| {
                explicit.contains(&action.id)
                    || request.include_oracle_artifacts
                        && matches!(
                            action.kind,
                            OperationKind::ConservationOracle | OperationKind::ReachabilityOracle
                        )
            })
            .collect::<Vec<_>>();
        if selected.len() > MAX_EVIDENCE_ARTIFACTS {
            return Err(coded_invalid_request(
                "evidence_artifact_limit",
                "at most 32 artifact bodies may be included in one evidence bundle",
            ));
        }
        let mut artifacts = Vec::with_capacity(selected.len());
        for action in selected {
            let artifact = action.artifact.clone().ok_or_else(|| {
                coded_invalid_request(
                    "evidence_artifact_missing",
                    format!("operation {:?} has no terminal artifact", action.id),
                )
            })?;
            artifacts.push(EvidenceArtifact {
                operation_id: action.id.clone(),
                sequence: action.sequence,
                kind: action.kind,
                artifact,
            });
        }
        let content = EvidenceBundleContent {
            api_version: EVIDENCE_API_VERSION.to_owned(),
            workspace_id: self.workspace.clone(),
            experiment,
            instance: EvidenceInstance {
                id: instance.id,
                revision_digest: instance.revision_digest,
                lock_digest: instance.lock_digest,
            },
            revision,
            journal: actions.iter().map(EvidenceAction::from).collect(),
            artifacts,
        };
        let bundle = EvidenceBundle::from_content(content);
        if bundle.byte_length as usize > MAX_EVIDENCE_BUNDLE_BYTES {
            return Err(coded_invalid_request(
                "evidence_bundle_too_large",
                "evidence bundle content exceeds 512 KiB",
            ));
        }
        Ok(bundle)
    }
}

#[derive(Clone)]
struct KubernetesRuntime {
    client: Client,
    control_namespace: String,
}

#[tool_router(router = tool_router)]
impl ProofstormMcp {
    #[tool(description = "Read the selected Proofstorm workspace")]
    fn proofstorm_workspace_read(&self) -> Result<Json<Workspace>, ErrorData> {
        self.authorize(Capability::LabRead)?;
        self.store
            .workspace(&self.workspace, &self.principal)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "List a bounded filtered page of compact installed component identities. Use catalog_entry_read and catalog_config_schema_read for exact details"
    )]
    fn proofstorm_catalog_list(
        &self,
        Parameters(request): Parameters<CatalogListRequest>,
    ) -> Result<Json<CatalogListResponse>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        catalog_page(&request).map(Json)
    }

    #[tool(
        description = "Read exact installed metadata, compatibility, immutable image, features, and support for one component version without its configuration schema"
    )]
    fn proofstorm_catalog_entry_read(
        &self,
        Parameters(request): Parameters<CatalogEntryRequest>,
    ) -> Result<Json<CatalogEntryDetail>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        let catalog = default_catalog();
        let entry = exact_catalog_entry(&catalog.entries, &request.id, &request.version)?;
        let preferred = catalog.implementations.iter().any(|support| {
            support.implementation == entry.id && support.preferred_version == entry.version
        });
        bounded_agent_response(CatalogEntryDetail::from_entry(entry, preferred)).map(Json)
    }

    #[tool(
        description = "Read the complete configuration JSON Schema or one RFC 6901 fragment for an exact installed component version"
    )]
    fn proofstorm_catalog_config_schema_read(
        &self,
        Parameters(request): Parameters<CatalogConfigSchemaRequest>,
    ) -> Result<Json<CatalogConfigSchemaResponse>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        catalog_config_schema(request)
            .and_then(bounded_agent_response)
            .map(Json)
    }

    #[tool(
        description = "Discover the installed network-fault backend, features, directions, and bounds"
    )]
    fn proofstorm_network_capabilities(&self) -> Result<Json<NetworkFaultBackend>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        Ok(Json(network_policy_fault_backend()))
    }

    #[tool(description = "Create a versioned lab draft and return a compact mutation receipt")]
    fn proofstorm_lab_create(
        &self,
        Parameters(request): Parameters<CreateDraftRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::LabCreate)?;
        self.store
            .create_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                &request.lab,
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec!["/".into()])))
            .map_err(store_error)
    }

    #[tool(description = "Read a lab draft from the selected workspace")]
    fn proofstorm_lab_read(
        &self,
        Parameters(request): Parameters<ReadDraftRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::LabRead)?;
        self.store
            .read_draft(&self.workspace, &self.principal, &request.draft_id)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Replace a lab draft using optimistic version and idempotency checks, returning a compact mutation receipt"
    )]
    fn proofstorm_lab_edit(
        &self,
        Parameters(request): Parameters<EditDraftRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::LabEdit)?;
        self.store
            .edit_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &request.lab,
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec!["/".into()])))
            .map_err(store_error)
    }

    #[tool(
        description = "Add an installed, versioned component and return a compact draft mutation receipt"
    )]
    fn proofstorm_component_add(
        &self,
        Parameters(request): Parameters<MutateComponentRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let changed_path = format!("/components/{}", request.component.id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::AddComponent {
                    component: request.component,
                },
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec![changed_path])))
            .map_err(store_error)
    }

    #[tool(
        description = "Update an existing logical component and return a compact draft mutation receipt"
    )]
    fn proofstorm_component_update(
        &self,
        Parameters(request): Parameters<MutateComponentRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let changed_path = format!("/components/{}", request.component.id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::UpdateComponent {
                    component: request.component,
                },
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec![changed_path])))
            .map_err(store_error)
    }

    #[tool(
        description = "Remove an unlinked component and return a compact draft mutation receipt"
    )]
    fn proofstorm_component_remove(
        &self,
        Parameters(request): Parameters<RemoveComponentRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let changed_path = format!("/components/{}", request.component_id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::RemoveComponent {
                    component_id: request.component_id,
                },
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec![changed_path])))
            .map_err(store_error)
    }

    #[tool(
        description = "Add a uniquely named typed binding between two draft components; the link id is the stable binding identity"
    )]
    fn proofstorm_link_add(
        &self,
        Parameters(request): Parameters<MutateLinkRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let changed_path = format!("/links/{}", request.link.id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::AddLink { link: request.link },
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec![changed_path])))
            .map_err(store_error)
    }

    #[tool(description = "Remove one exact named typed binding from a lab draft")]
    fn proofstorm_link_remove(
        &self,
        Parameters(request): Parameters<MutateLinkRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let changed_path = format!("/links/{}", request.link.id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::RemoveLink { link: request.link },
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec![changed_path])))
            .map_err(store_error)
    }

    #[tool(description = "Clone a lab draft and return a compact mutation receipt")]
    fn proofstorm_lab_clone(
        &self,
        Parameters(request): Parameters<CloneDraftRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::LabClone)?;
        self.store
            .clone_draft(
                &self.workspace,
                &self.principal,
                &request.source_draft_id,
                &request.target_draft_id,
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec!["/".into()])))
            .map_err(store_error)
    }

    #[tool(description = "Validate a complete Proofstorm v1alpha1 lab without changing state")]
    fn proofstorm_lab_validate(
        &self,
        Parameters(request): Parameters<ValidateLabRequest>,
    ) -> Result<Json<ValidationReport>, ErrorData> {
        self.authorize(Capability::LabValidate)?;
        Ok(Json(validate_lab(&request.lab)))
    }

    #[tool(description = "Compare two lab drafts in the selected workspace")]
    fn proofstorm_lab_diff(
        &self,
        Parameters(request): Parameters<DiffDraftRequest>,
    ) -> Result<Json<DraftDiff>, ErrorData> {
        self.authorize(Capability::LabRead)?;
        self.store
            .diff_drafts(
                &self.workspace,
                &self.principal,
                &request.from_draft_id,
                &request.to_draft_id,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Publish an immutable lab revision and return a compact digest receipt. Set include_revision only for an explicit bulk read of the lab and resolved lock"
    )]
    fn proofstorm_lab_publish(
        &self,
        Parameters(request): Parameters<PublishDraftRequest>,
    ) -> Result<Json<PublishDraftResponse>, ErrorData> {
        self.authorize(Capability::LabPublish)?;
        self.store
            .publish(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &request.idempotency_key,
            )
            .map(|revision| Json(publish_draft_response(revision, request.include_revision)))
            .map_err(store_error)
    }

    #[tool(
        description = "Materialize an immutable published lab revision in the configured Kubernetes runtime"
    )]
    async fn proofstorm_lab_materialize(
        &self,
        Parameters(request): Parameters<MaterializeLabRequest>,
    ) -> Result<Json<LabInstanceStatus>, ErrorData> {
        self.authorize(Capability::LabMaterialize)?;
        let instance = self
            .store
            .materialize(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.revision_digest,
                &request.idempotency_key,
            )
            .map_err(store_error)?;
        let revision = self
            .store
            .revision_for_materialize(&self.workspace, &self.principal, &instance.revision_digest)
            .map_err(store_error)?;
        self.runtime()?
            .materialize(instance, revision)
            .await
            .map(Json)
    }

    #[tool(
        description = "Read a compact lab readiness receipt with component and inventory counts. Use the component-status and inventory list tools for paged detail"
    )]
    async fn proofstorm_lab_status(
        &self,
        Parameters(request): Parameters<InstanceRequest>,
    ) -> Result<Json<LabStatusSummary>, ErrorData> {
        self.full_lab_status(&request.instance_id)
            .await
            .map(compact_lab_status)
            .map(Json)
    }

    #[tool(
        description = "List sanitized component readiness for a lab instance in bounded cursor pages"
    )]
    async fn proofstorm_lab_component_status_list(
        &self,
        Parameters(request): Parameters<LabComponentStatusListRequest>,
    ) -> Result<Json<LabComponentStatusListResponse>, ErrorData> {
        validate_status_list_limit(request.limit)?;
        let status = self.full_lab_status(&request.instance_id).await?;
        let mut components = status.components;
        components.sort_by(|left, right| left.id.cmp(&right.id));
        let snapshot_digest = digest_json(&components);
        let start = status_page_start(request.cursor.as_deref(), &components, |component| {
            status_cursor(
                "component",
                &request.instance_id,
                &snapshot_digest,
                &component.id,
            )
        })?;
        let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
        let mut end = (start + limit).min(components.len());
        loop {
            let response = LabComponentStatusListResponse {
                instance_id: request.instance_id.clone(),
                revision_digest: status.instance.revision_digest.clone(),
                components: components[start..end].to_vec(),
                next_cursor: (end < components.len() && end > start).then(|| {
                    status_cursor(
                        "component",
                        &request.instance_id,
                        &snapshot_digest,
                        &components[end - 1].id,
                    )
                }),
            };
            if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
                return Ok(Json(response));
            }
            if end <= start + 1 {
                return Err(coded_invalid_request(
                    "status_response_too_large",
                    "one component status exceeds the agent response budget",
                ));
            }
            end -= 1;
        }
    }

    #[tool(
        description = "List sanitized Kubernetes inventory for a lab instance in bounded cursor pages"
    )]
    async fn proofstorm_lab_inventory_list(
        &self,
        Parameters(request): Parameters<LabInventoryListRequest>,
    ) -> Result<Json<LabInventoryListResponse>, ErrorData> {
        validate_status_list_limit(request.limit)?;
        let status = self.full_lab_status(&request.instance_id).await?;
        let mut inventory = status.inventory;
        inventory.sort_by_key(inventory_key);
        let inventory_digest = digest_json(&inventory);
        let start = status_page_start(request.cursor.as_deref(), &inventory, |entry| {
            status_cursor(
                "inventory",
                &request.instance_id,
                &inventory_digest,
                &inventory_key(entry),
            )
        })?;
        let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
        let mut end = (start + limit).min(inventory.len());
        loop {
            let response = LabInventoryListResponse {
                instance_id: request.instance_id.clone(),
                inventory_digest: inventory_digest.clone(),
                inventory: inventory[start..end].to_vec(),
                next_cursor: (end < inventory.len() && end > start).then(|| {
                    status_cursor(
                        "inventory",
                        &request.instance_id,
                        &inventory_digest,
                        &inventory_key(&inventory[end - 1]),
                    )
                }),
            };
            if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
                return Ok(Json(response));
            }
            if end <= start + 1 {
                return Err(coded_invalid_request(
                    "status_response_too_large",
                    "one inventory entry exceeds the agent response budget",
                ));
            }
            end -= 1;
        }
    }

    #[tool(
        description = "Wait with bounded server-side exponential backoff for a lab to reach a target phase, returning only compact phase, readiness counts, message, and teardown receipt. timeout_seconds must be 1..=120"
    )]
    async fn proofstorm_lab_wait(
        &self,
        Parameters(request): Parameters<LabWaitRequest>,
    ) -> Result<Json<LabWaitResult>, ErrorData> {
        validate_wait_timeout(request.timeout_seconds)?;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(u64::from(request.timeout_seconds));
        let mut backoff = std::time::Duration::from_millis(250);
        loop {
            let status = self.full_lab_status(&request.instance_id).await?;
            let reached = status.phase == request.target_phase;
            if reached || lab_wait_terminal(status.phase) {
                return Ok(Json(compact_lab_wait(
                    status,
                    request.target_phase,
                    reached,
                    false,
                )));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Json(compact_lab_wait(
                    status,
                    request.target_phase,
                    false,
                    true,
                )));
            }
            tokio::time::sleep(backoff.min(deadline - now)).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(2));
        }
    }

    #[tool(description = "Close a lab instance and begin verified Kubernetes teardown")]
    async fn proofstorm_lab_close(
        &self,
        Parameters(request): Parameters<InstanceRequest>,
    ) -> Result<Json<LabInstanceStatus>, ErrorData> {
        self.authorize(Capability::LabClose)?;
        let instance = self
            .store
            .instance_for_close(&self.workspace, &self.principal, &request.instance_id)
            .map_err(store_error)?;
        self.finalize_active_operations(&instance.id).await?;
        self.runtime()?.close(instance).await.map(Json)
    }

    /// Closing a lab deletes every runtime action resource, so the journal
    /// must reach a terminal phase for each non-terminal operation first.
    /// Cancellation is requested best-effort; the ledger outcome is recorded
    /// regardless, because the lab will not produce one afterwards.
    async fn finalize_active_operations(&self, instance_id: &str) -> Result<(), ErrorData> {
        let active = self
            .store
            .active_operations(&self.workspace, instance_id)
            .map_err(store_error)?;
        for operation in active {
            let token = proofstorm_core::digest_json(&(
                &self.workspace,
                &self.principal,
                &operation.id,
                "lab_close",
            ));
            let _ = self
                .runtime()?
                .request_action_cancellation(&operation, &token)
                .await;
            self.store
                .record_operation_result(
                    &self.workspace,
                    &operation.id,
                    OperationPhase::Cancelled,
                    serde_json::json!({
                        "code": "lab_closed",
                        "message": "the lab instance was closed before the operation reached a terminal phase",
                    }),
                )
                .map_err(store_error)?;
        }
        Ok(())
    }

    #[tool(description = "Create a durable experiment bound to one lab instance")]
    fn proofstorm_experiment_create(
        &self,
        Parameters(request): Parameters<CreateExperimentRequest>,
    ) -> Result<Json<Experiment>, ErrorData> {
        self.authorize(Capability::ExperimentCreate)?;
        self.store
            .create_experiment(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                &request.instance_id,
                &request.idempotency_key,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Read a durable experiment in the selected workspace")]
    fn proofstorm_experiment_read(
        &self,
        Parameters(request): Parameters<ExperimentRequest>,
    ) -> Result<Json<Experiment>, ErrorData> {
        self.authorize(Capability::ExperimentRead)?;
        self.store
            .experiment(&self.workspace, &self.principal, &request.experiment_id)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Close an unleased experiment")]
    fn proofstorm_experiment_close(
        &self,
        Parameters(request): Parameters<CloseExperimentRequest>,
    ) -> Result<Json<Experiment>, ErrorData> {
        self.authorize(Capability::ExperimentClose)?;
        self.store
            .close_experiment(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                &request.idempotency_key,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Acquire an exclusive expiring action-budget lease on a ready lab instance"
    )]
    async fn proofstorm_lease_acquire(
        &self,
        Parameters(request): Parameters<AcquireLeaseRequest>,
    ) -> Result<Json<ExperimentLease>, ErrorData> {
        self.authorize(Capability::LeaseAcquire)?;
        let experiment = self
            .store
            .experiment_for_lease(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        let (instance, _) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &experiment.instance_id,
                Capability::LeaseAcquire,
            )
            .map_err(store_error)?;
        let status = self.runtime()?.status(instance).await?;
        if status.phase != InstancePhase::Ready {
            return Err(coded_invalid_request(
                "instance_not_ready",
                format!(
                    "lab instance {:?} is not ready for a lease",
                    experiment.instance_id
                ),
            ));
        }
        self.store
            .acquire_lease(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                &request.lease_id,
                request.duration_seconds,
                request.max_actions,
                &request.idempotency_key,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Read an experiment lease and refresh its expiry state")]
    fn proofstorm_lease_read(
        &self,
        Parameters(request): Parameters<LeaseRequest>,
    ) -> Result<Json<ExperimentLease>, ErrorData> {
        self.authorize(Capability::ExperimentRead)?;
        self.store
            .lease(&self.workspace, &self.principal, &request.lease_id)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Release an experiment lease owned by the current principal")]
    fn proofstorm_lease_release(
        &self,
        Parameters(request): Parameters<ReleaseLeaseRequest>,
    ) -> Result<Json<ExperimentLease>, ErrorData> {
        self.authorize(Capability::LeaseRelease)?;
        self.store
            .release_lease(
                &self.workspace,
                &self.principal,
                &request.lease_id,
                &request.idempotency_key,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Start a stopped logical Bitcoin or Lightning node")]
    async fn proofstorm_node_start(
        &self,
        Parameters(request): Parameters<NodeControlRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.submit_node_control(request, OperationKind::NodeStart)
            .await
    }

    #[tool(description = "Stop a logical Bitcoin or Lightning node without deleting its state")]
    async fn proofstorm_node_stop(
        &self,
        Parameters(request): Parameters<NodeControlRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.submit_node_control(request, OperationKind::NodeStop)
            .await
    }

    #[tool(
        description = "Restart a running logical Bitcoin or Lightning node with sequence fencing"
    )]
    async fn proofstorm_node_restart(
        &self,
        Parameters(request): Parameters<NodeControlRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.submit_node_control(request, OperationKind::NodeRestart)
            .await
    }

    #[tool(
        description = "Read a bounded tail of one lab component's own container log, journaled as an experiment artifact. This reads the component's running container, unlike component_exec which starts a separate pod, and it keeps working while the component is unready, crash-looping, or stopped, which is when a native error is usually only visible in its log. The artifact also reports the pod phase, container readiness, and restart count"
    )]
    async fn proofstorm_component_logs(
        &self,
        Parameters(request): Parameters<ComponentLogsRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ComponentLogs)?;
        if !(1..=2_000).contains(&request.tail_lines) {
            return Err(invalid_operation("tail_lines must be in 1..=2000"));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ComponentLogs,
            )
            .map_err(store_error)?;
        let component = revision
            .lab
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this lab revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ComponentLogs,
            &request,
            &request.idempotency_key,
            Capability::ComponentLogs,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let resource = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ComponentLogs(ComponentLogsAction {
                component: request.component,
                tail_lines: request.tail_lines,
            }),
        );
        self.runtime()?.apply_action(&instance, &resource).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Run the fixed Nutshell and Keycloak OIDC/CAT/BAT baseline using the controller-generated disposable test identity. Credentials and issued bearer material remain inside the bounded Job; the terminal artifact contains only typed conformance observations"
    )]
    async fn proofstorm_authentication_conformance(
        &self,
        Parameters(request): Parameters<AuthenticationConformanceRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::AuthenticationTest)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::AuthenticationTest,
            )
            .map_err(store_error)?;
        validate_authentication_components(&revision, &request.mint, &request.identity_provider)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::AuthenticationConformance,
            &request,
            &request.idempotency_key,
            Capability::AuthenticationTest,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let resource = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::AuthenticationConformance(AuthenticationConformanceAction {
                mint: request.mint,
                identity_provider: request.identity_provider,
            }),
        );
        self.runtime()?.apply_action(&instance, &resource).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Mint valid BATs with the disposable test identity, spend one against a protected mint endpoint, and retain the spent bearer token as an opaque in-lab session. MCP returns only typed conformance observations and the source operation identity"
    )]
    async fn proofstorm_authentication_protected_spend(
        &self,
        Parameters(request): Parameters<AuthenticationProtectedSpendRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::AuthenticationTest)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::AuthenticationTest,
            )
            .map_err(store_error)?;
        validate_authentication_components(&revision, &request.mint, &request.identity_provider)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::AuthenticationProtectedSpend,
            &request,
            &request.idempotency_key,
            Capability::AuthenticationTest,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let resource = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::AuthenticationProtectedSpend(AuthenticationProtectedSpendAction {
                mint: request.mint,
                identity_provider: request.identity_provider,
            }),
        );
        self.runtime()?.apply_action(&instance, &resource).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "After a mint restart, replay a BAT retained by a successful protected-spend operation, require spent-token rejection, then mint and spend a fresh BAT. Test credentials and bearer tokens remain inside fixed Proofstorm jobs"
    )]
    async fn proofstorm_authentication_replay(
        &self,
        Parameters(request): Parameters<AuthenticationReplayRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize_all(&[Capability::AuthenticationTest, Capability::ArtifactRead])?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::AuthenticationTest,
            )
            .map_err(store_error)?;
        validate_authentication_components(&revision, &request.mint, &request.identity_provider)?;
        let source = self
            .store
            .operation(
                &self.workspace,
                &self.principal,
                &request.source_operation_id,
            )
            .map_err(store_error)?;
        let source_valid = source.instance_id == request.instance_id
            && source.experiment_id == request.experiment_id
            && source.lease_id == request.lease_id
            && source.principal_id == self.principal
            && source.kind == OperationKind::AuthenticationProtectedSpend
            && source.phase == OperationPhase::Succeeded
            && source.artifact.as_ref().is_some_and(|artifact| {
                artifact.content["contract"] == "proofstorm/authentication-protected-spend/v1"
                    && artifact.content["conformant"] == true
                    && artifact.content["session_operation_id"] == source.id
                    && artifact.content["mint"] == request.mint
                    && artifact.content["identity_provider"] == request.identity_provider
            });
        if !source_valid {
            return Err(invalid_operation(
                "source operation must be a successful protected spend in the same instance, experiment, lease, principal, mint, and identity provider",
            ));
        }
        let session_secret = format!("{}-auth-session", source.resource_name);
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::AuthenticationReplay,
            &request,
            &request.idempotency_key,
            Capability::AuthenticationTest,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let resource = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::AuthenticationReplay(AuthenticationReplayAction {
                mint: request.mint,
                identity_provider: request.identity_provider,
                session_secret,
                source_operation_id: request.source_operation_id,
            }),
        );
        self.runtime()?.apply_action(&instance, &resource).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Run an unrestricted non-interactive shell program in a fresh short-lived pod built from a lab component's exact pinned image, with that component's local data volumes and native CLI available. It does not run inside the component's running container: localhost is not the component, and the component's own processes and listening sockets are not visible. Reach the component over the network instead, using the supplied variables rather than guessed addresses. target_component optionally selects a distinct lab service and defaults to component; Proofstorm exposes generic PROOFSTORM_TARGET_* metadata plus native endpoint variables such as BITCOIN_RPC_HOST and BITCOIN_RPC_PORT without interpreting the command. Targets resolve to a Service, so while a component is not ready its Service has no endpoints and connections to it are refused immediately rather than timing out; the artifact reports target_ready_endpoints so a refusal is distinguishable from a missing listener. The workload has no Kubernetes token, host access, or cross-lab credentials; combined output and exit code are journaled as a bounded experiment artifact"
    )]
    async fn proofstorm_component_exec(
        &self,
        Parameters(request): Parameters<ComponentExecRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ComponentExec)?;
        if request.script.is_empty() || request.script.len() > 16 * 1024 {
            return Err(invalid_operation(
                "script must contain 1..=16384 UTF-8 bytes",
            ));
        }
        if !(1..=300).contains(&request.timeout_seconds) {
            return Err(invalid_operation("timeout_seconds must be in 1..=300"));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ComponentExec,
            )
            .map_err(store_error)?;
        let component = revision
            .lab
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this lab revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let target_component = request
            .target_component
            .as_deref()
            .unwrap_or(&request.component)
            .to_owned();
        let target = revision
            .lab
            .components
            .iter()
            .find(|component| component.id == target_component)
            .ok_or_else(|| {
                invalid_operation("target_component is not part of this lab revision")
            })?;
        component_image_any(&revision, &target_component, target.kind)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::NativeExec,
            &request,
            &request.idempotency_key,
            Capability::ComponentExec,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::NativeExec(NativeExecAction {
                component: request.component,
                target_component,
                script: request.script,
                timeout_seconds: request.timeout_seconds,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Fund two LND nodes and open a bounded payer-to-mint channel")]
    async fn proofstorm_liquidity_bootstrap(
        &self,
        Parameters(request): Parameters<BootstrapLiquidityRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        const REQUIRED: &[Capability] = &[
            Capability::ChainMine,
            Capability::WalletFund,
            Capability::PeerConnect,
            Capability::ChannelOpen,
        ];
        self.authorize_all(REQUIRED)?;
        validate_bootstrap_bounds(&request)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletFund,
            )
            .map_err(store_error)?;
        let _bitcoin_image = component_image(
            &revision,
            &request.chain,
            ComponentKind::Bitcoin,
            "bitcoin-core",
        )?;
        let mint_lnd_image = component_image(
            &revision,
            &request.mint_lightning,
            ComponentKind::Lightning,
            "lnd",
        )?;
        let payer_lnd_image = component_image(
            &revision,
            &request.payer_lightning,
            ComponentKind::Lightning,
            "lnd",
        )?;
        if mint_lnd_image != payer_lnd_image {
            return Err(invalid_operation(
                "bootstrap LND components must use the same pinned adapter image",
            ));
        }
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::BootstrapLiquidity,
            &request,
            &request.idempotency_key,
            Capability::WalletFund,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::BootstrapLiquidity(BootstrapLiquidityAction {
                chain: request.chain.clone(),
                mint_lightning: request.mint_lightning.clone(),
                payer_lightning: request.payer_lightning.clone(),
                funding_sat: request.funding_sat,
                channel_sat: request.channel_sat,
                push_sat: request.push_sat,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Connect two logical Lightning components through their locked adapters")]
    async fn proofstorm_peer_connect(
        &self,
        Parameters(request): Parameters<PeerConnectRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::PeerConnect)?;
        validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::PeerConnect,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.from_lightning, ComponentKind::Lightning)?;
        component_image_any(&revision, &request.to_lightning, ComponentKind::Lightning)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::PeerConnect,
            &request,
            &request.idempotency_key,
            Capability::PeerConnect,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::PeerConnect(PeerConnectAction {
                from_lightning: request.from_lightning.clone(),
                to_lightning: request.to_lightning.clone(),
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Disconnect two logical Lightning peers through their locked adapters")]
    async fn proofstorm_peer_disconnect(
        &self,
        Parameters(request): Parameters<PeerDisconnectRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::PeerDisconnect)?;
        validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::PeerDisconnect,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.from_lightning, ComponentKind::Lightning)?;
        component_image_any(&revision, &request.to_lightning, ComponentKind::Lightning)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::PeerDisconnect,
            &request,
            &request.idempotency_key,
            Capability::PeerDisconnect,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::PeerDisconnect(PeerDisconnectAction {
                from_lightning: request.from_lightning.clone(),
                to_lightning: request.to_lightning.clone(),
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Open and confirm a bounded channel between logical Lightning components")]
    async fn proofstorm_channel_open(
        &self,
        Parameters(request): Parameters<ChannelOpenRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize_all(&[Capability::ChannelOpen, Capability::ChainMine])?;
        validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
        validate_channel_bounds(request.channel_sat, request.push_sat)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ChannelOpen,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.chain, ComponentKind::Bitcoin)?;
        component_image_any(&revision, &request.from_lightning, ComponentKind::Lightning)?;
        component_image_any(&revision, &request.to_lightning, ComponentKind::Lightning)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ChannelOpen,
            &request,
            &request.idempotency_key,
            Capability::ChannelOpen,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ChannelOpen(ChannelOpenAction {
                chain: request.chain.clone(),
                from_lightning: request.from_lightning.clone(),
                to_lightning: request.to_lightning.clone(),
                channel_sat: request.channel_sat,
                push_sat: request.push_sat,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Cooperatively close and confirm an opaque logical Lightning channel")]
    async fn proofstorm_channel_close(
        &self,
        Parameters(request): Parameters<ChannelCloseRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.submit_channel_close(request, false).await
    }

    #[tool(description = "Force close and confirm an opaque logical Lightning channel")]
    async fn proofstorm_channel_force_close(
        &self,
        Parameters(request): Parameters<ChannelCloseRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.submit_channel_close(request, true).await
    }

    #[tool(
        description = "Move bounded local liquidity between two opaque channels using a circular payment"
    )]
    async fn proofstorm_channel_rebalance(
        &self,
        Parameters(request): Parameters<ChannelRebalanceRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ChannelRebalance)?;
        validate_channel_id(&request.outgoing_channel_id)?;
        validate_channel_id(&request.incoming_channel_id)?;
        validate_rebalance_bounds(&request)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ChannelRebalance,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.lightning, ComponentKind::Lightning)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ChannelRebalance,
            &request,
            &request.idempotency_key,
            Capability::ChannelRebalance,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ChannelRebalance(ChannelRebalanceAction {
                lightning: request.lightning,
                outgoing_channel_id: request.outgoing_channel_id,
                incoming_channel_id: request.incoming_channel_id,
                amount_sat: request.amount_sat,
                max_fee_sat: request.max_fee_sat,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Bidirectionally partition two logical components using a durable bounded network fault"
    )]
    async fn proofstorm_network_partition(
        &self,
        Parameters(request): Parameters<NetworkPartitionRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::NetworkPartition)?;
        if request.from_component == request.to_component {
            return Err(invalid_operation("partition endpoints must be distinct"));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::NetworkPartition,
            )
            .map_err(store_error)?;
        for component in [&request.from_component, &request.to_component] {
            if !revision
                .lab
                .components
                .iter()
                .any(|item| item.id == *component)
            {
                return Err(invalid_operation(&format!(
                    "component {component:?} is not part of this lab revision"
                )));
            }
        }
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::NetworkPartition,
            &request,
            &request.idempotency_key,
            Capability::NetworkPartition,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::NetworkPartition(NetworkPartitionAction {
                from_component: request.from_component,
                to_component: request.to_component,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Apply bounded directional latency between logical components when the installed backend supports shaping"
    )]
    fn proofstorm_network_delay(
        &self,
        Parameters(request): Parameters<NetworkDelayRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::NetworkDelay)?;
        validate_network_pair(&request.from_component, &request.to_component)?;
        validate_network_delay_bounds(&request)?;
        require_network_fault_support(NetworkFaultFeature::Delay, request.direction)?;
        Err(network_fault_contract_violation(NetworkFaultFeature::Delay))
    }

    #[tool(
        description = "Apply bounded directional packet loss between logical components when the installed backend supports shaping"
    )]
    fn proofstorm_network_loss(
        &self,
        Parameters(request): Parameters<NetworkLossRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::NetworkDrop)?;
        validate_network_pair(&request.from_component, &request.to_component)?;
        validate_network_loss_bounds(&request)?;
        require_network_fault_support(NetworkFaultFeature::Loss, request.direction)?;
        Err(network_fault_contract_violation(NetworkFaultFeature::Loss))
    }

    #[tool(description = "Heal the durable network partition created by a prior operation")]
    async fn proofstorm_network_heal(
        &self,
        Parameters(request): Parameters<NetworkHealRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::NetworkHeal)?;
        let (instance, _) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::NetworkHeal,
            )
            .map_err(store_error)?;
        let partition = self
            .store
            .operation(
                &self.workspace,
                &self.principal,
                &request.partition_operation_id,
            )
            .map_err(store_error)?;
        if partition.kind != OperationKind::NetworkPartition
            || partition.instance_id != request.instance_id
            || partition.experiment_id != request.experiment_id
            || partition.phase != OperationPhase::Succeeded
        {
            return Err(invalid_operation(
                "partition operation must be succeeded and belong to the same instance and experiment",
            ));
        }
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::NetworkHeal,
            &request,
            &request.idempotency_key,
            Capability::NetworkHeal,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::NetworkHeal(NetworkHealAction {
                partition_operation_id: request.partition_operation_id,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Initialize a persistent logical wallet through its locked adapter")]
    async fn proofstorm_wallet_initialize(
        &self,
        Parameters(request): Parameters<WalletInitializeRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::WalletCreate)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletCreate,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletInitialize,
            &request,
            &request.idempotency_key,
            Capability::WalletCreate,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletInitialize(WalletInitializeAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Read a sanitized balance from a snapshot of a logical wallet")]
    async fn proofstorm_wallet_balance(
        &self,
        Parameters(request): Parameters<WalletBalanceRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::WalletControl)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletControl,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletBalance,
            &request,
            &request.idempotency_key,
            Capability::WalletControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletBalance(WalletBalanceAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Fund a logical wallet with a bounded quote paid by a named Lightning node"
    )]
    async fn proofstorm_wallet_fund(
        &self,
        Parameters(request): Parameters<WalletFundRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::WalletFund)?;
        validate_wallet_amount(request.amount_sat)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletFund,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        component_image_any(
            &revision,
            &request.payer_lightning,
            ComponentKind::Lightning,
        )?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletFund,
            &request,
            &request.idempotency_key,
            Capability::WalletFund,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletFund(WalletFundAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                payer_lightning: request.payer_lightning.clone(),
                amount_sat: request.amount_sat,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Create a bounded receive quote whose Lightning payment request remains private to the recipient wallet"
    )]
    async fn proofstorm_wallet_invoice(
        &self,
        Parameters(request): Parameters<WalletInvoiceRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::WalletFund)?;
        validate_wallet_amount(request.amount_sat)?;
        if !(30..=600).contains(&request.timeout_seconds) {
            return Err(invalid_operation(
                "timeout_seconds must be between 30 and 600",
            ));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletFund,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletInvoice,
            &request,
            &request.idempotency_key,
            Capability::WalletFund,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletInvoice(WalletInvoiceAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                amount_sat: request.amount_sat,
                timeout_seconds: request.timeout_seconds,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Pay a durable private receive quote from a distinct logical wallet without exposing its Lightning invoice"
    )]
    async fn proofstorm_wallet_pay(
        &self,
        Parameters(request): Parameters<WalletPayRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize_all(&[Capability::WalletControl, Capability::ArtifactRead])?;
        if request.recipient_wallet == request.wallet {
            return Err(invalid_operation(
                "payer and recipient wallets must be distinct",
            ));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletControl,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.recipient_wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        component_image_any(&revision, &request.recipient_mint, ComponentKind::Mint)?;
        let mut request_json = serde_json::to_value(&request).map_err(|error| {
            ErrorData::internal_error(
                error.to_string(),
                Some(serde_json::json!({"code": "serialization_failed"})),
            )
        })?;
        if let Some(object) = request_json.as_object_mut() {
            object.remove("idempotency_key");
        }
        let operation = self
            .store
            .create_wallet_pay_operation(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.experiment_id,
                &request.lease_id,
                &request.operation_id,
                &request_json,
                &request.idempotency_key,
                &request.recipient_wallet,
                &request.recipient_mint,
                &request.mint_quote_id,
                &request.wallet,
                &request.mint,
            )
            .map_err(store_error)?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletPay(WalletPayAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                recipient_wallet: request.recipient_wallet.clone(),
                recipient_mint: request.recipient_mint.clone(),
                mint_quote_id: request.mint_quote_id.clone(),
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Refresh and claim an exact recipient mint quote without attempting payment"
    )]
    async fn proofstorm_wallet_quote_claim(
        &self,
        Parameters(request): Parameters<WalletQuoteClaimRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::WalletControl)?;
        if !(1..=120).contains(&request.timeout_seconds) {
            return Err(invalid_operation(
                "timeout_seconds must be between 1 and 120",
            ));
        }
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletControl,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletQuoteClaim,
            &request,
            &request.idempotency_key,
            Capability::WalletControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletQuoteClaim(WalletQuoteClaimAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                mint_quote_id: request.mint_quote_id.clone(),
                timeout_seconds: request.timeout_seconds,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Mint to a persistent Cashu wallet and perform a bounded self swap")]
    async fn proofstorm_wallet_round_trip(
        &self,
        Parameters(request): Parameters<WalletRoundTripRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        const REQUIRED: &[Capability] = &[
            Capability::WalletCreate,
            Capability::WalletFund,
            Capability::WalletControl,
        ];
        self.authorize_all(REQUIRED)?;
        validate_wallet_bounds(&request)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::WalletControl,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        component_image(
            &revision,
            &request.payer_lightning,
            ComponentKind::Lightning,
            "lnd",
        )?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletRoundTrip,
            &request,
            &request.idempotency_key,
            Capability::WalletControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletRoundTrip(WalletRoundTripAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                payer_lightning: request.payer_lightning.clone(),
                amount_sat: request.amount_sat,
                tolerance_sat: request.tolerance_sat,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Run a bounded read-only-wallet conservation oracle")]
    async fn proofstorm_conservation_oracle(
        &self,
        Parameters(request): Parameters<ConservationOracleRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::OracleRun)?;
        validate_oracle_bounds(&request)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::OracleRun,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ConservationOracle,
            &request,
            &request.idempotency_key,
            Capability::OracleRun,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ConservationOracle(ConservationOracleAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                expected_sat: request.expected_sat,
                tolerance_sat: request.tolerance_sat,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Observe bounded service reachability between two lab components using the source component's actual network-policy identity"
    )]
    async fn proofstorm_reachability_oracle(
        &self,
        Parameters(request): Parameters<ReachabilityOracleRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::OracleRun)?;
        validate_reachability_oracle_bounds(&request)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::OracleRun,
            )
            .map_err(store_error)?;
        if !revision
            .lab
            .components
            .iter()
            .any(|component| component.id == request.from_component)
        {
            return Err(invalid_operation(&format!(
                "component {:?} is not part of this lab revision",
                request.from_component
            )));
        }
        let destination = revision
            .lab
            .components
            .iter()
            .find(|component| component.id == request.to_component)
            .ok_or_else(|| {
                invalid_operation(&format!(
                    "component {:?} is not part of this lab revision",
                    request.to_component
                ))
            })?;
        if !component_ports(destination).contains_key(&request.service) {
            return Err(invalid_operation(&format!(
                "component {:?} does not advertise logical service {:?}",
                request.to_component, request.service
            )));
        }
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ReachabilityOracle,
            &request,
            &request.idempotency_key,
            Capability::OracleRun,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ReachabilityOracle(ReachabilityOracleAction {
                from_component: request.from_component.clone(),
                to_component: request.to_component.clone(),
                service: request.service.clone(),
                timeout_seconds: request.timeout_seconds,
                attempts: request.attempts,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Read an operation and persist its bounded terminal artifact")]
    async fn proofstorm_operation_status(
        &self,
        Parameters(request): Parameters<OperationRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ArtifactRead)?;
        let operation = self
            .store
            .operation(&self.workspace, &self.principal, &request.operation_id)
            .map_err(store_error)?;
        if operation.artifact.is_some() {
            return Ok(Json(operation));
        }
        self.store
            .operation_context(
                &self.workspace,
                &self.principal,
                &operation.instance_id,
                Capability::ArtifactRead,
            )
            .map_err(store_error)?;
        let terminal = self.runtime()?.action_status(&operation).await?;
        let Some((phase, artifact)) = terminal else {
            return Ok(Json(operation));
        };
        let observations = wallet_quote_observations_from_artifact(&artifact).map_err(|_| {
            coded_invalid_request(
                "invalid_wallet_quote_observation",
                "terminal artifact contains an invalid wallet quote observation",
            )
        })?;
        validate_operation_quote_observations(&operation, &observations)?;
        let completed = self
            .store
            .record_operation_result_with_quote_observations(
                &self.workspace,
                &operation.id,
                phase,
                artifact,
                &observations,
            )
            .map_err(store_error)?;
        Ok(Json(completed))
    }

    #[tool(
        description = "Wait with bounded server-side exponential backoff for an operation to become terminal, returning compact identity, phase, and terminal artifact. timeout_seconds must be 1..=120"
    )]
    async fn proofstorm_operation_wait(
        &self,
        Parameters(request): Parameters<OperationWaitRequest>,
    ) -> Result<Json<OperationWaitResult>, ErrorData> {
        validate_wait_timeout(request.timeout_seconds)?;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(u64::from(request.timeout_seconds));
        let mut backoff = std::time::Duration::from_millis(250);
        loop {
            let operation = self
                .proofstorm_operation_status(Parameters(OperationRequest {
                    operation_id: request.operation_id.clone(),
                }))
                .await?
                .0;
            if operation_terminal(operation.phase) {
                return Ok(Json(compact_operation_wait(operation, false)));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Json(compact_operation_wait(operation, true)));
            }
            tokio::time::sleep(backoff.min(deadline - now)).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(2));
        }
    }

    #[tool(description = "Request idempotent cancellation of an owned non-terminal action")]
    async fn proofstorm_action_cancel(
        &self,
        Parameters(request): Parameters<CancelOperationRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ActionCancel)?;
        let operation = self
            .store
            .operation_for_cancel(&self.workspace, &self.principal, &request.operation_id)
            .map_err(store_error)?;
        if matches!(
            operation.phase,
            OperationPhase::Succeeded | OperationPhase::Failed | OperationPhase::Cancelled
        ) {
            return Ok(Json(operation));
        }
        let token = proofstorm_core::digest_json(&(
            &self.workspace,
            &self.principal,
            &request.operation_id,
            &request.idempotency_key,
        ));
        if self
            .runtime()?
            .request_action_cancellation(&operation, &token)
            .await?
        {
            return Ok(Json(operation));
        }
        let finalized = self
            .store
            .record_operation_result(
                &self.workspace,
                &operation.id,
                OperationPhase::Failed,
                missing_action_artifact(&operation),
            )
            .map_err(store_error)?;
        Ok(Json(finalized))
    }

    #[tool(
        description = "Read a bounded page of compact canonical action summaries. Use operation_status for one request or artifact body"
    )]
    fn proofstorm_action_list(
        &self,
        Parameters(request): Parameters<ActionListRequest>,
    ) -> Result<Json<ActionListResponse>, ErrorData> {
        self.authorize(Capability::ExperimentRead)?;
        let actions = self
            .store
            .actions(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                request.after_sequence,
                request.limit,
            )
            .map_err(store_error)?;
        let source_has_more =
            if actions.len() == usize::try_from(request.limit).unwrap_or(usize::MAX) {
                let after = actions
                    .last()
                    .map_or(request.after_sequence, |action| action.sequence);
                !self
                    .store
                    .actions(
                        &self.workspace,
                        &self.principal,
                        &request.experiment_id,
                        after,
                        1,
                    )
                    .map_err(store_error)?
                    .is_empty()
            } else {
                false
            };
        let summaries = actions.iter().map(ActionSummary::from).collect::<Vec<_>>();
        let mut end = summaries.len();
        loop {
            let has_more = source_has_more || end < summaries.len();
            let response = ActionListResponse {
                actions: summaries[..end].to_vec(),
                next_after_sequence: (has_more && end > 0).then(|| summaries[end - 1].sequence),
            };
            if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
                return Ok(Json(response));
            }
            if end == 0 {
                return Err(coded_invalid_request(
                    "action_response_too_large",
                    "action page envelope exceeds the agent response budget",
                ));
            }
            end -= 1;
        }
    }

    #[tool(
        description = "Export a compact deterministic evidence manifest for a closed experiment. Set include_content only for an explicit bulk download of the revision, journal, and selected artifacts"
    )]
    fn proofstorm_artifact_export(
        &self,
        Parameters(request): Parameters<ArtifactExportRequest>,
    ) -> Result<Json<EvidenceExportResponse>, ErrorData> {
        let bundle = self.build_evidence_bundle(&request)?;
        let resource_uri = evidence_resource_uri(&request, &bundle.digest);
        Ok(Json(evidence_export_response(
            bundle,
            resource_uri,
            request.include_content,
        )))
    }

    #[tool(
        description = "Read one bounded semantic section of a closed experiment's deterministic evidence bundle. Use JSON Pointer for large revision, lock, or artifact documents"
    )]
    fn proofstorm_evidence_section_read(
        &self,
        Parameters(request): Parameters<EvidenceSectionReadRequest>,
    ) -> Result<Json<EvidenceSectionReadResponse>, ErrorData> {
        let export_request = ArtifactExportRequest {
            experiment_id: request.experiment_id,
            include_oracle_artifacts: request.include_oracle_artifacts,
            artifact_operation_ids: request.artifact_operation_ids,
            include_content: false,
        };
        let bundle = self.build_evidence_bundle(&export_request)?;
        if matches!(request.section, EvidenceSection::Journal) {
            if !(1..=50).contains(&request.limit) {
                return Err(coded_invalid_request(
                    "evidence_section_limit_invalid",
                    "journal limit must be between 1 and 50",
                ));
            }
            let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
            let candidates = bundle
                .content
                .journal
                .iter()
                .filter(|action| action.sequence > request.after_sequence)
                .take(limit + 1)
                .cloned()
                .collect::<Vec<_>>();
            let source_has_more = candidates.len() > limit;
            let page_len = candidates.len().min(limit);
            let mut end = page_len;
            loop {
                let has_more = source_has_more || end < page_len;
                let response = EvidenceSectionReadResponse {
                    evidence_digest: bundle.digest.clone(),
                    section: request.section,
                    data: evidence_json(&candidates[..end])?,
                    next_after_sequence: (has_more && end > 0)
                        .then(|| candidates[end - 1].sequence),
                };
                if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
                    return Ok(Json(response));
                }
                if end <= 1 {
                    return Err(coded_invalid_request(
                        "evidence_action_too_large",
                        "one evidence journal action exceeds the agent response budget",
                    ));
                }
                end -= 1;
            }
        }
        let data = match request.section {
            EvidenceSection::Revision => evidence_pointer(
                evidence_json(&bundle.content.revision)?,
                &request.pointer,
                "revision",
            )?,
            EvidenceSection::Lock => evidence_pointer(
                evidence_json(&bundle.content.revision.lock)?,
                &request.pointer,
                "lock",
            )?,
            EvidenceSection::Artifact => {
                let operation_id = request.operation_id.as_deref().ok_or_else(|| {
                    coded_invalid_request(
                        "evidence_operation_id_required",
                        "operation_id is required for an artifact section read",
                    )
                })?;
                let artifact = bundle
                    .content
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.operation_id == operation_id)
                    .ok_or_else(|| {
                        coded_invalid_request(
                            "evidence_artifact_not_selected",
                            "operation_id is not present in the selected evidence artifacts",
                        )
                    })?;
                evidence_pointer(evidence_json(artifact)?, &request.pointer, "artifact")?
            }
            EvidenceSection::Journal => unreachable!("journal returned above"),
        };
        bounded_agent_response(EvidenceSectionReadResponse {
            evidence_digest: bundle.digest,
            section: request.section,
            data,
            next_after_sequence: None,
        })
        .map(Json)
    }

    #[tool(
        description = "Read the latest stored observation of an exact adapter-native wallet quote; this is historical data, not live wallet state"
    )]
    async fn proofstorm_wallet_quote_status(
        &self,
        Parameters(request): Parameters<WalletQuoteRequest>,
    ) -> Result<Json<WalletQuoteStatusResponse>, ErrorData> {
        self.authorize(Capability::ArtifactRead)?;
        self.store
            .wallet_quote_observation(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.wallet,
                &request.mint,
                request.direction,
                &request.quote_id,
            )
            .map(|last_observation| Json(WalletQuoteStatusResponse { last_observation }))
            .map_err(store_error)
    }

    #[tool(
        description = "List the latest stored observation per adapter-native wallet quote in an experiment; results are historical, not live wallet state"
    )]
    fn proofstorm_wallet_quote_list(
        &self,
        Parameters(request): Parameters<WalletQuoteListRequest>,
    ) -> Result<Json<WalletQuoteListResponse>, ErrorData> {
        self.authorize(Capability::ExperimentRead)?;
        let (snapshot_sequence, after_sequence) = match request.cursor.as_deref() {
            Some(cursor) => decode_quote_cursor(cursor, &request.experiment_id)?,
            None => (
                self.store
                    .wallet_quote_observation_max_sequence(
                        &self.workspace,
                        &self.principal,
                        &request.experiment_id,
                    )
                    .map_err(store_error)?,
                0,
            ),
        };
        let observations = self
            .store
            .wallet_quote_observations(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                after_sequence,
                snapshot_sequence,
                request.limit,
            )
            .map_err(store_error)?;
        let source_has_more =
            if observations.len() == usize::try_from(request.limit).unwrap_or(usize::MAX) {
                let after = observations
                    .last()
                    .map_or(after_sequence, |item| item.observation_sequence);
                !self
                    .store
                    .wallet_quote_observations(
                        &self.workspace,
                        &self.principal,
                        &request.experiment_id,
                        after,
                        snapshot_sequence,
                        1,
                    )
                    .map_err(store_error)?
                    .is_empty()
            } else {
                false
            };
        let mut end = observations.len();
        loop {
            let has_more = source_has_more || end < observations.len();
            let response = WalletQuoteListResponse {
                last_observations: observations[..end].to_vec(),
                next_cursor: (has_more && end > 0).then(|| {
                    encode_quote_cursor(
                        &request.experiment_id,
                        snapshot_sequence,
                        observations[end - 1].observation_sequence,
                    )
                }),
            };
            if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
                return Ok(Json(response));
            }
            if end == 0 {
                return Err(coded_invalid_request(
                    "wallet_quote_response_too_large",
                    "wallet quote page envelope exceeds the agent response budget",
                ));
            }
            end -= 1;
        }
    }

    #[tool(description = "Read an action and persist its bounded terminal artifact")]
    async fn proofstorm_action_status(
        &self,
        Parameters(request): Parameters<OperationRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.proofstorm_operation_status(Parameters(request)).await
    }
}

impl CatalogEntryDetail {
    fn from_entry(entry: &CatalogEntry, preferred: bool) -> Self {
        Self {
            id: entry.id.clone(),
            kind: entry.kind,
            description: entry.description.clone(),
            adapter_version: entry.adapter_version.clone(),
            protocol_action_adapter_version: entry.protocol_action_adapter_version.clone(),
            version: entry.version.clone(),
            preferred,
            release_channel: entry.release_channel,
            support_lifecycle: entry.support_lifecycle,
            config_version: entry.config_version.clone(),
            config_schema_digest: entry.config_schema_digest.clone(),
            features: entry.features.clone(),
            compatible_dependencies: entry.compatible_dependencies.clone(),
            support_matrix: entry.support_matrix.clone(),
            image: entry.image.clone(),
            source_digest: entry.source_digest.clone(),
            allowed_control: entry.allowed_control.clone(),
        }
    }
}

fn validate_operation_quote_observations(
    operation: &LabOperation,
    observations: &[WalletQuoteObservationInput],
) -> Result<(), ErrorData> {
    let field = |name: &str| {
        operation
            .request
            .get(name)
            .and_then(serde_json::Value::as_str)
    };
    let valid = observations
        .iter()
        .all(|observation| match (operation.kind, observation.role) {
            (OperationKind::WalletInvoice, WalletQuoteObservationRole::InvoiceReceive) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
                    && operation
                        .request
                        .get("amount_sat")
                        .and_then(serde_json::Value::as_u64)
                        == Some(observation.amount_sat)
            }
            (OperationKind::WalletPay, WalletQuoteObservationRole::PaymentMelt) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
            }
            (OperationKind::WalletPay, WalletQuoteObservationRole::PaymentReceive) => {
                field("recipient_wallet") == Some(&observation.wallet_id)
                    && field("recipient_mint") == Some(&observation.mint_id)
                    && field("mint_quote_id") == Some(&observation.quote_id)
            }
            (OperationKind::WalletQuoteClaim, WalletQuoteObservationRole::ClaimReceive) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
                    && field("mint_quote_id") == Some(&observation.quote_id)
            }
            _ => false,
        });
    if valid {
        Ok(())
    } else {
        Err(coded_invalid_request(
            "wallet_quote_observation_identity_mismatch",
            "terminal wallet quote observations do not match the admitted typed operation",
        ))
    }
}

fn compact_draft_mutation(draft: Draft, changed_paths: Vec<String>) -> DraftMutationResult {
    DraftMutationResult {
        draft_id: draft.id,
        version: draft.version,
        component_count: u32::try_from(draft.lab.components.len()).unwrap_or(u32::MAX),
        link_count: u32::try_from(draft.lab.links.len()).unwrap_or(u32::MAX),
        valid: validate_lab(&draft.lab).valid,
        changed_paths,
    }
}

fn evidence_export_response(
    bundle: EvidenceBundle,
    resource_uri: String,
    include_content: bool,
) -> EvidenceExportResponse {
    EvidenceExportResponse {
        media_type: bundle.media_type,
        digest: bundle.digest,
        byte_length: bundle.byte_length,
        workspace_id: bundle.content.workspace_id.clone(),
        experiment_id: bundle.content.experiment.id.clone(),
        revision_digest: bundle.content.instance.revision_digest.clone(),
        lock_digest: bundle.content.instance.lock_digest.clone(),
        journal_count: u32::try_from(bundle.content.journal.len()).unwrap_or(u32::MAX),
        artifact_count: u32::try_from(bundle.content.artifacts.len()).unwrap_or(u32::MAX),
        resource_uri,
        content_included: include_content,
        content: include_content
            .then(|| serde_json::to_value(bundle.content).expect("typed evidence serializes")),
    }
}

fn evidence_resource_uri(request: &ArtifactExportRequest, digest: &str) -> String {
    let mut artifact_ids = request.artifact_operation_ids.clone();
    artifact_ids.sort();
    format!(
        "proofstorm://evidence/{}/{}?oracles={}&artifacts={}",
        request.experiment_id,
        digest,
        u8::from(request.include_oracle_artifacts),
        artifact_ids.join(",")
    )
}

fn encode_quote_cursor(experiment_id: &str, snapshot: u64, sequence: u64) -> String {
    let digest = digest_json(&(experiment_id, snapshot, sequence));
    format!("{snapshot}.{sequence}.{}", &digest[7..23])
}

fn decode_quote_cursor(cursor: &str, experiment_id: &str) -> Result<(u64, u64), ErrorData> {
    let mut parts = cursor.split('.');
    let snapshot = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| {
            coded_invalid_request(
                "invalid_wallet_quote_cursor",
                "wallet quote cursor is invalid",
            )
        })?;
    let sequence = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| {
            coded_invalid_request(
                "invalid_wallet_quote_cursor",
                "wallet quote cursor is invalid",
            )
        })?;
    let supplied_digest = parts
        .next()
        .filter(|_| parts.next().is_none())
        .ok_or_else(|| {
            coded_invalid_request(
                "invalid_wallet_quote_cursor",
                "wallet quote cursor is invalid",
            )
        })?;
    let expected = encode_quote_cursor(experiment_id, snapshot, sequence);
    if expected.rsplit_once('.').map(|(_, digest)| digest) != Some(supplied_digest) {
        return Err(coded_invalid_request(
            "invalid_wallet_quote_cursor",
            "wallet quote cursor does not belong to this experiment",
        ));
    }
    Ok((snapshot, sequence))
}

fn parse_evidence_resource_uri(uri: &str) -> Result<(ArtifactExportRequest, String), ErrorData> {
    let remainder = uri
        .strip_prefix("proofstorm://evidence/")
        .ok_or_else(|| ErrorData::resource_not_found("unknown Proofstorm resource URI", None))?;
    let (path, query) = remainder
        .split_once('?')
        .ok_or_else(|| ErrorData::resource_not_found("invalid evidence resource URI", None))?;
    let (experiment_id, digest) = path
        .split_once('/')
        .filter(|(experiment_id, digest)| {
            !experiment_id.is_empty() && !digest.is_empty() && !digest.contains('/')
        })
        .ok_or_else(|| ErrorData::resource_not_found("invalid evidence resource URI", None))?;
    let mut oracles = None;
    let mut artifacts = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| ErrorData::resource_not_found("invalid evidence resource URI", None))?;
        match key {
            "oracles" if oracles.is_none() => {
                oracles = Some(match value {
                    "0" => false,
                    "1" => true,
                    _ => {
                        return Err(ErrorData::resource_not_found(
                            "invalid evidence resource URI",
                            None,
                        ));
                    }
                });
            }
            "artifacts" if artifacts.is_none() => {
                artifacts = Some(if value.is_empty() {
                    Vec::new()
                } else {
                    value.split(',').map(str::to_owned).collect()
                });
            }
            _ => {
                return Err(ErrorData::resource_not_found(
                    "invalid evidence resource URI",
                    None,
                ));
            }
        }
    }
    Ok((
        ArtifactExportRequest {
            experiment_id: experiment_id.to_owned(),
            include_oracle_artifacts: oracles.ok_or_else(|| {
                ErrorData::resource_not_found("invalid evidence resource URI", None)
            })?,
            artifact_operation_ids: artifacts.ok_or_else(|| {
                ErrorData::resource_not_found("invalid evidence resource URI", None)
            })?,
            include_content: false,
        },
        digest.to_owned(),
    ))
}

fn evidence_json<T: Serialize + ?Sized>(value: &T) -> Result<serde_json::Value, ErrorData> {
    serde_json::to_value(value).map_err(|error| {
        ErrorData::internal_error(
            format!("failed to serialize evidence section: {error}"),
            Some(serde_json::json!({"code": "evidence_serialization_failed"})),
        )
    })
}

fn evidence_pointer(
    value: serde_json::Value,
    pointer: &str,
    section: &str,
) -> Result<serde_json::Value, ErrorData> {
    if pointer.is_empty() {
        return Ok(value);
    }
    if !pointer.starts_with('/') {
        return Err(coded_invalid_request(
            "evidence_pointer_invalid",
            "JSON Pointer must be empty or start with '/'",
        ));
    }
    value.pointer(pointer).cloned().ok_or_else(|| {
        coded_invalid_request(
            "evidence_pointer_not_found",
            format!("JSON Pointer {pointer:?} does not exist in the {section} section"),
        )
    })
}

fn publish_draft_response(
    revision: PublishedRevision,
    include_revision: bool,
) -> PublishDraftResponse {
    PublishDraftResponse {
        workspace_id: revision.workspace_id,
        digest: revision.digest,
        lock_digest: revision.lock.digest.clone(),
        component_count: u32::try_from(revision.lab.components.len()).unwrap_or(u32::MAX),
        revision_included: include_revision,
        lab: include_revision
            .then(|| serde_json::to_value(revision.lab).expect("typed lab serializes")),
        lock: include_revision
            .then(|| serde_json::to_value(revision.lock).expect("typed lock serializes")),
    }
}

fn catalog_page(request: &CatalogListRequest) -> Result<CatalogListResponse, ErrorData> {
    if !(1..=MAX_CATALOG_LIST_LIMIT).contains(&request.limit) {
        return Err(coded_invalid_request(
            "catalog_limit_invalid",
            "catalog list limit must be in 1..=50",
        ));
    }
    let catalog = default_catalog();
    let catalog_digest = digest_json(&catalog);
    let filter_digest = catalog_filter_digest(request);
    let mut entries = catalog
        .entries
        .iter()
        .filter(|entry| catalog_entry_matches(entry, request))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.version.cmp(&right.version))
    });
    let start = match request.cursor.as_deref() {
        None => 0,
        Some(cursor) => entries
            .iter()
            .position(|entry| catalog_cursor(&catalog_digest, &filter_digest, entry) == cursor)
            .map(|position| position + 1)
            .ok_or_else(|| {
                coded_invalid_request(
                    "catalog_cursor_invalid",
                    "catalog cursor is invalid, stale, or belongs to different filters",
                )
            })?,
    };
    let preferred = catalog
        .implementations
        .iter()
        .map(|support| (&support.implementation, &support.preferred_version))
        .collect::<BTreeSet<_>>();
    let summaries = entries
        .iter()
        .skip(start)
        .take(usize::try_from(request.limit).unwrap_or(usize::MAX))
        .map(|entry| CatalogEntrySummary {
            id: entry.id.clone(),
            kind: entry.kind,
            version: entry.version.clone(),
            preferred: preferred.contains(&(&entry.id, &entry.version)),
            adapter_version: entry.adapter_version.clone(),
            protocol_action_adapter_version: entry.protocol_action_adapter_version.clone(),
            config_version: entry.config_version.clone(),
            config_schema_digest: entry.config_schema_digest.clone(),
            release_channel: entry.release_channel,
            support_lifecycle: entry.support_lifecycle,
        })
        .collect::<Vec<_>>();
    let mut end = summaries.len();
    loop {
        let has_more = start + end < entries.len();
        let next_cursor = has_more && end > 0;
        let response = CatalogListResponse {
            api_version: catalog.api_version.clone(),
            catalog_digest: catalog_digest.clone(),
            items: summaries[..end].to_vec(),
            next_cursor: next_cursor
                .then(|| catalog_cursor(&catalog_digest, &filter_digest, entries[start + end - 1])),
        };
        if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
            return Ok(response);
        }
        if end == 0 {
            return Err(coded_invalid_request(
                "catalog_response_too_large",
                "catalog page envelope exceeds the agent response budget",
            ));
        }
        end -= 1;
    }
}

fn catalog_filter_digest(request: &CatalogListRequest) -> String {
    digest_json(&(
        "proofstorm/catalog-filter/v1",
        &request.implementations,
        &request.kinds,
        &request.features_all,
        &request.release_channels,
        &request.support_lifecycles,
        &request.dependency,
    ))
}

fn catalog_entry_matches(entry: &CatalogEntry, request: &CatalogListRequest) -> bool {
    (request.implementations.is_empty() || request.implementations.contains(&entry.id))
        && (request.kinds.is_empty() || request.kinds.contains(&entry.kind))
        && request.features_all.is_subset(&entry.features)
        && (request.release_channels.is_empty()
            || request.release_channels.contains(&entry.release_channel))
        && (request.support_lifecycles.is_empty()
            || request
                .support_lifecycles
                .contains(&entry.support_lifecycle))
        && request.dependency.as_ref().is_none_or(|filter| {
            entry.compatible_dependencies.iter().any(|dependency| {
                dependency.link_kind == filter.link_kind
                    && dependency.implementation == filter.implementation
                    && filter
                        .version
                        .as_ref()
                        .is_none_or(|version| dependency.versions.contains(version))
            })
        })
}

fn catalog_cursor(catalog_digest: &str, filter_digest: &str, entry: &CatalogEntry) -> String {
    digest_json(&(
        "proofstorm/catalog-cursor/v1",
        catalog_digest,
        filter_digest,
        &entry.id,
        &entry.version,
    ))
}

fn exact_catalog_entry<'a>(
    entries: &'a [CatalogEntry],
    id: &str,
    version: &str,
) -> Result<&'a CatalogEntry, ErrorData> {
    entries
        .iter()
        .find(|entry| entry.id == id && entry.version == version)
        .ok_or_else(|| {
            ErrorData::resource_not_found(
                format!("catalog entry {id:?} version {version:?} was not found"),
                Some(serde_json::json!({"code": "catalog_entry_not_found"})),
            )
        })
}

fn catalog_config_schema(
    request: CatalogConfigSchemaRequest,
) -> Result<CatalogConfigSchemaResponse, ErrorData> {
    if !request.pointer.is_empty() && !request.pointer.starts_with('/') {
        return Err(coded_invalid_request(
            "catalog_schema_pointer_invalid",
            "configuration schema pointer must be empty or begin with '/'",
        ));
    }
    let catalog = default_catalog();
    let entry = exact_catalog_entry(&catalog.entries, &request.id, &request.version)?;
    let schema = if request.pointer.is_empty() {
        entry.config_schema.clone()
    } else {
        entry
            .config_schema
            .pointer(&request.pointer)
            .cloned()
            .ok_or_else(|| {
                ErrorData::resource_not_found(
                    format!(
                        "configuration schema pointer {:?} was not found for {:?} version {:?}",
                        request.pointer, request.id, request.version
                    ),
                    Some(serde_json::json!({"code": "catalog_schema_pointer_not_found"})),
                )
            })?
    };
    let mut referenced_schemas = BTreeMap::new();
    collect_local_schema_references(&schema, &entry.config_schema, &mut referenced_schemas)?;
    Ok(CatalogConfigSchemaResponse {
        id: entry.id.clone(),
        version: entry.version.clone(),
        config_version: entry.config_version.clone(),
        config_schema_digest: entry.config_schema_digest.clone(),
        fragment: !request.pointer.is_empty(),
        pointer: request.pointer,
        schema,
        referenced_schemas,
    })
}

fn collect_local_schema_references(
    value: &serde_json::Value,
    root: &serde_json::Value,
    referenced: &mut BTreeMap<String, serde_json::Value>,
) -> Result<(), ErrorData> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_local_schema_references(value, root, referenced)?;
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str)
                && let Some(pointer) = reference.strip_prefix('#')
                && !referenced.contains_key(reference)
            {
                let target = if pointer.is_empty() {
                    root
                } else {
                    root.pointer(pointer).ok_or_else(|| {
                        coded_invalid_request(
                            "catalog_schema_reference_invalid",
                            format!(
                                "configuration schema contains unresolved reference {reference:?}"
                            ),
                        )
                    })?
                };
                referenced.insert(reference.to_owned(), target.clone());
                collect_local_schema_references(target, root, referenced)?;
            }
            for nested in object.values() {
                collect_local_schema_references(nested, root, referenced)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn bounded_agent_response<T: Serialize>(value: T) -> Result<T, ErrorData> {
    let size = serialized_size(&value)?;
    if size > MAX_AGENT_RESPONSE_BYTES {
        return Err(ErrorData::invalid_request(
            format!("agent response is {size} bytes; maximum is {MAX_AGENT_RESPONSE_BYTES} bytes"),
            Some(serde_json::json!({
                "code": "agent_response_too_large",
                "actual_bytes": size,
                "maximum_bytes": MAX_AGENT_RESPONSE_BYTES,
            })),
        ));
    }
    Ok(value)
}

fn serialized_size(value: &impl Serialize) -> Result<usize, ErrorData> {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .map_err(|error| {
            ErrorData::internal_error(
                format!("failed to measure agent response: {error}"),
                Some(serde_json::json!({"code": "response_serialization_failed"})),
            )
        })
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ProofstormMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Use compact list and receipt tools for discovery. Read full evidence only through its manifest resource_uri; use proofstorm_evidence_section_read for bounded inspection.",
        )
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(
                "proofstorm://evidence/{experiment_id}/{digest}{?oracles,artifacts}",
                "proofstorm-evidence-bundle",
            )
            .with_title("Proofstorm evidence bundle")
            .with_description(
                "Complete deterministic evidence bundle identified by a manifest returned from proofstorm_artifact_export",
            )
            .with_mime_type("application/vnd.proofstorm.evidence.v1alpha1+json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (export_request, expected_digest) = parse_evidence_resource_uri(&request.uri)?;
        let bundle = self.build_evidence_bundle(&export_request)?;
        if bundle.digest != expected_digest {
            return Err(ErrorData::resource_not_found(
                "evidence resource digest does not match current durable content",
                Some(serde_json::json!({"code": "evidence_digest_mismatch"})),
            ));
        }
        let text = serde_json::to_string(&bundle).map_err(|error| {
            ErrorData::internal_error(
                format!("failed to serialize evidence resource: {error}"),
                Some(serde_json::json!({"code": "evidence_serialization_failed"})),
            )
        })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri)
                .with_mime_type("application/vnd.proofstorm.evidence.v1alpha1+json"),
        ])
        .into())
    }
}

fn tool_capabilities() -> Vec<(&'static str, &'static [Capability])> {
    let mut tools = design_tool_capabilities();
    tools.extend(runtime_tool_capabilities());
    tools
}

fn design_tool_capabilities() -> Vec<(&'static str, &'static [Capability])> {
    vec![
        ("proofstorm_workspace_read", &[Capability::LabRead]),
        ("proofstorm_catalog_list", &[Capability::CatalogRead]),
        ("proofstorm_catalog_entry_read", &[Capability::CatalogRead]),
        (
            "proofstorm_catalog_config_schema_read",
            &[Capability::CatalogRead],
        ),
        (
            "proofstorm_network_capabilities",
            &[Capability::CatalogRead],
        ),
        ("proofstorm_lab_create", &[Capability::LabCreate]),
        ("proofstorm_lab_read", &[Capability::LabRead]),
        ("proofstorm_lab_edit", &[Capability::LabEdit]),
        (
            "proofstorm_component_add",
            &[Capability::LabEdit, Capability::TopologyMutate],
        ),
        (
            "proofstorm_component_update",
            &[Capability::LabEdit, Capability::TopologyMutate],
        ),
        (
            "proofstorm_component_remove",
            &[Capability::LabEdit, Capability::TopologyMutate],
        ),
        (
            "proofstorm_link_add",
            &[Capability::LabEdit, Capability::TopologyMutate],
        ),
        (
            "proofstorm_link_remove",
            &[Capability::LabEdit, Capability::TopologyMutate],
        ),
        ("proofstorm_lab_clone", &[Capability::LabClone]),
        ("proofstorm_lab_validate", &[Capability::LabValidate]),
        ("proofstorm_lab_diff", &[Capability::LabRead]),
        ("proofstorm_lab_publish", &[Capability::LabPublish]),
        ("proofstorm_lab_materialize", &[Capability::LabMaterialize]),
        ("proofstorm_lab_status", &[Capability::LabStatus]),
        (
            "proofstorm_lab_component_status_list",
            &[Capability::LabStatus],
        ),
        ("proofstorm_lab_inventory_list", &[Capability::LabStatus]),
        ("proofstorm_lab_wait", &[Capability::LabStatus]),
        ("proofstorm_lab_close", &[Capability::LabClose]),
        (
            "proofstorm_experiment_create",
            &[Capability::ExperimentCreate],
        ),
        ("proofstorm_experiment_read", &[Capability::ExperimentRead]),
        (
            "proofstorm_experiment_close",
            &[Capability::ExperimentClose],
        ),
        ("proofstorm_lease_acquire", &[Capability::LeaseAcquire]),
        ("proofstorm_lease_read", &[Capability::ExperimentRead]),
        ("proofstorm_lease_release", &[Capability::LeaseRelease]),
    ]
}

fn runtime_tool_capabilities() -> Vec<(&'static str, &'static [Capability])> {
    vec![
        ("proofstorm_node_start", &[Capability::NodeControl]),
        ("proofstorm_node_stop", &[Capability::NodeControl]),
        ("proofstorm_node_restart", &[Capability::NodeControl]),
        ("proofstorm_component_exec", &[Capability::ComponentExec]),
        (
            "proofstorm_liquidity_bootstrap",
            &[
                Capability::ChainMine,
                Capability::WalletFund,
                Capability::PeerConnect,
                Capability::ChannelOpen,
            ],
        ),
        ("proofstorm_peer_connect", &[Capability::PeerConnect]),
        ("proofstorm_peer_disconnect", &[Capability::PeerDisconnect]),
        (
            "proofstorm_channel_open",
            &[Capability::ChannelOpen, Capability::ChainMine],
        ),
        (
            "proofstorm_channel_close",
            &[Capability::ChannelClose, Capability::ChainMine],
        ),
        (
            "proofstorm_channel_force_close",
            &[Capability::ChannelForceClose, Capability::ChainMine],
        ),
        (
            "proofstorm_channel_rebalance",
            &[Capability::ChannelRebalance],
        ),
        (
            "proofstorm_network_partition",
            &[Capability::NetworkPartition],
        ),
        ("proofstorm_network_delay", &[Capability::NetworkDelay]),
        ("proofstorm_network_loss", &[Capability::NetworkDrop]),
        ("proofstorm_network_heal", &[Capability::NetworkHeal]),
        ("proofstorm_wallet_initialize", &[Capability::WalletCreate]),
        ("proofstorm_wallet_balance", &[Capability::WalletControl]),
        ("proofstorm_wallet_fund", &[Capability::WalletFund]),
        ("proofstorm_wallet_invoice", &[Capability::WalletFund]),
        ("proofstorm_component_logs", &[Capability::ComponentLogs]),
        (
            "proofstorm_authentication_conformance",
            &[Capability::AuthenticationTest],
        ),
        (
            "proofstorm_authentication_protected_spend",
            &[Capability::AuthenticationTest],
        ),
        (
            "proofstorm_authentication_replay",
            &[Capability::AuthenticationTest, Capability::ArtifactRead],
        ),
        (
            "proofstorm_wallet_pay",
            &[Capability::WalletControl, Capability::ArtifactRead],
        ),
        (
            "proofstorm_wallet_quote_claim",
            &[Capability::WalletControl],
        ),
        (
            "proofstorm_wallet_round_trip",
            &[
                Capability::WalletCreate,
                Capability::WalletFund,
                Capability::WalletControl,
            ],
        ),
        ("proofstorm_conservation_oracle", &[Capability::OracleRun]),
        ("proofstorm_reachability_oracle", &[Capability::OracleRun]),
        ("proofstorm_action_cancel", &[Capability::ActionCancel]),
        ("proofstorm_operation_status", &[Capability::ArtifactRead]),
        ("proofstorm_operation_wait", &[Capability::ArtifactRead]),
        ("proofstorm_action_list", &[Capability::ExperimentRead]),
        (
            "proofstorm_artifact_export",
            &[Capability::ExperimentRead, Capability::ArtifactRead],
        ),
        (
            "proofstorm_evidence_section_read",
            &[Capability::ExperimentRead, Capability::ArtifactRead],
        ),
        ("proofstorm_action_status", &[Capability::ArtifactRead]),
        (
            "proofstorm_wallet_quote_status",
            &[Capability::ArtifactRead],
        ),
        (
            "proofstorm_wallet_quote_list",
            &[Capability::ExperimentRead],
        ),
    ]
}

impl ProofstormMcp {
    fn runtime(&self) -> Result<&KubernetesRuntime, ErrorData> {
        self.kubernetes.as_ref().ok_or_else(|| {
            coded_invalid_request(
                "runtime_unavailable",
                "Kubernetes runtime is not configured",
            )
        })
    }

    async fn submit_channel_close(
        &self,
        request: ChannelCloseRequest,
        force: bool,
    ) -> Result<Json<LabOperation>, ErrorData> {
        let (capability, kind) = if force {
            (
                Capability::ChannelForceClose,
                OperationKind::ChannelForceClose,
            )
        } else {
            (Capability::ChannelClose, OperationKind::ChannelClose)
        };
        self.authorize_all(&[capability, Capability::ChainMine])?;
        validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
        validate_channel_id(&request.channel_id)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                capability,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.chain, ComponentKind::Bitcoin)?;
        component_image_any(&revision, &request.from_lightning, ComponentKind::Lightning)?;
        component_image_any(&revision, &request.to_lightning, ComponentKind::Lightning)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            kind,
            &request,
            &request.idempotency_key,
            capability,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let parameters = ChannelCloseAction {
            chain: request.chain,
            from_lightning: request.from_lightning,
            to_lightning: request.to_lightning,
            channel_id: request.channel_id,
        };
        let action = if force {
            LabAction::ChannelForceClose(parameters)
        } else {
            LabAction::ChannelClose(parameters)
        };
        let resource = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            action,
        );
        self.runtime()?.apply_action(&instance, &resource).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    async fn submit_node_control(
        &self,
        request: NodeControlRequest,
        kind: OperationKind,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::NodeControl)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::NodeControl,
            )
            .map_err(store_error)?;
        let component = revision
            .lab
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("node component is not part of this lab revision"))?;
        if !matches!(
            component.kind,
            ComponentKind::Bitcoin | ComponentKind::Lightning
        ) {
            return Err(invalid_operation(
                "node lifecycle currently supports Bitcoin and Lightning components",
            ));
        }
        component_image_any(&revision, &request.component, component.kind)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            kind,
            &request,
            &request.idempotency_key,
            Capability::NodeControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let parameters = NodeControlAction {
            component: request.component,
        };
        let action = match kind {
            OperationKind::NodeStart => LabAction::NodeStart(parameters),
            OperationKind::NodeStop => LabAction::NodeStop(parameters),
            OperationKind::NodeRestart => LabAction::NodeRestart(parameters),
            _ => {
                return Err(ErrorData::internal_error(
                    "invalid node lifecycle operation kind",
                    Some(serde_json::json!({"code": "controller_invariant"})),
                ));
            }
        };
        let resource = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            action,
        );
        self.runtime()?.apply_action(&instance, &resource).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the MCP-to-journal boundary passes every authority and immutable identity explicitly"
    )]
    fn create_operation<R: Serialize>(
        &self,
        instance_id: &str,
        experiment_id: &str,
        lease_id: &str,
        operation_id: &str,
        kind: OperationKind,
        request: &R,
        idempotency_key: &str,
        capability: Capability,
    ) -> Result<LabOperation, ErrorData> {
        let mut value = serde_json::to_value(request).map_err(|error| {
            ErrorData::internal_error(
                format!("operation request serialization failed: {error}"),
                Some(serde_json::json!({"code": "serialization_failed"})),
            )
        })?;
        if let Some(object) = value.as_object_mut() {
            object.remove("idempotency_key");
        }
        self.store
            .create_operation(
                &self.workspace,
                &self.principal,
                instance_id,
                experiment_id,
                lease_id,
                operation_id,
                kind,
                &value,
                idempotency_key,
                capability,
            )
            .map_err(store_error)
    }
}

fn component_image(
    revision: &PublishedRevision,
    id: &str,
    kind: ComponentKind,
    implementation: &str,
) -> Result<String, ErrorData> {
    let Some(component) = revision
        .lab
        .components
        .iter()
        .find(|component| component.id == id)
    else {
        return Err(invalid_operation(&format!(
            "component {id:?} is not part of this lab revision"
        )));
    };
    if component.kind != kind || component.implementation != implementation {
        return Err(invalid_operation(&format!(
            "component {id:?} must be a {kind:?} component using {implementation:?}"
        )));
    }
    revision
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id)
        .map(|entry| entry.image.clone())
        .ok_or_else(|| invalid_operation(&format!("component {id:?} has no immutable lock entry")))
}

fn component_image_any(
    revision: &PublishedRevision,
    id: &str,
    kind: ComponentKind,
) -> Result<String, ErrorData> {
    let Some(component) = revision
        .lab
        .components
        .iter()
        .find(|component| component.id == id)
    else {
        return Err(invalid_operation(&format!(
            "component {id:?} is not part of this lab revision"
        )));
    };
    if component.kind != kind {
        return Err(invalid_operation(&format!(
            "component {id:?} must be a {kind:?} component"
        )));
    }
    revision
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id && entry.catalog_id == component.implementation)
        .map(|entry| entry.image.clone())
        .ok_or_else(|| invalid_operation(&format!("component {id:?} has no immutable lock entry")))
}

fn validate_authentication_components(
    revision: &PublishedRevision,
    mint: &str,
    identity_provider: &str,
) -> Result<(), ErrorData> {
    component_image(revision, mint, ComponentKind::Mint, "nutshell")?;
    component_image(
        revision,
        identity_provider,
        ComponentKind::IdentityProvider,
        "keycloak",
    )?;
    let links = revision
        .lab
        .links
        .iter()
        .filter(|link| {
            link.kind == LinkKind::AuthenticationBackend
                && link.from == mint
                && link.to == identity_provider
                && matches!(
                    link.binding.as_ref(),
                    Some(proofstorm_core::DependencyBinding::Authentication {
                        protocol: AuthenticationProtocol::Oidc
                    })
                )
        })
        .count();
    if links == 1 {
        Ok(())
    } else {
        Err(invalid_operation(&format!(
            "authentication conformance requires exactly one OIDC link from {mint:?} to {identity_provider:?}"
        )))
    }
}

fn runtime_action_resource(
    control_namespace: &str,
    instance: &LabInstance,
    operation: &LabOperation,
    action: LabAction,
) -> ProofstormLabAction {
    let mut resource = ProofstormLabAction::new(
        &operation.resource_name,
        ProofstormLabActionSpec {
            lab_name: instance.resource_name.clone(),
            workspace_id: operation.workspace_id.clone(),
            instance_id: operation.instance_id.clone(),
            instance_key: instance.instance_key.clone(),
            experiment_id: operation.experiment_id.clone(),
            lease_id: operation.lease_id.clone(),
            principal_id: operation.principal_id.clone(),
            sequence: operation.sequence,
            operation_id: operation.id.clone(),
            request_digest: operation.request_digest.clone(),
            capability: operation.capability,
            accepted_at_unix: operation.accepted_at_unix,
            action,
        },
    );
    resource.metadata.namespace = Some(control_namespace.to_owned());
    resource.metadata.labels = Some(std::collections::BTreeMap::from([
        (
            "proofstorm.dev/instance".to_owned(),
            instance.instance_key.clone(),
        ),
        (
            "proofstorm.dev/lab".to_owned(),
            instance.resource_name.clone(),
        ),
        (
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstorm-mcp".to_owned(),
        ),
    ]));
    resource
}

/// On-chain headroom the bootstrap keeps for the channel funding transaction
/// fee. Regtest funding transactions settle for a few thousand satoshis; this
/// is deliberately generous relative to the 20,000 sat minimum channel.
const BOOTSTRAP_FUNDING_MARGIN_SAT: u64 = 10_000;

fn validate_bootstrap_bounds(request: &BootstrapLiquidityRequest) -> Result<(), ErrorData> {
    if !(1..=1_000_000_000).contains(&request.funding_sat) {
        return Err(invalid_operation(
            "funding_sat must be between 1 and 1,000,000,000",
        ));
    }
    if !(20_000..=100_000_000).contains(&request.channel_sat) {
        return Err(invalid_operation(
            "channel_sat must be between 20,000 and 100,000,000",
        ));
    }
    if request.push_sat > request.channel_sat / 2 {
        return Err(invalid_operation("push_sat cannot exceed half the channel"));
    }
    // The funding transaction pays a miner fee out of the same on-chain output,
    // so a channel equal to the funding amount always fails inside the Job with
    // an insufficient-funds error that only reaches the node's own log.
    if request.channel_sat + BOOTSTRAP_FUNDING_MARGIN_SAT > request.funding_sat {
        return Err(coded_invalid_request(
            "insufficient_funding_margin",
            format!(
                "funding_sat must exceed channel_sat by at least {BOOTSTRAP_FUNDING_MARGIN_SAT} sat to pay the funding transaction fee; channel_sat {} needs funding_sat of at least {}",
                request.channel_sat,
                request.channel_sat + BOOTSTRAP_FUNDING_MARGIN_SAT
            ),
        ));
    }
    if request.mint_lightning == request.payer_lightning {
        return Err(invalid_operation(
            "mint and payer Lightning components must be distinct",
        ));
    }
    Ok(())
}

fn validate_lightning_pair(from: &str, to: &str) -> Result<(), ErrorData> {
    if from == to {
        return Err(invalid_operation(
            "from and to Lightning components must be distinct",
        ));
    }
    Ok(())
}

fn validate_network_pair(from: &str, to: &str) -> Result<(), ErrorData> {
    if from == to {
        return Err(invalid_operation(
            "network fault endpoints must be distinct logical components",
        ));
    }
    Ok(())
}

fn validate_network_delay_bounds(request: &NetworkDelayRequest) -> Result<(), ErrorData> {
    if !(1..=MAX_NETWORK_DELAY_MS).contains(&request.delay_ms) {
        return Err(invalid_operation(&format!(
            "delay_ms must be between 1 and {MAX_NETWORK_DELAY_MS}"
        )));
    }
    if request.jitter_ms > MAX_NETWORK_JITTER_MS || request.jitter_ms > request.delay_ms {
        return Err(invalid_operation(&format!(
            "jitter_ms cannot exceed delay_ms or {MAX_NETWORK_JITTER_MS}"
        )));
    }
    Ok(())
}

fn validate_network_loss_bounds(request: &NetworkLossRequest) -> Result<(), ErrorData> {
    if !(1..=MAX_NETWORK_LOSS_BASIS_POINTS).contains(&request.loss_basis_points) {
        return Err(invalid_operation(&format!(
            "loss_basis_points must be between 1 and {MAX_NETWORK_LOSS_BASIS_POINTS}"
        )));
    }
    Ok(())
}

fn require_network_fault_support(
    feature: NetworkFaultFeature,
    direction: NetworkFaultDirection,
) -> Result<(), ErrorData> {
    let backend = network_policy_fault_backend();
    if backend.supports(feature) && backend.directions.contains(&direction) {
        return Ok(());
    }
    Err(ErrorData::invalid_request(
        format!(
            "network fault backend {:?} does not support {feature:?} with {direction:?} direction",
            backend.id
        ),
        Some(serde_json::json!({
            "code": "network_fault_unsupported",
            "backend_id": backend.id,
            "backend_version": backend.version,
            "feature": feature,
            "direction": direction,
        })),
    ))
}

fn network_fault_contract_violation(feature: NetworkFaultFeature) -> ErrorData {
    ErrorData::internal_error(
        format!("network fault backend advertises unimplemented {feature:?} support"),
        Some(serde_json::json!({"code": "network_fault_backend_contract_violation"})),
    )
}

fn validate_channel_bounds(channel_sat: u64, push_sat: u64) -> Result<(), ErrorData> {
    if !(20_000..=100_000_000).contains(&channel_sat) {
        return Err(invalid_operation(
            "channel_sat must be between 20,000 and 100,000,000",
        ));
    }
    if push_sat > channel_sat / 2 {
        return Err(invalid_operation("push_sat cannot exceed half the channel"));
    }
    Ok(())
}

fn validate_channel_id(channel_id: &str) -> Result<(), ErrorData> {
    let digest = channel_id.strip_prefix("ch-").unwrap_or_default();
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_operation(
            "channel_id must be an opaque ch- prefixed SHA-256 handle",
        ));
    }
    Ok(())
}

fn validate_rebalance_bounds(request: &ChannelRebalanceRequest) -> Result<(), ErrorData> {
    if request.outgoing_channel_id == request.incoming_channel_id {
        return Err(invalid_operation(
            "outgoing and incoming channel handles must differ",
        ));
    }
    if !(1..=10_000_000).contains(&request.amount_sat) {
        return Err(invalid_operation(
            "amount_sat must be between 1 and 10,000,000",
        ));
    }
    if request.max_fee_sat > request.amount_sat || request.max_fee_sat > 100_000 {
        return Err(invalid_operation(
            "max_fee_sat cannot exceed amount_sat or 100,000",
        ));
    }
    Ok(())
}

fn validate_wallet_bounds(request: &WalletRoundTripRequest) -> Result<(), ErrorData> {
    validate_wallet_amount(request.amount_sat)?;
    if request.tolerance_sat > request.amount_sat || request.tolerance_sat > 10_000 {
        return Err(invalid_operation(
            "tolerance_sat cannot exceed amount_sat or 10,000 sat",
        ));
    }
    Ok(())
}

fn validate_wallet_amount(amount_sat: u64) -> Result<(), ErrorData> {
    if !(1..=500_000).contains(&amount_sat) {
        return Err(invalid_operation(
            "amount_sat must be between 1 and 500,000",
        ));
    }
    Ok(())
}

fn validate_oracle_bounds(request: &ConservationOracleRequest) -> Result<(), ErrorData> {
    if request.expected_sat > 100_000_000 || request.tolerance_sat > 10_000 {
        return Err(invalid_operation(
            "expected_sat cannot exceed 100,000,000 and tolerance_sat cannot exceed 10,000",
        ));
    }
    Ok(())
}

fn validate_reachability_oracle_bounds(
    request: &ReachabilityOracleRequest,
) -> Result<(), ErrorData> {
    if request.from_component == request.to_component {
        return Err(invalid_operation(
            "from_component and to_component must differ",
        ));
    }
    if !(1..=5).contains(&request.timeout_seconds) {
        return Err(invalid_operation("timeout_seconds must be between 1 and 5"));
    }
    if !(1..=5).contains(&request.attempts) {
        return Err(invalid_operation("attempts must be between 1 and 5"));
    }
    Ok(())
}

fn validate_wait_timeout(timeout_seconds: u32) -> Result<(), ErrorData> {
    if (1..=120).contains(&timeout_seconds) {
        return Ok(());
    }
    Err(ErrorData::invalid_request(
        "timeout_seconds must be between 1 and 120".to_owned(),
        Some(serde_json::json!({"code": "wait_timeout_invalid"})),
    ))
}

const fn lab_wait_terminal(phase: InstancePhase) -> bool {
    matches!(phase, InstancePhase::Closed | InstancePhase::CleanupBlocked)
}

const fn operation_terminal(phase: OperationPhase) -> bool {
    matches!(
        phase,
        OperationPhase::Succeeded | OperationPhase::Failed | OperationPhase::Cancelled
    )
}

fn validate_status_list_limit(limit: u32) -> Result<(), ErrorData> {
    if (1..=50).contains(&limit) {
        return Ok(());
    }
    Err(ErrorData::invalid_request(
        "status list limit must be between 1 and 50".to_owned(),
        Some(serde_json::json!({"code": "status_list_limit_invalid"})),
    ))
}

fn status_page_start<T>(
    cursor: Option<&str>,
    items: &[T],
    cursor_for: impl Fn(&T) -> String,
) -> Result<usize, ErrorData> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    items
        .iter()
        .position(|item| cursor_for(item) == cursor)
        .map(|position| position + 1)
        .ok_or_else(|| {
            ErrorData::invalid_request(
                "status cursor is invalid or belongs to an older snapshot".to_owned(),
                Some(serde_json::json!({"code": "status_cursor_invalid"})),
            )
        })
}

fn status_cursor(kind: &str, instance_id: &str, snapshot_digest: &str, boundary: &str) -> String {
    digest_json(&(
        "proofstorm-status-cursor/v1",
        kind,
        instance_id,
        snapshot_digest,
        boundary,
    ))
}

fn inventory_key(entry: &InventoryEntry) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        entry.api_version, entry.kind, entry.namespace, entry.name
    )
}

fn compact_lab_status(mut status: LabInstanceStatus) -> LabStatusSummary {
    let ready_components = status
        .components
        .iter()
        .filter(|component| component.ready)
        .count();
    status.inventory.sort_by_key(inventory_key);
    LabStatusSummary {
        instance_id: status.instance.id,
        revision_digest: status.instance.revision_digest,
        lock_digest: status.instance.lock_digest,
        phase: status.phase,
        instance_namespace: status.instance_namespace,
        ready_components: u32::try_from(ready_components).unwrap_or(u32::MAX),
        total_components: u32::try_from(status.components.len()).unwrap_or(u32::MAX),
        inventory_count: u32::try_from(status.inventory.len()).unwrap_or(u32::MAX),
        inventory_digest: digest_json(&status.inventory),
        teardown_receipt: status.teardown_receipt,
        message: status.message,
    }
}

fn compact_lab_wait(
    status: LabInstanceStatus,
    target_phase: InstancePhase,
    reached: bool,
    timed_out: bool,
) -> LabWaitResult {
    let ready_components = status
        .components
        .iter()
        .filter(|component| component.ready)
        .count();
    LabWaitResult {
        instance_id: status.instance.id,
        phase: status.phase,
        target_phase,
        reached,
        timed_out,
        ready_components: u32::try_from(ready_components).unwrap_or(u32::MAX),
        total_components: u32::try_from(status.components.len()).unwrap_or(u32::MAX),
        teardown_receipt: status.teardown_receipt,
        message: status.message,
    }
}

fn compact_operation_wait(operation: LabOperation, timed_out: bool) -> OperationWaitResult {
    OperationWaitResult {
        operation_id: operation.id,
        sequence: operation.sequence,
        kind: operation.kind,
        phase: operation.phase,
        terminal: operation_terminal(operation.phase),
        timed_out,
        artifact: operation.artifact,
    }
}

/// One coded invalid-request error.
///
/// The `code` travels in the error payload so agents can branch on a stable
/// identifier rather than parsing prose.
fn coded_invalid_request(code: &str, message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_request(message.into(), Some(serde_json::json!({"code": code})))
}

fn invalid_operation(message: &str) -> ErrorData {
    coded_invalid_request("invalid_operation", message)
}

impl KubernetesRuntime {
    async fn apply_action(
        &self,
        instance: &LabInstance,
        action: &ProofstormLabAction,
    ) -> Result<(), ErrorData> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        let lab = labs
            .get(&instance.resource_name)
            .await
            .map_err(kube_error)?;
        if lab.status.as_ref().map(|status| status.phase) != Some(LabPhase::Ready) {
            return Err(coded_invalid_request(
                "instance_not_ready",
                format!("lab instance {:?} is not ready for actions", instance.id),
            ));
        }
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let name = action.metadata.name.as_deref().ok_or_else(|| {
            ErrorData::internal_error(
                "typed action has no resource name",
                Some(serde_json::json!({"code": "render_failure"})),
            )
        })?;
        if let Some(existing) = actions.get_opt(name).await.map_err(kube_error)? {
            if existing.spec != action.spec {
                return Err(coded_invalid_request(
                    "action_identity_conflict",
                    format!("action resource {name:?} already exists with a different request"),
                ));
            }
            return Ok(());
        }
        actions
            .patch(
                name,
                &PatchParams::apply("proofstorm-mcp"),
                &Patch::Apply(action),
            )
            .await
            .map_err(kube_error)?;
        Ok(())
    }

    async fn action_status(
        &self,
        operation: &LabOperation,
    ) -> Result<Option<(OperationPhase, serde_json::Value)>, ErrorData> {
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let Some(action) = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
        else {
            // The runtime resource is gone (lab closed, or garbage collected)
            // before the journal saw a terminal phase. A running operation
            // whose resource vanished is a terminal outcome, never a live
            // one; a pending operation may simply not be applied yet.
            return Ok((operation.phase == OperationPhase::Running)
                .then(|| (OperationPhase::Failed, missing_action_artifact(operation))));
        };
        let Some(status) = action.status else {
            return Ok(None);
        };
        match status.phase {
            ActionPhase::Pending | ActionPhase::Running => Ok(None),
            ActionPhase::Succeeded => Ok(Some((
                OperationPhase::Succeeded,
                status.artifact.map_or_else(
                    || serde_json::json!({"code": "terminal_artifact_missing"}),
                    |artifact| serde_json::to_value(artifact).expect("typed artifact serializes"),
                ),
            ))),
            ActionPhase::Failed => Ok(Some((
                OperationPhase::Failed,
                status.error.map_or_else(
                    || serde_json::json!({"code": "action_failed"}),
                    |error| serde_json::to_value(error).expect("typed action error serializes"),
                ),
            ))),
            ActionPhase::Cancelled => Ok(Some((
                OperationPhase::Cancelled,
                serde_json::json!({"code": "action_cancelled"}),
            ))),
        }
    }

    /// Request cancellation of a runtime action. Returns `false` when the
    /// runtime resource no longer exists, so the caller finalizes the journal
    /// entry itself instead of leaving it non-terminal forever.
    async fn request_action_cancellation(
        &self,
        operation: &LabOperation,
        token: &str,
    ) -> Result<bool, ErrorData> {
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let Some(action) = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
        else {
            return Ok(false);
        };
        if action.spec.workspace_id != operation.workspace_id
            || action.spec.instance_id != operation.instance_id
            || action.spec.experiment_id != operation.experiment_id
            || action.spec.operation_id != operation.id
            || action.spec.principal_id != operation.principal_id
            || action.spec.request_digest != operation.request_digest
        {
            return Err(coded_invalid_request(
                "action_identity_conflict",
                "action cancellation identity does not match the journal",
            ));
        }
        if action.status.as_ref().is_some_and(|status| {
            matches!(
                status.phase,
                ActionPhase::Succeeded | ActionPhase::Failed | ActionPhase::Cancelled
            )
        }) || action.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
        {
            return Ok(true);
        }
        actions
            .patch(
                &operation.resource_name,
                &PatchParams::default(),
                &Patch::Merge(serde_json::json!({
                    "metadata": {"annotations": {(ACTION_CANCEL_ANNOTATION): token}}
                })),
            )
            .await
            .map_err(kube_error)?;
        Ok(true)
    }

    async fn materialize(
        &self,
        instance: LabInstance,
        revision: PublishedRevision,
    ) -> Result<LabInstanceStatus, ErrorData> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        let mut resource = ProofstormLab::new(
            &instance.resource_name,
            ProofstormLabSpec {
                workspace_id: instance.workspace_id.clone(),
                instance_id: instance.id.clone(),
                instance_key: instance.instance_key.clone(),
                revision_digest: instance.revision_digest.clone(),
                lock: revision.lock,
                lab: revision.lab,
            },
        );
        resource.metadata.namespace = Some(self.control_namespace.clone());
        let applied = labs
            .patch(
                &instance.resource_name,
                &PatchParams::apply("proofstorm-mcp").force(),
                &Patch::Apply(&resource),
            )
            .await
            .map_err(kube_error)?;
        Ok(status_from_resource(instance, &applied))
    }

    async fn status(&self, instance: LabInstance) -> Result<LabInstanceStatus, ErrorData> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        if let Some(resource) = labs
            .get_opt(&instance.resource_name)
            .await
            .map_err(kube_error)?
        {
            return Ok(status_from_resource(instance, &resource));
        }
        let receipts = Api::<ConfigMap>::namespaced(self.client.clone(), &self.control_namespace);
        let name = format!("proofstorm-teardown-{}", instance.instance_key);
        let receipt = receipts.get_opt(&name).await.map_err(kube_error)?;
        let Some(receipt) = receipt else {
            return Err(ErrorData::resource_not_found(
                format!(
                    "lab instance {:?} has no runtime resource or teardown receipt",
                    instance.id
                ),
                Some(serde_json::json!({"code": "runtime_not_found"})),
            ));
        };
        let data = receipt.data.unwrap_or_default();
        Ok(LabInstanceStatus {
            instance: instance.clone(),
            phase: InstancePhase::Closed,
            instance_namespace: data.get("instanceNamespace").cloned().unwrap_or_default(),
            components: vec![],
            inventory: vec![],
            teardown_receipt: Some(CoreTeardownReceipt {
                instance_id: instance.id,
                instance_namespace: data.get("instanceNamespace").cloned().unwrap_or_default(),
                inventory_digest: data.get("inventoryDigest").cloned().unwrap_or_default(),
                verified_absent: data
                    .get("verifiedAbsent")
                    .is_some_and(|value| value == "true"),
            }),
            message: None,
        })
    }

    async fn close(&self, instance: LabInstance) -> Result<LabInstanceStatus, ErrorData> {
        let mut status = self.status(instance.clone()).await?;
        if status.phase == InstancePhase::Closed {
            return Ok(status);
        }
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        labs.delete(&instance.resource_name, &DeleteParams::default())
            .await
            .map_err(kube_error)?;
        status.phase = InstancePhase::Closing;
        status.message = Some("deleting instance namespace and verifying absence".into());
        Ok(status)
    }
}

fn status_from_resource(instance: LabInstance, resource: &ProofstormLab) -> LabInstanceStatus {
    let status = resource.status.clone().unwrap_or_default();
    LabInstanceStatus {
        instance,
        phase: match status.phase {
            LabPhase::Pending => InstancePhase::Pending,
            LabPhase::Ready => InstancePhase::Ready,
            LabPhase::Closing => InstancePhase::Closing,
            LabPhase::CleanupBlocked => InstancePhase::CleanupBlocked,
        },
        instance_namespace: status.instance_namespace.unwrap_or_default(),
        components: status.components,
        inventory: status.inventory,
        teardown_receipt: status.teardown_receipt.map(|receipt| CoreTeardownReceipt {
            instance_id: receipt.instance_id,
            instance_namespace: receipt.instance_namespace,
            inventory_digest: receipt.inventory_digest,
            verified_absent: receipt.verified_absent,
        }),
        message: status.message,
    }
}

fn missing_action_artifact(operation: &LabOperation) -> serde_json::Value {
    serde_json::json!({
        "code": "action_runtime_not_found",
        "resource_name": operation.resource_name,
        "message": "the runtime action resource no longer exists; its outcome was not observed",
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err adapter owns the Kubernetes error"
)]
fn kube_error(error: kube::Error) -> ErrorData {
    ErrorData::internal_error(
        format!("Kubernetes runtime failure: {error}"),
        Some(serde_json::json!({"code": "runtime_failure"})),
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err adapters own their error and this function classifies its variant"
)]
fn store_error(error: StoreError) -> ErrorData {
    let data = Some(serde_json::json!({"code": error.code()}));
    match error {
        StoreError::Io(_)
        | StoreError::Database(_)
        | StoreError::Serialization(_)
        | StoreError::Poisoned
        | StoreError::VersionOverflow(_)
        | StoreError::InvalidStoredVersion(_) => ErrorData::internal_error(error.to_string(), data),
        StoreError::NotFound { .. } => ErrorData::resource_not_found(error.to_string(), data),
        _ => ErrorData::invalid_request(error.to_string(), data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{API_VERSION, LabPolicy};

    fn lab(name: &str) -> LabSpec {
        LabSpec {
            api_version: API_VERSION.into(),
            name: name.into(),
            components: vec![],
            links: vec![],
            policy: LabPolicy::default(),
        }
    }

    fn seeded_store() -> Store {
        let store = Store::memory().expect("store");
        store
            .put_workspace(&Workspace {
                id: "alpha".into(),
                name: "Alpha".into(),
            })
            .expect("workspace");
        for principal in ["designer", "reader"] {
            store.put_principal(principal).expect("principal");
        }
        for capability in [
            Capability::CatalogRead,
            Capability::LabRead,
            Capability::LabCreate,
            Capability::LabEdit,
            Capability::LabClone,
            Capability::LabValidate,
            Capability::LabPublish,
            Capability::LabMaterialize,
            Capability::LabStatus,
            Capability::LabClose,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("designer grant");
        }
        store
            .grant("alpha", "reader", Capability::LabRead)
            .expect("reader grant");
        store
    }

    #[test]
    fn discovery_is_filtered_for_two_principals() {
        let store = seeded_store();
        let designer =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("designer session");
        let reader = ProofstormMcp::new(store, "alpha", "reader").expect("reader session");
        assert_eq!(designer.tool_names().len(), 18);
        assert!(
            designer
                .tool_names()
                .contains(&"proofstorm_lab_wait".to_owned())
        );
        let backend = designer
            .proofstorm_network_capabilities()
            .expect("network backend discovery")
            .0;
        assert_eq!(backend.id, "kubernetes-network-policy");
        assert!(backend.supports(NetworkFaultFeature::Partition));
        assert!(!backend.supports(NetworkFaultFeature::Delay));
        let catalog = default_catalog();
        assert_eq!(catalog.entries.len(), 12);
        assert!(catalog.entries.iter().all(|entry| {
            entry.config_version.contains('/')
                && entry.config_schema_digest.starts_with("sha256:")
                && entry.support_lifecycle == proofstorm_core::SupportLifecycle::Preferred
                && entry.image.contains("@sha256:")
        }));
        assert_eq!(catalog.implementations.len(), 12);
        assert!(catalog.implementations.iter().all(|support| {
            support.minimum_supported == support.preferred_version
                && support.supported_versions.len() == 1
                && support
                    .supported_versions
                    .contains(&support.preferred_version)
        }));
        let cdk = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "cdk")
            .expect("CDK support contract");
        assert_eq!(cdk.config_version, "cdk-mintd/0.18/v1");
        assert_eq!(
            cdk.support_matrix.storage,
            [
                proofstorm_core::StorageBackend::Sqlite,
                proofstorm_core::StorageBackend::Postgres,
            ]
            .into()
        );
        assert_eq!(
            cdk.support_matrix.payment_methods,
            [proofstorm_core::PaymentMethod::Bolt11].into()
        );
        assert_eq!(
            cdk.support_matrix.payment_backends,
            ["cln".into(), "lnd".into()].into()
        );
        assert!(cdk.support_matrix.units.contains("sat"));
        assert_eq!(cdk.support_matrix.payment_bindings.len(), 2);
        assert!(cdk.support_matrix.payment_bindings.iter().all(|binding| {
            binding.method == proofstorm_core::PaymentMethod::Bolt11 && binding.unit == "sat"
        }));
        assert_eq!(
            cdk.support_matrix.compatible_wallet_adapters[0].implementation,
            "nutshell-wallet"
        );
        assert!(
            cdk.support_matrix.compatible_wallet_adapters[0]
                .versions
                .contains("0.20.3")
        );
        assert!(cdk.config_schema["properties"].get("mnemonic").is_none());
        assert_embedded_ldk_support(catalog);
        assert_eq!(
            cdk.config_schema["x-proofstorm-managed-settings"]["mnemonic"]["x-proofstorm-classification"],
            "runtime_policy"
        );
        assert!(
            !serde_json::to_string(&catalog)
                .expect("catalog serializes")
                .contains("abandon abandon")
        );
        assert!(
            cdk.features
                .contains(&proofstorm_core::CatalogFeature::Bolt11)
        );
        assert_eq!(cdk.compatible_dependencies[0].implementation, "lnd");
        assert_nutshell_support(catalog);
        assert_eq!(
            reader.tool_names(),
            vec![
                "proofstorm_lab_diff",
                "proofstorm_lab_read",
                "proofstorm_workspace_read",
            ]
        );
    }

    fn assert_embedded_ldk_support(catalog: &proofstorm_core::CatalogResponse) {
        let cdk_ldk = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "cdk-ldk")
            .expect("embedded LDK support contract");
        assert_eq!(cdk_ldk.config_version, "cdk-mintd-ldk/0.18/v1");
        assert_eq!(cdk_ldk.support_matrix.embedded_payment_bindings.len(), 2);
        assert!(
            cdk_ldk
                .support_matrix
                .payment_methods
                .contains(&proofstorm_core::PaymentMethod::Bolt12)
        );
        assert!(cdk_ldk.support_matrix.payment_bindings.is_empty());
    }

    #[test]
    fn catalog_summary_then_exact_detail_and_schema_is_progressive() {
        let service = ProofstormMcp::new(seeded_store(), "alpha", "designer").expect("service");
        let page = service
            .proofstorm_catalog_list(Parameters(CatalogListRequest::default()))
            .expect("catalog discovery")
            .0;
        assert_eq!(page.items.len(), 12);
        assert!(page.next_cursor.is_none());
        assert!(serialized_size(&page).expect("page size") < 8 * 1024);
        assert!(page.items.iter().all(|entry| {
            entry.config_version.contains('/')
                && entry.config_schema_digest.starts_with("sha256:")
                && entry.support_lifecycle == SupportLifecycle::Preferred
        }));
        let summary = page
            .items
            .iter()
            .find(|entry| entry.id == "nutshell")
            .expect("Nutshell summary");
        let detail = service
            .proofstorm_catalog_entry_read(Parameters(CatalogEntryRequest {
                id: summary.id.clone(),
                version: summary.version.clone(),
            }))
            .expect("Nutshell detail")
            .0;
        assert!(detail.image.contains("@sha256:"));
        assert_eq!(detail.config_schema_digest, summary.config_schema_digest);
        let schema = service
            .proofstorm_catalog_config_schema_read(Parameters(CatalogConfigSchemaRequest {
                id: summary.id.clone(),
                version: summary.version.clone(),
                pointer: "/properties".into(),
            }))
            .expect("Nutshell schema properties")
            .0;
        assert!(schema.fragment);
        assert_eq!(schema.config_schema_digest, summary.config_schema_digest);
        assert!(schema.schema.get("auth_rate_limit_per_minute").is_some());
    }

    #[test]
    fn catalog_pages_are_filtered_bounded_and_cursor_stable() {
        let service = ProofstormMcp::new(seeded_store(), "alpha", "designer").expect("service");
        let first = service
            .proofstorm_catalog_list(Parameters(CatalogListRequest {
                limit: 5,
                ..CatalogListRequest::default()
            }))
            .expect("first page")
            .0;
        assert_eq!(first.items.len(), 5);
        assert!(serialized_size(&first).expect("first size") <= MAX_AGENT_RESPONSE_BYTES);
        let cursor = first.next_cursor.clone().expect("continuation cursor");
        let second = service
            .proofstorm_catalog_list(Parameters(CatalogListRequest {
                limit: 5,
                cursor: Some(cursor.clone()),
                ..CatalogListRequest::default()
            }))
            .expect("second page")
            .0;
        assert_eq!(second.items.len(), 5);
        assert!(
            first
                .items
                .iter()
                .all(|left| second.items.iter().all(|right| {
                    (left.id.as_str(), left.version.as_str())
                        != (right.id.as_str(), right.version.as_str())
                }))
        );

        let filtered = service
            .proofstorm_catalog_list(Parameters(CatalogListRequest {
                implementations: ["nutshell".into()].into(),
                features_all: [CatalogFeature::RedisCache].into(),
                ..CatalogListRequest::default()
            }))
            .expect("filtered catalog")
            .0;
        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.items[0].id, "nutshell");
        assert!(filtered.next_cursor.is_none());

        let stale = service.proofstorm_catalog_list(Parameters(CatalogListRequest {
            implementations: ["nutshell".into()].into(),
            cursor: Some(cursor),
            ..CatalogListRequest::default()
        }));
        let Err(stale) = stale else {
            panic!("cursor must be bound to filters");
        };
        assert_eq!(
            stale.data.expect("cursor error data")["code"],
            "catalog_cursor_invalid"
        );
    }

    #[tokio::test]
    async fn component_logs_requires_its_capability_and_bounded_lines() {
        let store = seeded_store();
        let unauthorized = ProofstormMcp::new(store.clone(), "alpha", "designer")
            .expect("session without component.logs");
        let request = |tail_lines: u32| ComponentLogsRequest {
            instance_id: "instance-one".into(),
            experiment_id: "experiment-one".into(),
            lease_id: "lease-one".into(),
            operation_id: "operation-logs".into(),
            component: "chain".into(),
            tail_lines,
            idempotency_key: "logs-one".into(),
        };
        let Err(denied) = unauthorized
            .proofstorm_component_logs(Parameters(request(100)))
            .await
        else {
            panic!("component.logs is a separate authority");
        };
        assert_eq!(denied.data.expect("denial data")["code"], "access_denied");

        store
            .grant("alpha", "designer", Capability::ComponentLogs)
            .expect("grant component.logs");
        let authorized =
            ProofstormMcp::new(store, "alpha", "designer").expect("session with component.logs");
        for lines in [0, 2_001] {
            let Err(rejected) = authorized
                .proofstorm_component_logs(Parameters(request(lines)))
                .await
            else {
                panic!("tail_lines {lines} must be rejected");
            };
            assert_eq!(
                rejected.data.expect("bounds data")["code"],
                "invalid_operation",
                "tail_lines {lines} is out of bounds"
            );
        }
    }

    #[tokio::test]
    async fn authentication_conformance_is_a_separate_capability() {
        let store = seeded_store();
        let unauthorized = ProofstormMcp::new(store.clone(), "alpha", "designer")
            .expect("session without authentication.test");
        let Err(denied) = unauthorized
            .proofstorm_authentication_conformance(Parameters(AuthenticationConformanceRequest {
                instance_id: "instance-one".into(),
                experiment_id: "experiment-one".into(),
                lease_id: "lease-one".into(),
                operation_id: "operation-auth".into(),
                mint: "mint".into(),
                identity_provider: "identity".into(),
                idempotency_key: "auth-one".into(),
            }))
            .await
        else {
            panic!("authentication conformance must require its own capability");
        };
        assert_eq!(denied.data.expect("denial data")["code"], "access_denied");
        assert!(
            !unauthorized
                .tool_names()
                .contains(&"proofstorm_authentication_conformance".to_owned())
        );
        assert!(
            !unauthorized
                .tool_names()
                .contains(&"proofstorm_authentication_protected_spend".to_owned())
        );
        assert!(
            !unauthorized
                .tool_names()
                .contains(&"proofstorm_authentication_replay".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::AuthenticationTest)
            .expect("grant authentication.test");
        let authorized =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("authorized session");
        assert!(
            authorized
                .tool_names()
                .contains(&"proofstorm_authentication_conformance".to_owned())
        );
        assert!(
            authorized
                .tool_names()
                .contains(&"proofstorm_authentication_protected_spend".to_owned())
        );
        assert!(
            !authorized
                .tool_names()
                .contains(&"proofstorm_authentication_replay".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::ArtifactRead)
            .expect("grant artifact.read");
        let replay_authorized =
            ProofstormMcp::new(store, "alpha", "designer").expect("replay-authorized session");
        assert!(
            replay_authorized
                .tool_names()
                .contains(&"proofstorm_authentication_replay".to_owned())
        );
    }

    #[test]
    fn draft_mutations_return_compact_receipts() {
        let service = ProofstormMcp::new(seeded_store(), "alpha", "designer").expect("service");
        let receipt = service
            .proofstorm_lab_create(Parameters(CreateDraftRequest {
                draft_id: "compact-draft".into(),
                lab: lab("compact-draft"),
                idempotency_key: "create-compact-draft".into(),
            }))
            .expect("create draft")
            .0;
        assert_eq!(receipt.draft_id, "compact-draft");
        assert_eq!(receipt.version, 1);
        assert_eq!(receipt.component_count, 0);
        assert_eq!(receipt.link_count, 0);
        assert!(receipt.valid);
        assert_eq!(receipt.changed_paths, ["/"]);
        let encoded = serde_json::to_string(&receipt).expect("serialize receipt");
        assert!(!encoded.contains("api_version"));
        assert!(serialized_size(&receipt).expect("receipt size") < 1024);

        let draft = service
            .proofstorm_lab_read(Parameters(ReadDraftRequest {
                draft_id: "compact-draft".into(),
            }))
            .expect("explicit full draft")
            .0;
        assert_eq!(draft.lab.name, "compact-draft");

        let published = service
            .proofstorm_lab_publish(Parameters(PublishDraftRequest {
                draft_id: "compact-draft".into(),
                expected_version: 1,
                idempotency_key: "publish-compact-draft".into(),
                include_revision: false,
            }))
            .expect("publish receipt")
            .0;
        assert!(!published.revision_included);
        assert!(published.lab.is_none());
        assert!(published.lock.is_none());
        assert!(published.digest.starts_with("sha256:"));
        assert!(published.lock_digest.starts_with("sha256:"));
        assert!(serialized_size(&published).expect("publish size") < 1024);
    }

    #[test]
    fn fully_authorized_tool_discovery_has_a_regression_budget() {
        let store = seeded_store();
        let capabilities = tool_capabilities()
            .into_iter()
            .flat_map(|(_, required)| required.iter().copied())
            .collect::<BTreeSet<_>>();
        for capability in capabilities {
            store
                .grant("alpha", "designer", capability)
                .expect("full discovery grant");
        }
        let service = ProofstormMcp::new(store, "alpha", "designer").expect("service");
        let encoded = serde_json::to_vec(&service.tool_router.list_all()).expect("tool discovery");
        eprintln!(
            "all tool discovery: {} tools, {} bytes",
            service.tool_names().len(),
            encoded.len()
        );
        assert_eq!(service.tool_names().len(), 66);
        assert!(
            service
                .tool_names()
                .contains(&"proofstorm_wallet_quote_claim".to_owned()),
            "recipient quote claiming is a first-class recovery operation"
        );
        assert!(
            service
                .tool_names()
                .contains(&"proofstorm_component_logs".to_owned()),
            "reading a component log is a first-class runtime observation"
        );
        assert!(
            encoded.len() < 190 * 1024,
            "fully authorized tool discovery is {} bytes",
            encoded.len()
        );
        for (toolset, maximum) in [
            (ProofstormToolset::Design, 100 * 1024),
            (ProofstormToolset::Runtime, 160 * 1024),
            (ProofstormToolset::Evidence, 100 * 1024),
        ] {
            let focused = service.clone().with_toolset(toolset);
            let tools = focused.tool_names();
            let size = serde_json::to_vec(&focused.tool_router.list_all())
                .expect("focused tool discovery")
                .len();
            eprintln!(
                "{toolset:?} tool discovery: {} tools, {size} bytes",
                tools.len()
            );
            assert!(size < maximum, "{toolset:?} discovery is {size} bytes");
            assert!(tools.contains(&"proofstorm_catalog_list".to_owned()));
        }
        let design = service.clone().with_toolset(ProofstormToolset::Design);
        assert!(
            !design
                .tool_names()
                .contains(&"proofstorm_component_exec".to_owned())
        );
        let evidence = service.with_toolset(ProofstormToolset::Evidence);
        assert!(
            !evidence
                .tool_names()
                .contains(&"proofstorm_wallet_pay".to_owned())
        );
    }

    fn assert_nutshell_support(catalog: &proofstorm_core::CatalogResponse) {
        let nutshell = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "nutshell")
            .expect("Nutshell mint support contract");
        assert_eq!(nutshell.config_version, "nutshell-mint/0.20/v1");
        assert_eq!(
            nutshell.support_matrix.payment_backends,
            ["cln".into(), "lnd".into()].into()
        );
        assert!(
            nutshell.config_schema["x-proofstorm-managed-settings"]
                .get("mint_private_key")
                .is_some()
        );
        assert!(
            nutshell
                .features
                .contains(&proofstorm_core::CatalogFeature::RedisCache)
        );
        assert!(
            nutshell
                .features
                .contains(&proofstorm_core::CatalogFeature::ClearAuth)
        );
        assert!(
            nutshell
                .features
                .contains(&proofstorm_core::CatalogFeature::BlindAuth)
        );
        assert!(nutshell.compatible_dependencies.iter().any(|dependency| {
            dependency.link_kind == proofstorm_core::LinkKind::DatabaseBackend
                && dependency.implementation == "redis"
                && dependency.versions.contains("8.10.1")
        }));
    }

    #[test]
    fn native_exec_is_hidden_without_its_distinct_capability() {
        let store = seeded_store();
        let restricted =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("restricted session");
        assert!(
            !restricted
                .tool_names()
                .contains(&"proofstorm_component_exec".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::ComponentExec)
            .expect("exec grant");
        let authorized = ProofstormMcp::new(store, "alpha", "designer").expect("exec session");
        assert!(
            authorized
                .tool_names()
                .contains(&"proofstorm_component_exec".to_owned())
        );
    }

    #[test]
    fn wait_contracts_are_bounded_terminal_and_capability_filtered() {
        assert!(validate_wait_timeout(1).is_ok());
        assert!(validate_wait_timeout(120).is_ok());
        for timeout in [0, 121] {
            let error = validate_wait_timeout(timeout).expect_err("timeout must refuse");
            assert_eq!(
                error.data.expect("structured wait error")["code"],
                "wait_timeout_invalid"
            );
        }
        assert!(!lab_wait_terminal(InstancePhase::Ready));
        assert!(lab_wait_terminal(InstancePhase::Closed));
        assert!(lab_wait_terminal(InstancePhase::CleanupBlocked));
        assert!(!operation_terminal(OperationPhase::Running));
        assert!(operation_terminal(OperationPhase::Succeeded));
        assert!(operation_terminal(OperationPhase::Failed));
        assert!(operation_terminal(OperationPhase::Cancelled));

        let store = seeded_store();
        let restricted =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("restricted session");
        assert!(
            !restricted
                .tool_names()
                .contains(&"proofstorm_operation_wait".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::ArtifactRead)
            .expect("artifact grant");
        let authorized = ProofstormMcp::new(store, "alpha", "designer").expect("wait session");
        assert!(
            authorized
                .tool_names()
                .contains(&"proofstorm_operation_wait".to_owned())
        );
    }

    #[test]
    fn lab_status_receipt_and_page_cursors_are_compact_and_snapshot_bound() {
        let status = LabInstanceStatus {
            instance: LabInstance {
                id: "instance-one".into(),
                workspace_id: "alpha".into(),
                revision_digest: "sha256:revision".into(),
                lock_digest: "sha256:lock".into(),
                instance_key: "secret-routing-key".into(),
                resource_name: "proofstorm-resource".into(),
            },
            phase: InstancePhase::Ready,
            instance_namespace: "proofstorm-instance-one".into(),
            components: vec![],
            inventory: vec![InventoryEntry {
                api_version: "v1".into(),
                kind: "Service".into(),
                namespace: "proofstorm-instance-one".into(),
                name: "service-one".into(),
            }],
            teardown_receipt: None,
            message: None,
        };
        let receipt = compact_lab_status(status);
        assert_eq!(receipt.total_components, 0);
        assert_eq!(receipt.inventory_count, 1);
        assert!(receipt.inventory_digest.starts_with("sha256:"));
        let encoded = serde_json::to_string(&receipt).expect("status receipt");
        assert!(!encoded.contains("\"components\":["));
        assert!(!encoded.contains("inventory\":"));
        assert!(!encoded.contains("secret-routing-key"));
        assert!(serialized_size(&receipt).expect("status size") < 1024);

        let items = vec!["alpha", "beta", "gamma"];
        let snapshot = digest_json(&items);
        let cursor = status_cursor("component", "instance-one", &snapshot, "alpha");
        assert_eq!(
            status_page_start(Some(&cursor), &items, |item| status_cursor(
                "component",
                "instance-one",
                &snapshot,
                item
            ))
            .expect("valid cursor"),
            1
        );
        let stale = status_page_start(Some(&cursor), &items, |item| {
            status_cursor("component", "instance-one", "sha256:new", item)
        })
        .expect_err("stale cursor");
        assert_eq!(
            stale.data.expect("cursor data")["code"],
            "status_cursor_invalid"
        );
    }

    #[test]
    fn handler_rechecks_authority_after_discovery() {
        let store = seeded_store();
        let session = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            session
                .tool_names()
                .contains(&"proofstorm_lab_create".to_owned())
        );
        store
            .revoke("alpha", "designer", Capability::LabCreate)
            .expect("revoke");
        let result = session.proofstorm_lab_create(Parameters(CreateDraftRequest {
            draft_id: "refused".into(),
            lab: lab("refused"),
            idempotency_key: "create-refused".into(),
        }));
        let Err(error) = result else {
            panic!("handler must refuse stale discovery authority");
        };
        assert_eq!(
            error.data.expect("structured error")["code"],
            "access_denied"
        );
    }

    #[test]
    fn operation_discovery_requires_the_complete_capability_union() {
        let store = seeded_store();
        for capability in [
            Capability::ChainMine,
            Capability::WalletFund,
            Capability::PeerConnect,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("partial operation grant");
        }
        let partial = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            !partial
                .tool_names()
                .contains(&"proofstorm_liquidity_bootstrap".to_owned())
        );
        assert!(
            partial
                .tool_names()
                .contains(&"proofstorm_peer_connect".to_owned())
        );
        assert!(
            !partial
                .tool_names()
                .contains(&"proofstorm_channel_open".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::ChannelOpen)
            .expect("complete operation grant");
        let complete = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            complete
                .tool_names()
                .contains(&"proofstorm_liquidity_bootstrap".to_owned())
        );
        assert!(
            complete
                .tool_names()
                .contains(&"proofstorm_channel_open".to_owned())
        );
        assert!(
            !complete
                .tool_names()
                .contains(&"proofstorm_node_restart".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::NodeControl)
            .expect("node control grant");
        let node_control = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        for tool in [
            "proofstorm_node_start",
            "proofstorm_node_stop",
            "proofstorm_node_restart",
        ] {
            assert!(node_control.tool_names().contains(&tool.to_owned()));
        }
        for capability in [
            Capability::PeerDisconnect,
            Capability::ChannelClose,
            Capability::ChannelForceClose,
            Capability::ChannelRebalance,
            Capability::NetworkDelay,
            Capability::NetworkDrop,
            Capability::NetworkPartition,
            Capability::NetworkHeal,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("teardown grant");
        }
        let teardown = ProofstormMcp::new(store, "alpha", "designer").expect("session");
        for tool in [
            "proofstorm_peer_disconnect",
            "proofstorm_channel_close",
            "proofstorm_channel_force_close",
            "proofstorm_channel_rebalance",
            "proofstorm_network_delay",
            "proofstorm_network_loss",
            "proofstorm_network_partition",
            "proofstorm_network_heal",
        ] {
            assert!(teardown.tool_names().contains(&tool.to_owned()));
        }
    }

    #[test]
    fn unsupported_network_shaping_is_bounded_and_fails_before_admission() {
        let store = seeded_store();
        for capability in [Capability::NetworkDelay, Capability::NetworkDrop] {
            store
                .grant("alpha", "designer", capability)
                .expect("network shaping grant");
        }
        let session = ProofstormMcp::new(store, "alpha", "designer").expect("session");
        let delay_result = session.proofstorm_network_delay(Parameters(NetworkDelayRequest {
            instance_id: "missing-instance".into(),
            experiment_id: "missing-experiment".into(),
            lease_id: "missing-lease".into(),
            operation_id: "delay".into(),
            from_component: "wallet".into(),
            to_component: "mint".into(),
            direction: NetworkFaultDirection::FromTo,
            delay_ms: 100,
            jitter_ms: 10,
            idempotency_key: "delay-key".into(),
        }));
        let Err(delay_error) = delay_result else {
            panic!("network-policy backend must refuse delay");
        };
        assert_eq!(
            delay_error.data.expect("structured delay error")["code"],
            "network_fault_unsupported"
        );

        let loss_result = session.proofstorm_network_loss(Parameters(NetworkLossRequest {
            instance_id: "missing-instance".into(),
            experiment_id: "missing-experiment".into(),
            lease_id: "missing-lease".into(),
            operation_id: "loss".into(),
            from_component: "wallet".into(),
            to_component: "mint".into(),
            direction: NetworkFaultDirection::Bidirectional,
            loss_basis_points: 250,
            idempotency_key: "loss-key".into(),
        }));
        let Err(loss_error) = loss_result else {
            panic!("network-policy backend must refuse loss");
        };
        assert_eq!(
            loss_error.data.expect("structured loss error")["code"],
            "network_fault_unsupported"
        );

        assert!(
            validate_network_delay_bounds(&NetworkDelayRequest {
                instance_id: String::new(),
                experiment_id: String::new(),
                lease_id: String::new(),
                operation_id: String::new(),
                from_component: "a".into(),
                to_component: "b".into(),
                direction: NetworkFaultDirection::FromTo,
                delay_ms: MAX_NETWORK_DELAY_MS + 1,
                jitter_ms: 0,
                idempotency_key: String::new(),
            })
            .is_err()
        );
        assert!(
            validate_network_loss_bounds(&NetworkLossRequest {
                instance_id: String::new(),
                experiment_id: String::new(),
                lease_id: String::new(),
                operation_id: String::new(),
                from_component: "a".into(),
                to_component: "b".into(),
                direction: NetworkFaultDirection::FromTo,
                loss_basis_points: 0,
                idempotency_key: String::new(),
            })
            .is_err()
        );
    }

    #[test]
    fn composer_discovery_requires_edit_and_topology_authority() {
        let store = seeded_store();
        let partial = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            !partial
                .tool_names()
                .contains(&"proofstorm_component_add".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::TopologyMutate)
            .expect("topology grant");
        let complete = ProofstormMcp::new(store, "alpha", "designer").expect("session");
        for tool in [
            "proofstorm_component_add",
            "proofstorm_component_update",
            "proofstorm_component_remove",
            "proofstorm_link_add",
            "proofstorm_link_remove",
        ] {
            assert!(complete.tool_names().contains(&tool.to_owned()));
        }
    }

    #[test]
    fn reachability_oracle_is_capability_filtered_and_bounded() {
        let store = seeded_store();
        let denied =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("denied session");
        assert!(
            !denied
                .tool_names()
                .contains(&"proofstorm_reachability_oracle".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::OracleRun)
            .expect("oracle grant");
        let allowed = ProofstormMcp::new(store, "alpha", "designer").expect("allowed session");
        assert!(
            allowed
                .tool_names()
                .contains(&"proofstorm_reachability_oracle".to_owned())
        );
        assert!(
            validate_reachability_oracle_bounds(&ReachabilityOracleRequest {
                instance_id: String::new(),
                experiment_id: String::new(),
                lease_id: String::new(),
                operation_id: String::new(),
                from_component: "wallet".into(),
                to_component: "mint".into(),
                service: "http".into(),
                timeout_seconds: 5,
                attempts: 5,
                idempotency_key: String::new(),
            })
            .is_ok()
        );
        assert!(
            validate_reachability_oracle_bounds(&ReachabilityOracleRequest {
                instance_id: String::new(),
                experiment_id: String::new(),
                lease_id: String::new(),
                operation_id: String::new(),
                from_component: "wallet".into(),
                to_component: "wallet".into(),
                service: "http".into(),
                timeout_seconds: 1,
                attempts: 1,
                idempotency_key: String::new(),
            })
            .is_err()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the evidence fixture proves lifecycle closure, restart independence, and sanitization together"
    )]
    fn evidence_export_is_deterministic_bounded_and_cluster_independent() {
        let store = seeded_store();
        for capability in [
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::ExperimentClose,
            Capability::LeaseAcquire,
            Capability::LeaseRelease,
            Capability::OracleRun,
            Capability::ArtifactRead,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("evidence grant");
        }
        store
            .create_draft(
                "alpha",
                "designer",
                "evidence-lab",
                &lab("evidence-lab"),
                "create-evidence-lab",
            )
            .expect("draft");
        let revision = store
            .publish(
                "alpha",
                "designer",
                "evidence-lab",
                1,
                "publish-evidence-lab",
            )
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "evidence-instance",
                &revision.digest,
                "materialize-evidence-lab",
            )
            .expect("instance");
        store
            .create_experiment(
                "alpha",
                "designer",
                "evidence-experiment",
                "evidence-instance",
                "create-evidence-experiment",
            )
            .expect("experiment");
        store
            .acquire_lease(
                "alpha",
                "designer",
                "evidence-experiment",
                "evidence-lease",
                300,
                1,
                "acquire-evidence-lease",
            )
            .expect("lease");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "evidence-instance",
                "evidence-experiment",
                "evidence-lease",
                "evidence-oracle",
                OperationKind::ConservationOracle,
                &serde_json::json!({"expected_sat": 100, "tolerance_sat": 0}),
                "create-evidence-oracle",
                Capability::OracleRun,
            )
            .expect("operation");
        store
            .record_operation_result(
                "alpha",
                &operation.id,
                OperationPhase::Succeeded,
                serde_json::json!({"expected_sat": 100, "actual_sat": 100, "conserved": true}),
            )
            .expect("artifact");

        let active = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        let Err(error) = active.proofstorm_artifact_export(Parameters(ArtifactExportRequest {
            experiment_id: "evidence-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: vec![],
            include_content: false,
        })) else {
            panic!("active experiment evidence must refuse");
        };
        assert_eq!(
            error.data.expect("structured error")["code"],
            "evidence_experiment_active"
        );

        store
            .release_lease(
                "alpha",
                "designer",
                "evidence-lease",
                "release-evidence-lease",
            )
            .expect("release");
        store
            .close_experiment(
                "alpha",
                "designer",
                "evidence-experiment",
                "close-evidence-experiment",
            )
            .expect("close");
        let restarted = ProofstormMcp::new(store, "alpha", "designer").expect("restart session");
        let request = ArtifactExportRequest {
            experiment_id: "evidence-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: vec![],
            include_content: true,
        };
        let first = restarted
            .proofstorm_artifact_export(Parameters(request.clone()))
            .expect("first export")
            .0;
        let second = restarted
            .proofstorm_artifact_export(Parameters(request))
            .expect("second export")
            .0;
        assert_eq!(first, second);
        assert!(first.content_included);
        let content = serde_json::from_value::<EvidenceBundleContent>(
            first.content.clone().expect("explicit bulk content"),
        )
        .expect("typed evidence content");
        assert_eq!(first.digest, proofstorm_core::digest_json(&content));
        assert_eq!(content.journal.len(), 1);
        assert_eq!(content.artifacts.len(), 1);
        assert_eq!(content.revision.digest, revision.digest);
        assert_eq!(content.instance.lock_digest, content.revision.lock.digest);
        assert!(first.byte_length as usize <= MAX_EVIDENCE_BUNDLE_BYTES);
        let encoded = serde_json::to_string(&first).expect("serialize evidence");
        assert!(!encoded.contains("resource_name"));
        assert!(!encoded.contains("instance_key"));
        assert!(!encoded.contains("kubernetes"));

        let journal = restarted
            .proofstorm_action_list(Parameters(ActionListRequest {
                experiment_id: "evidence-experiment".into(),
                after_sequence: 0,
                limit: 100,
            }))
            .expect("summary journal")
            .0;
        assert_eq!(journal.actions.len(), 1);
        assert!(journal.next_after_sequence.is_none());
        assert_eq!(journal.actions[0].sequence, 1);
        assert_eq!(
            journal.actions[0]
                .artifact
                .as_ref()
                .expect("artifact descriptor")
                .digest,
            content.artifacts[0].artifact.digest
        );
        let encoded_journal = serde_json::to_string(&journal).expect("serialize journal");
        assert!(!encoded_journal.contains("expected_sat"));
        assert!(!encoded_journal.contains("resource_name"));
        assert!(serialized_size(&journal).expect("journal size") <= MAX_AGENT_RESPONSE_BYTES);

        let manifest = restarted
            .proofstorm_artifact_export(Parameters(ArtifactExportRequest {
                experiment_id: "evidence-experiment".into(),
                include_oracle_artifacts: true,
                artifact_operation_ids: vec![],
                include_content: false,
            }))
            .expect("compact evidence manifest")
            .0;
        assert_eq!(manifest.digest, first.digest);
        assert_eq!(manifest.byte_length, first.byte_length);
        assert!(!manifest.content_included);
        assert!(manifest.content.is_none());
        assert!(
            manifest
                .resource_uri
                .starts_with("proofstorm://evidence/evidence-experiment/sha256:")
        );
        let (resource_request, resource_digest) =
            parse_evidence_resource_uri(&manifest.resource_uri).expect("resource URI");
        assert_eq!(resource_digest, manifest.digest);
        assert_eq!(resource_request.experiment_id, "evidence-experiment");
        assert!(resource_request.include_oracle_artifacts);
        assert!(resource_request.artifact_operation_ids.is_empty());
        let resource_bundle = restarted
            .build_evidence_bundle(&resource_request)
            .expect("resource bundle");
        assert_eq!(resource_bundle.digest, resource_digest);
        assert!(serialized_size(&manifest).expect("manifest size") < 1024);

        let revision_section = restarted
            .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
                experiment_id: "evidence-experiment".into(),
                include_oracle_artifacts: true,
                artifact_operation_ids: vec![],
                section: EvidenceSection::Revision,
                pointer: "/digest".into(),
                operation_id: None,
                after_sequence: 0,
                limit: 20,
            }))
            .expect("revision section")
            .0;
        assert_eq!(revision_section.evidence_digest, manifest.digest);
        assert_eq!(revision_section.data, revision.digest);
        assert!(serialized_size(&revision_section).expect("section size") < 1024);

        let journal_section = restarted
            .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
                experiment_id: "evidence-experiment".into(),
                include_oracle_artifacts: true,
                artifact_operation_ids: vec![],
                section: EvidenceSection::Journal,
                pointer: String::new(),
                operation_id: None,
                after_sequence: 0,
                limit: 1,
            }))
            .expect("journal section")
            .0;
        assert_eq!(journal_section.data.as_array().map(Vec::len), Some(1));
        assert!(journal_section.next_after_sequence.is_none());
    }

    #[tokio::test]
    async fn wallet_tools_are_independently_capability_filtered() {
        let store = seeded_store();
        store
            .grant("alpha", "designer", Capability::WalletCreate)
            .expect("create grant");
        let create = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            create
                .tool_names()
                .contains(&"proofstorm_wallet_initialize".to_owned())
        );
        assert!(
            !create
                .tool_names()
                .contains(&"proofstorm_wallet_balance".to_owned())
        );
        assert!(
            !create
                .tool_names()
                .contains(&"proofstorm_wallet_fund".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::WalletControl)
            .expect("control grant");
        store
            .grant("alpha", "designer", Capability::WalletFund)
            .expect("fund grant");
        let complete = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            complete
                .tool_names()
                .contains(&"proofstorm_wallet_balance".to_owned())
        );
        assert!(
            complete
                .tool_names()
                .contains(&"proofstorm_wallet_fund".to_owned())
        );
        assert!(
            complete
                .tool_names()
                .contains(&"proofstorm_wallet_invoice".to_owned())
        );
        assert!(
            complete
                .tool_names()
                .contains(&"proofstorm_wallet_quote_claim".to_owned())
        );
        assert!(
            !complete
                .tool_names()
                .contains(&"proofstorm_wallet_pay".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::ArtifactRead)
            .expect("artifact grant");
        let status = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        assert!(
            status
                .tool_names()
                .contains(&"proofstorm_wallet_quote_status".to_owned())
        );
        assert!(
            status
                .tool_names()
                .contains(&"proofstorm_wallet_pay".to_owned())
        );
        let Err(missing_quote) = status
            .proofstorm_wallet_quote_status(Parameters(WalletQuoteRequest {
                instance_id: "missing-instance".into(),
                wallet: "missing-wallet".into(),
                mint: "missing-mint".into(),
                direction: WalletQuoteDirection::Receive,
                quote_id: "missing-quote".into(),
            }))
            .await
        else {
            panic!("missing quote must refuse");
        };
        assert_eq!(
            missing_quote.data.expect("structured quote error")["code"],
            "not_found"
        );
        assert!(
            !status
                .tool_names()
                .contains(&"proofstorm_wallet_quote_list".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::ExperimentRead)
            .expect("experiment read grant");
        let readable = ProofstormMcp::new(store, "alpha", "designer").expect("session");
        assert!(
            readable
                .tool_names()
                .contains(&"proofstorm_wallet_quote_list".to_owned())
        );
    }

    #[test]
    fn wallet_quote_cursor_is_snapshot_and_experiment_bound() {
        let cursor = encode_quote_cursor("experiment-one", 42, 17);
        assert_eq!(
            decode_quote_cursor(&cursor, "experiment-one").expect("valid cursor"),
            (42, 17)
        );
        let error = decode_quote_cursor(&cursor, "experiment-two")
            .expect_err("cursor cannot cross experiments");
        assert_eq!(
            error.data.expect("structured cursor error")["code"],
            "invalid_wallet_quote_cursor"
        );
    }
}
