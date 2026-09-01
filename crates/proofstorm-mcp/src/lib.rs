use std::collections::BTreeSet;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Patch, PatchParams},
};
use proofstorm_core::{
    Capability, CatalogResponse, ComponentKind, ComponentSpec, DraftMutation, EVIDENCE_API_VERSION,
    EvidenceAction, EvidenceArtifact, EvidenceBundle, EvidenceBundleContent, EvidenceInstance,
    Experiment, ExperimentLease, ExperimentPhase, InstancePhase, LabInstance, LabInstanceStatus,
    LabOperation, LabSpec, LinkSpec, MAX_NETWORK_DELAY_MS, MAX_NETWORK_JITTER_MS,
    MAX_NETWORK_LOSS_BASIS_POINTS, NetworkFaultBackend, NetworkFaultDirection, NetworkFaultFeature,
    OperationArtifact, OperationKind, OperationPhase, PublishedRevision,
    TeardownReceipt as CoreTeardownReceipt, ValidateLabRequest, ValidationReport, WalletQuote,
    WalletQuoteDirection, WalletQuotePhase, default_catalog, network_policy_fault_backend,
    validate_lab,
};
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionPhase, BootstrapLiquidityAction, ChannelCloseAction,
    ChannelOpenAction, ChannelRebalanceAction, ConservationOracleAction, LabAction, LabPhase,
    NativeExecAction, NetworkHealAction, NetworkPartitionAction, NodeControlAction,
    PeerConnectAction, PeerDisconnectAction, ProofstormLab, ProofstormLabAction,
    ProofstormLabActionSpec, ProofstormLabSpec, ReachabilityOracleAction, WalletBalanceAction,
    WalletFundAction, WalletInitializeAction, WalletInvoiceAction, WalletPayAction,
    WalletRoundTripAction, component_ports,
};
use proofstorm_store::{Draft, DraftDiff, Store, StoreError, Workspace};
use rmcp::{
    ErrorData, Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
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
pub struct EditDraftRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub lab: LabSpec,
    pub idempotency_key: String,
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
    pub quote_id: String,
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
    pub quote_id: String,
    pub wallet: String,
    pub mint: String,
    pub idempotency_key: String,
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
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionListResponse {
    pub actions: Vec<LabOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_sequence: Option<u64>,
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
}

const fn default_true() -> bool {
    true
}

const MAX_EVIDENCE_ACTIONS: u32 = 100;
const MAX_EXPLICIT_EVIDENCE_ARTIFACTS: usize = 16;
const MAX_EVIDENCE_ARTIFACTS: usize = 32;
const MAX_EVIDENCE_BUNDLE_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteRequest {
    pub quote_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteListRequest {
    pub experiment_id: String,
    #[serde(default)]
    pub after_quote_id: Option<String>,
    #[serde(default = "default_quote_list_limit")]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteListResponse {
    pub quotes: Vec<WalletQuote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_quote_id: Option<String>,
}

const fn default_quote_list_limit() -> u32 {
    50
}

const fn default_action_list_limit() -> u32 {
    50
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

    #[tool(description = "List the installed Proofstorm component catalog")]
    fn proofstorm_catalog_list(&self) -> Result<Json<CatalogResponse>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        Ok(Json(default_catalog()))
    }

    #[tool(
        description = "Discover the installed network-fault backend, features, directions, and bounds"
    )]
    fn proofstorm_network_capabilities(&self) -> Result<Json<NetworkFaultBackend>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        Ok(Json(network_policy_fault_backend()))
    }

    #[tool(description = "Create a versioned lab draft in the selected workspace")]
    fn proofstorm_lab_create(
        &self,
        Parameters(request): Parameters<CreateDraftRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::LabCreate)?;
        self.store
            .create_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                &request.lab,
                &request.idempotency_key,
            )
            .map(Json)
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

    #[tool(description = "Replace a lab draft using optimistic version and idempotency checks")]
    fn proofstorm_lab_edit(
        &self,
        Parameters(request): Parameters<EditDraftRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
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
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Add an installed, versioned component to a lab draft")]
    fn proofstorm_component_add(
        &self,
        Parameters(request): Parameters<MutateComponentRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
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
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Update an existing logical component in a lab draft")]
    fn proofstorm_component_update(
        &self,
        Parameters(request): Parameters<MutateComponentRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
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
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Remove an unlinked component from a lab draft")]
    fn proofstorm_component_remove(
        &self,
        Parameters(request): Parameters<RemoveComponentRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
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
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Add a typed relationship between two draft components")]
    fn proofstorm_link_add(
        &self,
        Parameters(request): Parameters<MutateLinkRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::AddLink { link: request.link },
                &request.idempotency_key,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Remove one exact typed relationship from a lab draft")]
    fn proofstorm_link_remove(
        &self,
        Parameters(request): Parameters<MutateLinkRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::RemoveLink { link: request.link },
                &request.idempotency_key,
            )
            .map(Json)
            .map_err(store_error)
    }

    #[tool(description = "Clone a lab draft within the selected workspace")]
    fn proofstorm_lab_clone(
        &self,
        Parameters(request): Parameters<CloneDraftRequest>,
    ) -> Result<Json<Draft>, ErrorData> {
        self.authorize(Capability::LabClone)?;
        self.store
            .clone_draft(
                &self.workspace,
                &self.principal,
                &request.source_draft_id,
                &request.target_draft_id,
                &request.idempotency_key,
            )
            .map(Json)
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

    #[tool(description = "Publish an immutable lab revision with a deterministic catalog lock")]
    fn proofstorm_lab_publish(
        &self,
        Parameters(request): Parameters<PublishDraftRequest>,
    ) -> Result<Json<PublishedRevision>, ErrorData> {
        self.authorize(Capability::LabPublish)?;
        self.store
            .publish(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &request.idempotency_key,
            )
            .map(Json)
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

    #[tool(description = "Read sanitized readiness, topology, and inventory for a lab instance")]
    async fn proofstorm_lab_status(
        &self,
        Parameters(request): Parameters<InstanceRequest>,
    ) -> Result<Json<LabInstanceStatus>, ErrorData> {
        self.authorize(Capability::LabStatus)?;
        let instance = self
            .store
            .instance(&self.workspace, &self.principal, &request.instance_id)
            .map_err(store_error)?;
        self.runtime()?.status(instance).await.map(Json)
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
            let status = self
                .proofstorm_lab_status(Parameters(InstanceRequest {
                    instance_id: request.instance_id.clone(),
                }))
                .await?
                .0;
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
        self.runtime()?.close(instance).await.map(Json)
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
            return Err(ErrorData::invalid_request(
                format!(
                    "lab instance {:?} is not ready for a lease",
                    experiment.instance_id
                ),
                Some(serde_json::json!({"code": "instance_not_ready"})),
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
        description = "Run an unrestricted non-interactive shell program in a lab component's exact pinned image, with its component-local data and native CLI available. target_component optionally selects a distinct lab service and defaults to component; Proofstorm exposes generic PROOFSTORM_TARGET_* metadata plus native endpoint variables such as BITCOIN_RPC_HOST and BITCOIN_RPC_PORT without interpreting the command. The workload has no Kubernetes token, host access, or cross-lab credentials; combined output and exit code are journaled as a bounded experiment artifact"
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
        let quote_key = format!(
            "quote-{}",
            &proofstorm_core::digest_json(&(
                &self.workspace,
                &self.principal,
                &request.quote_id,
                &request.idempotency_key,
            ))[7..26]
        );
        let quote = match self.store.create_wallet_quote(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.quote_id,
            &request.wallet,
            &request.mint,
            WalletQuoteDirection::Receive,
            request.amount_sat,
            request.timeout_seconds,
            &quote_key,
        ) {
            Ok(quote) if quote.phase == WalletQuotePhase::Requested => quote,
            Ok(_) => {
                let failed = self
                    .store
                    .record_operation_result(
                        &self.workspace,
                        &operation.id,
                        OperationPhase::Failed,
                        serde_json::json!({"code": "quote_not_requestable"}),
                    )
                    .map_err(store_error)?;
                return Ok(Json(failed));
            }
            Err(error) => {
                self.store
                    .record_operation_result(
                        &self.workspace,
                        &operation.id,
                        OperationPhase::Failed,
                        serde_json::json!({"code": "quote_admission_failed"}),
                    )
                    .map_err(store_error)?;
                return Err(store_error(error));
            }
        };
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletInvoice(WalletInvoiceAction {
                quote_id: request.quote_id.clone(),
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                amount_sat: request.amount_sat,
                timeout_seconds: request.timeout_seconds,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .transition_wallet_quote(
                &self.workspace,
                &quote.id,
                WalletQuotePhase::Requested,
                WalletQuotePhase::Ready,
                Some(&operation.id),
                None,
            )
            .map_err(store_error)?;
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
        let quote = self
            .store
            .wallet_quote(&self.workspace, &self.principal, &request.quote_id)
            .map_err(store_error)?;
        if quote.direction != WalletQuoteDirection::Receive
            || quote.instance_id != request.instance_id
            || quote.experiment_id != request.experiment_id
            || quote.lease_id != request.lease_id
        {
            return Err(invalid_operation(
                "quote must be a receive quote in the same instance, experiment, and lease",
            ));
        }
        if quote.phase != WalletQuotePhase::Ready {
            match self
                .store
                .operation(&self.workspace, &self.principal, &request.operation_id)
            {
                Ok(existing)
                    if existing.kind == OperationKind::WalletPay
                        && existing.request["quote_id"] == request.quote_id
                        && existing.request["wallet"] == request.wallet
                        && existing.request["mint"] == request.mint =>
                {
                    return Ok(Json(existing));
                }
                Ok(_) => {
                    return Err(invalid_operation(
                        "operation identity does not match the existing quote payment",
                    ));
                }
                Err(StoreError::NotFound { .. }) => {
                    return Err(invalid_operation("quote is not ready for payment"));
                }
                Err(error) => return Err(store_error(error)),
            }
        }
        if quote.wallet_id == request.wallet {
            return Err(invalid_operation(
                "payer and recipient wallets must be distinct",
            ));
        }
        validate_wallet_amount(quote.amount_sat)?;
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
        component_image_any(&revision, &quote.wallet_id, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::WalletPay,
            &request,
            &request.idempotency_key,
            Capability::WalletControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        self.store
            .transition_wallet_quote(
                &self.workspace,
                &quote.id,
                WalletQuotePhase::Ready,
                WalletQuotePhase::Pending,
                None,
                None,
            )
            .map_err(store_error)?;
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::WalletPay(WalletPayAction {
                quote_id: request.quote_id.clone(),
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
                recipient_wallet: quote.wallet_id,
                amount_sat: quote.amount_sat,
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
        component_image(&revision, &request.mint, ComponentKind::Mint, "cdk")?;
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
        component_image(&revision, &request.mint, ComponentKind::Mint, "cdk")?;
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
            self.sync_wallet_quote_operation(&operation)?;
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
        let completed = self
            .store
            .record_operation_result(&self.workspace, &operation.id, phase, artifact)
            .map_err(store_error)?;
        self.sync_wallet_quote_operation(&completed)?;
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
        self.runtime()?
            .request_action_cancellation(&operation, &token)
            .await?;
        Ok(Json(operation))
    }

    #[tool(description = "Read a bounded page of the canonical experiment action journal")]
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
        let next_after_sequence = actions.last().map(|action| action.sequence);
        Ok(Json(ActionListResponse {
            actions,
            next_after_sequence,
        }))
    }

    #[tool(
        description = "Export a deterministic bounded evidence bundle for a closed experiment, including the immutable revision, canonical journal, and selected sanitized artifacts"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "evidence admission, selection, and final size checks stay visibly atomic"
    )]
    fn proofstorm_artifact_export(
        &self,
        Parameters(request): Parameters<ArtifactExportRequest>,
    ) -> Result<Json<EvidenceBundle>, ErrorData> {
        self.authorize_all(&[Capability::ExperimentRead, Capability::ArtifactRead])?;
        if request.artifact_operation_ids.len() > MAX_EXPLICIT_EVIDENCE_ARTIFACTS {
            return Err(evidence_error(
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
            return Err(evidence_error(
                "evidence_artifact_duplicate",
                "artifact operation IDs must be unique",
            ));
        }
        let experiment = self
            .store
            .experiment(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        if experiment.phase != ExperimentPhase::Closed {
            return Err(evidence_error(
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
                return Err(evidence_error(
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
            return Err(evidence_error(
                "evidence_journal_incomplete",
                "all experiment actions must be terminal before evidence export",
            ));
        }
        let known_ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(unknown) = explicit.iter().find(|id| !known_ids.contains(id.as_str())) {
            return Err(evidence_error(
                "evidence_artifact_unknown",
                &format!("operation {unknown:?} is not in the experiment journal"),
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
            return Err(evidence_error(
                "evidence_artifact_limit",
                "at most 32 artifact bodies may be included in one evidence bundle",
            ));
        }
        let mut artifacts = Vec::with_capacity(selected.len());
        for action in selected {
            let artifact = action.artifact.clone().ok_or_else(|| {
                evidence_error(
                    "evidence_artifact_missing",
                    &format!("operation {:?} has no terminal artifact", action.id),
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
            return Err(evidence_error(
                "evidence_bundle_too_large",
                "evidence bundle content exceeds 512 KiB",
            ));
        }
        Ok(Json(bundle))
    }

    #[tool(description = "Read a sanitized durable wallet quote lifecycle by Proofstorm ID")]
    async fn proofstorm_wallet_quote_status(
        &self,
        Parameters(request): Parameters<WalletQuoteRequest>,
    ) -> Result<Json<WalletQuote>, ErrorData> {
        self.authorize(Capability::ArtifactRead)?;
        let quote = self
            .store
            .wallet_quote(&self.workspace, &self.principal, &request.quote_id)
            .map_err(store_error)?;
        if !quote.phase.is_terminal()
            && let Some(operation_id) = quote.operation_id.as_deref()
        {
            let operation = self
                .store
                .operation(&self.workspace, &self.principal, operation_id)
                .map_err(store_error)?;
            if operation.artifact.is_some() {
                self.sync_wallet_quote_operation(&operation)?;
            } else if self.kubernetes.is_some() {
                self.proofstorm_operation_status(Parameters(OperationRequest {
                    operation_id: operation_id.to_owned(),
                }))
                .await?;
            }
            return self
                .store
                .wallet_quote(&self.workspace, &self.principal, &request.quote_id)
                .map(Json)
                .map_err(store_error);
        }
        Ok(Json(quote))
    }

    #[tool(description = "List sanitized wallet quote lifecycles for an experiment")]
    fn proofstorm_wallet_quote_list(
        &self,
        Parameters(request): Parameters<WalletQuoteListRequest>,
    ) -> Result<Json<WalletQuoteListResponse>, ErrorData> {
        self.authorize(Capability::ExperimentRead)?;
        let quotes = self
            .store
            .wallet_quotes(
                &self.workspace,
                &self.principal,
                &request.experiment_id,
                request.after_quote_id.as_deref(),
                request.limit,
            )
            .map_err(store_error)?;
        let next_after_quote_id = quotes.last().map(|quote| quote.id.clone());
        Ok(Json(WalletQuoteListResponse {
            quotes,
            next_after_quote_id,
        }))
    }

    #[tool(description = "Read an action and persist its bounded terminal artifact")]
    async fn proofstorm_action_status(
        &self,
        Parameters(request): Parameters<OperationRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.proofstorm_operation_status(Parameters(request)).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ProofstormMcp {}

fn tool_capabilities() -> Vec<(&'static str, &'static [Capability])> {
    let mut tools = design_tool_capabilities();
    tools.extend(runtime_tool_capabilities());
    tools
}

fn design_tool_capabilities() -> Vec<(&'static str, &'static [Capability])> {
    vec![
        ("proofstorm_workspace_read", &[Capability::LabRead]),
        ("proofstorm_catalog_list", &[Capability::CatalogRead]),
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
        (
            "proofstorm_wallet_pay",
            &[Capability::WalletControl, Capability::ArtifactRead],
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

fn wallet_quote_recovery_outcome(
    kind: OperationKind,
    operation_phase: OperationPhase,
    quote_phase: WalletQuotePhase,
    artifact_code: &str,
) -> Option<(WalletQuotePhase, Option<&str>)> {
    if quote_phase.is_terminal()
        || matches!(
            operation_phase,
            OperationPhase::Pending | OperationPhase::Running
        )
    {
        return None;
    }
    match (kind, operation_phase) {
        (OperationKind::WalletInvoice, OperationPhase::Succeeded) => {
            Some((WalletQuotePhase::Settled, None))
        }
        (OperationKind::WalletPay, OperationPhase::Succeeded) => {
            Some((WalletQuotePhase::Paid, None))
        }
        (_, OperationPhase::Cancelled)
            if matches!(
                quote_phase,
                WalletQuotePhase::Requested | WalletQuotePhase::Ready
            ) =>
        {
            Some((WalletQuotePhase::Cancelled, Some("action_cancelled")))
        }
        (_, OperationPhase::Cancelled) => Some((
            WalletQuotePhase::Inconclusive,
            Some("payment_state_inconclusive"),
        )),
        (_, OperationPhase::Failed)
            if matches!(
                quote_phase,
                WalletQuotePhase::Pending | WalletQuotePhase::Paid | WalletQuotePhase::Inconclusive
            ) || matches!(
                artifact_code,
                "action_job_lost" | "terminal_artifact_missing"
            ) =>
        {
            Some((WalletQuotePhase::Inconclusive, Some(artifact_code)))
        }
        (_, OperationPhase::Failed) if artifact_code == "action_deadline_exceeded" => {
            Some((WalletQuotePhase::Expired, Some("action_deadline_exceeded")))
        }
        (_, OperationPhase::Failed) => Some((WalletQuotePhase::Failed, Some(artifact_code))),
        _ => None,
    }
}

impl ProofstormMcp {
    fn runtime(&self) -> Result<&KubernetesRuntime, ErrorData> {
        self.kubernetes.as_ref().ok_or_else(|| {
            ErrorData::invalid_request(
                "Kubernetes runtime is not configured",
                Some(serde_json::json!({"code": "runtime_unavailable"})),
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

    fn sync_wallet_quote_operation(&self, operation: &LabOperation) -> Result<(), ErrorData> {
        if !matches!(
            operation.kind,
            OperationKind::WalletInvoice | OperationKind::WalletPay
        ) || matches!(
            operation.phase,
            OperationPhase::Pending | OperationPhase::Running
        ) {
            return Ok(());
        }
        let Some(quote_id) = operation.request["quote_id"].as_str() else {
            return Err(ErrorData::internal_error(
                "wallet quote operation has no quote identity",
                Some(serde_json::json!({"code": "quote_identity_missing"})),
            ));
        };
        let artifact_code = operation
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.content["code"].as_str())
            .unwrap_or("action_failed");
        for _ in 0..3 {
            let quote = self
                .store
                .wallet_quote(&self.workspace, &self.principal, quote_id)
                .map_err(store_error)?;
            if quote.phase.is_terminal() {
                return Ok(());
            }
            let Some((next, terminal_code)) = wallet_quote_recovery_outcome(
                operation.kind,
                operation.phase,
                quote.phase,
                artifact_code,
            ) else {
                return Ok(());
            };
            match self.store.transition_wallet_quote(
                &self.workspace,
                quote_id,
                quote.phase,
                next,
                None,
                terminal_code,
            ) {
                Ok(_) => return Ok(()),
                Err(StoreError::StaleQuote { .. }) => {}
                Err(error) => return Err(store_error(error)),
            }
        }
        Err(ErrorData::invalid_request(
            "wallet quote changed repeatedly while synchronizing action status",
            Some(serde_json::json!({"code": "stale_quote"})),
        ))
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
    if request.channel_sat > request.funding_sat || request.push_sat > request.channel_sat / 2 {
        return Err(invalid_operation(
            "channel_sat cannot exceed funding_sat and push_sat cannot exceed half the channel",
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

fn invalid_operation(message: &str) -> ErrorData {
    ErrorData::invalid_request(
        message.to_owned(),
        Some(serde_json::json!({"code": "invalid_operation"})),
    )
}

fn evidence_error(code: &str, message: &str) -> ErrorData {
    ErrorData::invalid_request(message.to_owned(), Some(serde_json::json!({"code": code})))
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
            return Err(ErrorData::invalid_request(
                format!("lab instance {:?} is not ready for actions", instance.id),
                Some(serde_json::json!({"code": "instance_not_ready"})),
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
                return Err(ErrorData::invalid_request(
                    format!("action resource {name:?} already exists with a different request"),
                    Some(serde_json::json!({"code": "action_identity_conflict"})),
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
        let action = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
            .ok_or_else(|| {
                ErrorData::resource_not_found(
                    format!("action {:?} was not found", operation.resource_name),
                    Some(serde_json::json!({"code": "action_runtime_not_found"})),
                )
            })?;
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

    async fn request_action_cancellation(
        &self,
        operation: &LabOperation,
        token: &str,
    ) -> Result<(), ErrorData> {
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let action = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
            .ok_or_else(|| {
                ErrorData::resource_not_found(
                    format!("action {:?} was not found", operation.resource_name),
                    Some(serde_json::json!({"code": "action_runtime_not_found"})),
                )
            })?;
        if action.spec.workspace_id != operation.workspace_id
            || action.spec.instance_id != operation.instance_id
            || action.spec.experiment_id != operation.experiment_id
            || action.spec.operation_id != operation.id
            || action.spec.principal_id != operation.principal_id
            || action.spec.request_digest != operation.request_digest
        {
            return Err(ErrorData::invalid_request(
                "action cancellation identity does not match the journal",
                Some(serde_json::json!({"code": "action_identity_conflict"})),
            ));
        }
        if action.status.as_ref().is_some_and(|status| {
            matches!(
                status.phase,
                ActionPhase::Succeeded | ActionPhase::Failed | ActionPhase::Cancelled
            )
        }) || action.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
        {
            return Ok(());
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
        Ok(())
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
        assert_eq!(designer.tool_names().len(), 14);
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
        assert_eq!(
            reader.tool_names(),
            vec![
                "proofstorm_lab_diff",
                "proofstorm_lab_read",
                "proofstorm_workspace_read",
            ]
        );
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
        assert_eq!(first.digest, proofstorm_core::digest_json(&first.content));
        assert_eq!(first.content.journal.len(), 1);
        assert_eq!(first.content.artifacts.len(), 1);
        assert_eq!(first.content.revision.digest, revision.digest);
        assert_eq!(
            first.content.instance.lock_digest,
            first.content.revision.lock.digest
        );
        assert!(first.byte_length as usize <= MAX_EVIDENCE_BUNDLE_BYTES);
        let encoded = serde_json::to_string(&first).expect("serialize evidence");
        assert!(!encoded.contains("resource_name"));
        assert!(!encoded.contains("instance_key"));
        assert!(!encoded.contains("kubernetes"));
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

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the restart acceptance scenario keeps durable setup and reconciliation evidence together"
    )]
    async fn quote_status_repairs_ambiguous_state_from_durable_settlement_after_restart() {
        let store = seeded_store();
        for capability in [
            Capability::ExperimentCreate,
            Capability::LeaseAcquire,
            Capability::WalletFund,
            Capability::ArtifactRead,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("recovery grant");
        }
        store
            .create_draft(
                "alpha",
                "designer",
                "recovery",
                &lab("recovery"),
                "create-recovery",
            )
            .expect("draft");
        let revision = store
            .publish("alpha", "designer", "recovery", 1, "publish-recovery")
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "recovery-instance",
                &revision.digest,
                "materialize-recovery",
            )
            .expect("instance");
        store
            .create_experiment(
                "alpha",
                "designer",
                "recovery-experiment",
                "recovery-instance",
                "create-recovery-experiment",
            )
            .expect("experiment");
        store
            .acquire_lease(
                "alpha",
                "designer",
                "recovery-experiment",
                "recovery-lease",
                300,
                1,
                "acquire-recovery-lease",
            )
            .expect("lease");
        store
            .create_wallet_quote(
                "alpha",
                "designer",
                "recovery-instance",
                "recovery-experiment",
                "recovery-lease",
                "recovery-quote",
                "receiver-wallet",
                "mint",
                WalletQuoteDirection::Receive,
                100,
                300,
                "create-recovery-quote",
            )
            .expect("quote");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "recovery-instance",
                "recovery-experiment",
                "recovery-lease",
                "recovery-invoice",
                OperationKind::WalletInvoice,
                &serde_json::json!({"quote_id": "recovery-quote"}),
                "create-recovery-invoice",
                Capability::WalletFund,
            )
            .expect("operation");
        store
            .transition_wallet_quote(
                "alpha",
                "recovery-quote",
                WalletQuotePhase::Requested,
                WalletQuotePhase::Inconclusive,
                Some(&operation.id),
                Some("terminal_artifact_missing"),
            )
            .expect("ambiguous quote");
        store
            .record_operation_result(
                "alpha",
                &operation.id,
                OperationPhase::Succeeded,
                serde_json::json!({"phase": "settled", "amount_sat": 100}),
            )
            .expect("durable settlement artifact");

        let restarted = ProofstormMcp::new(store, "alpha", "designer").expect("restart session");
        let quote = restarted
            .proofstorm_wallet_quote_status(Parameters(WalletQuoteRequest {
                quote_id: "recovery-quote".into(),
            }))
            .await
            .expect("reconciled quote")
            .0;
        assert_eq!(quote.phase, WalletQuotePhase::Settled);
        assert!(quote.settled_at_unix.is_some());
        assert!(quote.terminal_code.is_none());
    }

    #[test]
    fn wallet_quote_recovery_is_conservative_and_reconcilable() {
        assert_eq!(
            wallet_quote_recovery_outcome(
                OperationKind::WalletInvoice,
                OperationPhase::Failed,
                WalletQuotePhase::Ready,
                "action_deadline_exceeded",
            ),
            Some((WalletQuotePhase::Expired, Some("action_deadline_exceeded")))
        );
        assert_eq!(
            wallet_quote_recovery_outcome(
                OperationKind::WalletInvoice,
                OperationPhase::Cancelled,
                WalletQuotePhase::Ready,
                "action_cancelled",
            ),
            Some((WalletQuotePhase::Cancelled, Some("action_cancelled")))
        );
        for code in ["action_job_lost", "terminal_artifact_missing"] {
            assert_eq!(
                wallet_quote_recovery_outcome(
                    OperationKind::WalletPay,
                    OperationPhase::Failed,
                    WalletQuotePhase::Pending,
                    code,
                ),
                Some((WalletQuotePhase::Inconclusive, Some(code)))
            );
        }
        assert_eq!(
            wallet_quote_recovery_outcome(
                OperationKind::WalletInvoice,
                OperationPhase::Succeeded,
                WalletQuotePhase::Inconclusive,
                "action_failed",
            ),
            Some((WalletQuotePhase::Settled, None))
        );
        assert_eq!(
            wallet_quote_recovery_outcome(
                OperationKind::WalletPay,
                OperationPhase::Succeeded,
                WalletQuotePhase::Inconclusive,
                "action_failed",
            ),
            Some((WalletQuotePhase::Paid, None))
        );
        assert_eq!(
            wallet_quote_recovery_outcome(
                OperationKind::WalletPay,
                OperationPhase::Failed,
                WalletQuotePhase::Pending,
                "action_deadline_exceeded",
            ),
            Some((
                WalletQuotePhase::Inconclusive,
                Some("action_deadline_exceeded")
            ))
        );
    }
}
