#[cfg(test)]
use proofstorm_core::default_catalog;
mod directories;
#[cfg(test)]
mod surface_tests;
#[cfg(test)]
mod workspace_evidence_tests;
pub use directories::{DirectoryQuery, EnvironmentRequest};
mod cell_read;
mod submission;
pub use cell_read::CellReadRequest;
pub use submission::{CellPatch, PlanReference, SubmissionRequest};
mod cell_input;
pub use cell_input::{CellFile, CellInput};
mod cell_search;
pub use cell_search::{CellSearchRequest, CellSearchResult, CellSearchSection};
mod activity_search;
mod cell_inspect;
mod cell_sync;
pub use cell_inspect::CellInspectRequest;
mod cell_up;
mod read_query;
mod session_directory;
pub use session_directory::SessionListRequest;
mod status_search;
pub use activity_search::ActivitySearchRequest;
mod operation_read;
pub use operation_read::OperationReadRequest;
mod tool_schema;

use proofstorm_app::runtime::missing_action_artifact;
use proofstorm_app::runtime::runtime_action_resource;
#[cfg(test)]
use proofstorm_app::runtime::terminal_action_observation;
#[cfg(test)]
use proofstorm_core::ComponentStatus;
#[cfg(test)]
use proofstorm_kube::ActionPhase;
use std::collections::{BTreeMap, BTreeSet};

use kube::{
    Api, Client,
    api::{Patch, PatchParams},
};
use proofstorm_core::{
    AuthenticationProtocol, BitcoinNetwork, CandidateBuild, CandidateSource, Capability,
    CatalogDependencySupport, CatalogEntry, CatalogFeature, CatalogResponse,
    CatalogRuntimeEndpoint, CatalogSupportMatrix, CellInstance, CellInstanceStatus, CellOperation,
    CellPolicy, CellSpec, ComponentKind, ComponentSpec, ControlClass, DatabaseRole,
    DependencyBinding, EVIDENCE_API_VERSION, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance, Experiment, ExperimentPhase, InstancePhase,
    InventoryEntry, LinkKind, LinkSpec, NetworkFaultBackend, OperationArtifact, OperationKind,
    OperationPhase, PaymentMethod, PublishedRevision, ReleaseChannel, SupportLifecycle,
    TeardownReceipt as CoreTeardownReceipt, ValidationIssue, digest_json,
    network_policy_fault_backend, validate_cell,
};
use proofstorm_kube::{
    CANDIDATE_CANCEL_ANNOTATION, CellAction, ComponentExecLiveAction, ComponentForensicsAction,
    ComponentLogsAction, NetworkHealAction, NetworkPartitionAction, ProofstormCandidateBuild,
    ProofstormCandidateBuildSpec, ProofstormCellAction, ReachabilityOracleAction,
    WalletBalanceAction, component_ports,
};
use proofstorm_store::{Draft, Store, StoreError, Workspace};
use rmcp::{
    ErrorData, Json, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ListResourceTemplatesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents,
        ResourceTemplate, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellSyncRequest {
    pub name: String,
    /// Return activity after this sequence. Pass the response's `next_sequence`
    /// here to continue reading a page.
    #[serde(default)]
    pub after_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellExecRequest {
    pub name: String,
    pub component: String,
    pub request_id: String,
    #[serde(default)]
    pub argv: Vec<String>,
    /// Shell script instead of argv. Its exit status describes the shell.
    #[serde(default)]
    pub script: String,
    /// Optional opaque custody binding. Discover `private_transfer` and `private_access_issue`.
    #[serde(default)]
    pub private_payload: Option<proofstorm_core::private_io::PayloadBinding>,
    /// Optional run grouping; ordinary commands receive automatic attribution.
    #[serde(default)]
    pub run_id: String,
    #[serde(default = "default_wait_timeout_seconds")]
    pub timeout_seconds: u32,
    /// Choose before execution: `private` (default) hides both streams;
    /// `public` returns bounded stdout/stderr for help, addresses, node IDs,
    /// and other native results. `json_fields` requires 1..=16 named receipt
    /// fields: `status`, `state`, `failure_reason`, `settled`, `synced_to_chain`, `amount`,
    /// `amount_sat`, `fee_paid`, `fee_paid_sat`, `value_sat`, `total_fees`, `total_fees_msat`,
    /// `num_active_channels`, `balance`, `confirmed_balance`, `unconfirmed_balance`,
    /// `seedAccess.state`, `seedAccess.requiresPassphrase`, cocoSession.state.
    /// It is not an arbitrary JSON query. `bolt11` extracts an invoice from
    /// text; `lnd_invoice` extracts one from LND addinvoice JSON. Omit fields
    /// except in `json_fields` mode. Private output cannot be read from the receipt.
    #[serde(default)]
    pub output: proofstorm_core::native::NativeOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskRequest {
    pub name: String,
    pub component: String,
    /// Exact retry key for this control call. Use a new key for each fresh status/log read.
    pub request_id: String,
    pub task: proofstorm_core::workspace::TaskRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFileRequest {
    pub name: String,
    pub component: String,
    /// Exact retry key for this control call. Use a new key for each fresh read.
    pub request_id: String,
    pub file: proofstorm_core::workspace::FileRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellRemoveRequest {
    pub name: String,
    /// Copy `instance_key` from `cell_inspect`.
    pub expected_instance_key: String,
    #[serde(default = "default_wait_timeout_seconds")]
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CellRemoveReceipt {
    pub name: String,
    pub instance_key: String,
    /// True only after verifying absence of this exact incarnation.
    pub complete: bool,
    /// This call's wait ended; repeat `cell_remove` to advance or verify cleanup.
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<CoreTeardownReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_tool: Option<String>,
}

pub use proofstorm_app::cell::ReconciliationError as CellReconciliationError;

/// Full configuration is returned only on this explicit read, not expanded into every discovery profile.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellReadResponse {
    pub id: String,
    pub workspace_id: String,
    pub version: u64,
    /// Canonical complete cell document. Preserve all components, links and policy when editing.
    #[schemars(with = "serde_json::Value")]
    pub cell: CellSpec,
}
impl From<Draft> for CellReadResponse {
    fn from(draft: Draft) -> Self {
        Self {
            id: draft.id,
            workspace_id: draft.workspace_id,
            version: draft.version,
            cell: draft.cell,
        }
    }
}

pub use proofstorm_view::{CatalogDependencyFilter, CatalogListRequest};

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
    /// Controls accepted by publication for this exact component release.
    pub allowed_control: Vec<ControlClass>,
    /// Safe default control for ordinary cell authoring.
    pub recommended_control: ControlClass,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    pub origin: proofstorm_core::CatalogOrigin,
    pub build_profile: Option<proofstorm_core::CandidateBuildProfile>,
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
    pub runtime_endpoints: Vec<CatalogRuntimeEndpoint>,
    pub image: String,
    pub source_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CandidateSource>,
    pub allowed_control: Vec<ControlClass>,
    /// Safe default control for ordinary cell authoring.
    pub recommended_control: ControlClass,
    /// Names of all agent-authorable configuration properties. An empty
    /// configuration is valid when `required_config_fields` is empty.
    pub authorable_config_fields: Vec<String>,
    pub required_config_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_defaults: BTreeMap<String, serde_json::Value>,
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

pub use proofstorm_app::candidate::CandidateBuildRequest;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateWaitRequest {
    pub candidate_id: String,
    #[serde(default = "default_wait_timeout_seconds")]
    #[schemars(range(min = 1, max = 120))]
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateCancelRequest {
    pub candidate_id: String,
}

pub use proofstorm_view::{CandidateBuildReceipt, CandidateCatalogSelector};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellValidationResult {
    pub valid: bool,
    pub component_count: usize,
    pub link_count: usize,
    pub issue_count: usize,
    pub next_issue_offset: Option<usize>,
    pub details_omitted: bool,
    pub issues: Vec<ValidationIssue>,
    pub component_ids: Vec<String>,
    pub link_ids: Vec<String>,
    pub warnings: Vec<String>,
}

/// A link authored through MCP. Backend binding fields are flattened into each
/// kind-specific wire variant so a client cannot silently lose the entire
/// nested binding object. Proofstorm constructs the canonical persisted
/// `DependencyBinding`; peer and network-path variants admit no binding fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AddLinkInput {
    BitcoinPeer {
        id: String,
        from: String,
        to: String,
    },
    LightningPeer {
        id: String,
        from: String,
        to: String,
    },
    ChainBackend {
        id: String,
        from: String,
        to: String,
        network: BitcoinNetwork,
    },
    PaymentBackend {
        id: String,
        from: String,
        to: String,
        method: PaymentMethod,
        unit: String,
    },
    DatabaseBackend {
        id: String,
        from: String,
        to: String,
        role: DatabaseRole,
    },
    AuthenticationBackend {
        id: String,
        from: String,
        to: String,
        protocol: AuthenticationProtocol,
    },
    NetworkPath {
        id: String,
        from: String,
        to: String,
    },
}

impl TryFrom<AddLinkInput> for LinkSpec {
    type Error = String;

    fn try_from(input: AddLinkInput) -> Result<Self, Self::Error> {
        let link = match input {
            AddLinkInput::BitcoinPeer { id, from, to } => Self {
                id,
                kind: LinkKind::BitcoinPeer,
                from,
                to,
                binding: None,
            },
            AddLinkInput::LightningPeer { id, from, to } => Self {
                id,
                kind: LinkKind::LightningPeer,
                from,
                to,
                binding: None,
            },
            AddLinkInput::ChainBackend {
                id,
                from,
                to,
                network,
            } => Self {
                id,
                kind: LinkKind::ChainBackend,
                from,
                to,
                binding: Some(DependencyBinding::Chain { network }),
            },
            AddLinkInput::PaymentBackend {
                id,
                from,
                to,
                method,
                unit,
            } => Self {
                id,
                kind: LinkKind::PaymentBackend,
                from,
                to,
                binding: Some(DependencyBinding::Payment { method, unit }),
            },
            AddLinkInput::DatabaseBackend { id, from, to, role } => Self {
                id,
                kind: LinkKind::DatabaseBackend,
                from,
                to,
                binding: Some(DependencyBinding::Database { role }),
            },
            AddLinkInput::AuthenticationBackend {
                id,
                from,
                to,
                protocol,
            } => Self {
                id,
                kind: LinkKind::AuthenticationBackend,
                from,
                to,
                binding: Some(DependencyBinding::Authentication { protocol }),
            },
            AddLinkInput::NetworkPath { id, from, to } => Self {
                id,
                kind: LinkKind::NetworkPath,
                from,
                to,
                binding: None,
            },
        };
        Ok(link)
    }
}

impl TryFrom<LinkSpec> for AddLinkInput {
    type Error = String;

    fn try_from(link: LinkSpec) -> Result<Self, Self::Error> {
        let LinkSpec {
            id,
            kind,
            from,
            to,
            binding,
        } = link;
        match (kind, binding) {
            (LinkKind::BitcoinPeer, None) => Ok(Self::BitcoinPeer { id, from, to }),
            (LinkKind::LightningPeer, None) => Ok(Self::LightningPeer { id, from, to }),
            (LinkKind::ChainBackend, Some(DependencyBinding::Chain { network })) => {
                Ok(Self::ChainBackend {
                    id,
                    from,
                    to,
                    network,
                })
            }
            (LinkKind::PaymentBackend, Some(DependencyBinding::Payment { method, unit })) => {
                Ok(Self::PaymentBackend {
                    id,
                    from,
                    to,
                    method,
                    unit,
                })
            }
            (LinkKind::DatabaseBackend, Some(DependencyBinding::Database { role })) => {
                Ok(Self::DatabaseBackend { id, from, to, role })
            }
            (
                LinkKind::AuthenticationBackend,
                Some(DependencyBinding::Authentication { protocol }),
            ) => Ok(Self::AuthenticationBackend {
                id,
                from,
                to,
                protocol,
            }),
            (LinkKind::NetworkPath, None) => Ok(Self::NetworkPath { id, from, to }),
            _ => Err(format!(
                "canonical {kind:?} link {id:?} has a missing or mismatched binding"
            )),
        }
    }
}

/// Complete cell input accepted at the MCP boundary. Unlike the persisted core
/// model, backend-link variants require their binding fields as flat scalars.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoredCellSpec {
    pub api_version: String,
    pub name: String,
    pub components: Vec<ComponentSpec>,
    pub links: Vec<AddLinkInput>,
    /// Optional capability restrictions. Omit this field for the safe default
    /// policy unless the experiment deliberately needs a narrower envelope.
    #[serde(default)]
    pub policy: CellPolicy,
}

/// Accept the canonical JSON object and the common MCP-client failure mode where
/// that object is encoded one extra time as a JSON string. Parsing still lands
/// in the same strict `AuthoredCellSpec` contract, including unknown-field and
/// required-field checks; malformed or incomplete strings remain fail-closed.
fn deserialize_authored_cell<'de, D>(deserializer: D) -> Result<AuthoredCellSpec, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let value = match value {
        serde_json::Value::String(encoded) => {
            serde_json::from_str(&encoded).map_err(serde::de::Error::custom)?
        }
        value => value,
    };
    let authored_error = match serde_json::from_value(value.clone()) {
        Ok(authored) => return Ok(authored),
        Err(error) => error,
    };
    let has_canonical_binding = value
        .get("links")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|links| {
            links.iter().any(|link| {
                link.as_object()
                    .is_some_and(|link| link.contains_key("binding"))
            })
        });
    if !has_canonical_binding {
        return Err(serde::de::Error::custom(authored_error));
    }
    let canonical: CellSpec = match serde_json::from_value(value) {
        Ok(canonical) => canonical,
        Err(_) => return Err(serde::de::Error::custom(authored_error)),
    };
    Ok(AuthoredCellSpec {
        api_version: canonical.api_version,
        name: canonical.name,
        components: canonical.components,
        links: canonical
            .links
            .into_iter()
            .map(AddLinkInput::try_from)
            .collect::<Result<_, _>>()
            .map_err(serde::de::Error::custom)?,
        policy: canonical.policy,
    })
}

impl TryFrom<AuthoredCellSpec> for CellSpec {
    type Error = String;

    fn try_from(input: AuthoredCellSpec) -> Result<Self, Self::Error> {
        Ok(Self {
            api_version: input.api_version,
            name: input.name,
            components: input.components,
            links: input
                .links
                .into_iter()
                .map(LinkSpec::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            policy: input.policy,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellStatusSummary {
    pub instance_key: String,
    pub observed_generation: u64,
    pub observed_revision_digest: String,
    pub last_converged_revision: Option<String>,
    pub retained_storage: BTreeMap<String, String>,

    pub generation: u64,
    /// First eight startup failures; use `component_status_list` for all details.
    #[serde(default)]
    pub blockers: Vec<StartupBlocker>,
    pub instance_id: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub phase: InstancePhase,
    pub instance_namespace: String,
    pub ready_components: u32,
    pub total_components: u32,
    pub inventory_count: u32,
    pub inventory_digest: String,
    /// Meaning of `ready` and the required next runtime action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_guidance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<CoreTeardownReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellComponentStatusListRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Exact component ID; omit to search all components.
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub ready: Option<bool>,
    /// Literal text in each component's JSON; empty matches all.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_insensitive: bool,
    /// Return only id, kind and ready. Mutually exclusive with fields.
    #[serde(default)]
    pub scan: bool,
    /// RFC 6901 pointers relative to a component, e.g. /conditions/0/reason.
    /// Empty returns full statuses. Missing fields are null.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default = "default_status_list_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    /// Opaque continuation token returned by a prior component-status page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellComponentStatusListResponse {
    pub instance_id: String,
    pub revision_digest: String,
    /// Identifies this live observation. Readiness can change between pages;
    /// the cursor is bound to filters, the cell revision and matching component IDs.
    pub observation_digest: String,
    pub components: Vec<serde_json::Value>,
    pub matched_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellInventoryListRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Exact Kubernetes kind and namespace filters.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    /// Literal text in each entry's JSON; empty matches all.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_insensitive: bool,
    /// RFC 6901 pointers relative to an entry, e.g. /name or /kind.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default = "default_status_list_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    /// Opaque continuation token returned by a prior inventory page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellInventoryListResponse {
    pub instance_id: String,
    pub inventory_digest: String,
    pub inventory: Vec<serde_json::Value>,
    pub matched_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellWaitRequest {
    /// Copy from close/status to verify deletion even after local cleanup.
    #[serde(default)]
    pub expected_instance_key: Option<String>,
    /// Wait for this desired generation; returns superseded if another edit replaces it.
    #[serde(default)]
    pub expected_generation: Option<u64>,
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Phase that ends the wait successfully. `ready` and `closed` are the
    /// normal materialization and teardown targets.
    pub target_phase: InstancePhase,
    /// Server-side wait bound in 1..=120 seconds.
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellWaitResult {
    pub instance_key: String,
    pub observed_generation: u64,
    pub observed_revision_digest: String,
    pub last_converged_revision: Option<String>,
    pub retained_storage: BTreeMap<String, String>,

    pub generation: u64,
    pub superseded: bool,
    /// First eight startup failures. For a ready target, these end the wait early.
    #[serde(default)]
    pub blockers: Vec<StartupBlocker>,
    pub instance_id: String,
    pub phase: InstancePhase,
    pub target_phase: InstancePhase,
    pub reached: bool,
    pub timed_out: bool,
    pub ready_components: u32,
    pub total_components: u32,
    /// Meaning of `ready` and the required next runtime action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_guidance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_receipt: Option<CoreTeardownReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationResult {
    pub operation_id: String,
    /// Recorded snapshot digest for subsequent `operation_read` calls.
    pub operation_digest: String,
    pub run_id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub phase: OperationPhase,
    pub terminal: bool,
    pub timed_out: bool,
    /// Digest remains available even when the optional artifact body is omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
    /// Native exit, cleanup, projection and truncation facts, preserved even
    /// when stdout/stderr would exceed the response budget. Phase alone is not
    /// native command success. Missing fields mean unavailable, not success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<OperationArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationWaitError {
    pub operation_id: String,
    pub error: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationWaitRequest {
    /// Unique operation IDs to await together. Start independent operations
    /// first, then prefer this over repeated single-operation waits.
    #[schemars(length(min = 1))]
    pub operation_ids: Vec<String>,
    /// Shared server-side wait bound in 1..=120 seconds.
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationWaitResult {
    /// Successful reads preserve their relative request order.
    pub operations: Vec<OperationResult>,
    /// Per-ID failures never discard successfully read operations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<OperationWaitError>,
    pub all_terminal: bool,
    pub timed_out: bool,
    /// True only when optional artifact bodies were removed to keep the batch
    /// response within the agent response budget. Use `operation_wait` for any
    /// one omitted body.
    pub artifact_bodies_omitted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentControlRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub component: String,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentLogsRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub component: String,
    /// Lines to read from the end of the component's current container log,
    /// between 1 and 2000. The artifact is additionally byte-bounded.
    pub tail_lines: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentExecRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub component: String,
    /// Cell component whose native service endpoint should be exposed to the
    /// command. When omitted, the execution component is also the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_component: Option<String>,
    /// An unrestricted non-interactive shell program run by `/bin/sh` inside
    /// the component's pinned image. Native command failures are returned as
    /// an exit code in the terminal artifact.
    pub script: String,
    pub timeout_seconds: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeExecutionRequest {
    /// Opaque custody reference; token bytes never belong in this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_payload: Option<proofstorm_core::private_io::PayloadBinding>,
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub component: String,
    /// POSIX shell program. Its exit code describes the shell, including any pipelines.
    #[serde(default)]
    pub script: String,
    /// Direct command and arguments. Prefer this to preserve the native process exit status.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Private default; public raw. `json_fields`: status,state,`failure_reason`,settled,
    /// `synced_to_chain`,amount,`amount_sat`,
    /// `fee_paid`,`fee_paid_sat`,`value_sat`,`total_fees`,`total_fees_msat`,`num_active_channels`,
    /// balance,`confirmed_balance`,`unconfirmed_balance`,`seedAccess.state`,
    /// `seedAccess.requiresPassphrase`,`cocoSession.state`.
    /// bolt11: invoice text. `lnd_invoice`: LND JSON with matching hash.
    /// Both return validated invoice/hash/amount/currency/expiry; raw streams stay private.
    #[serde(default)]
    pub output: proofstorm_core::native::NativeOutput,
    pub timeout_seconds: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

// Method-specific public input. Keep the flat Kubernetes action an internal wire type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "transferMethod", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateTransferInput {
    Prepare {
        component: String,
        #[serde(rename = "destinationComponent")]
        destination_component: String,
        /// Reserve before export: 1..=1048576 bytes; CDK recipients allow at most 65536.
        #[serde(rename = "maximumBytes")]
        #[schemars(range(min = 1, max = 1_048_576))]
        maximum_bytes: u32,
    },
    Handoff {
        component: String,
        reference: String,
        #[serde(rename = "recipientGrantId")]
        recipient_grant_id: String,
    },
    Status {
        component: String,
        reference: String,
    },
    Deliver {
        component: String,
        reference: String,
    },
    Release {
        component: String,
        reference: String,
    },
}

impl PrivateTransferInput {
    fn action(&self) -> Result<proofstorm_kube::PrivateTransferAction, ErrorData> {
        use proofstorm_core::private_io::MAX_PRIVATE_BYTES;
        use proofstorm_kube::{PrivateTransferAction, TransferMethod};
        let recipient_grant_id = if let Self::Handoff {
            recipient_grant_id, ..
        } = self
        {
            if recipient_grant_id.trim().is_empty() {
                return Err(invalid_operation(
                    "handoff.recipientGrantId must be nonempty",
                ));
            }
            Some(recipient_grant_id.clone())
        } else {
            None
        };
        let (method, component, destination, reference, maximum) = match self {
            Self::Prepare {
                component,
                destination_component,
                maximum_bytes,
            } => {
                if !(1..=MAX_PRIVATE_BYTES).contains(maximum_bytes) {
                    return Err(invalid_operation(
                        "prepare.maximumBytes must be between 1 and 1048576; no operation was created",
                    ));
                }
                if component == destination_component {
                    return Err(invalid_operation(
                        "prepare.destinationComponent must differ from component; no operation was created",
                    ));
                }
                (
                    TransferMethod::Prepare,
                    component,
                    Some(destination_component.clone()),
                    None,
                    Some(*maximum_bytes),
                )
            }
            Self::Handoff {
                component,
                reference,
                ..
            } => (
                TransferMethod::Handoff,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
            Self::Status {
                component,
                reference,
            } => (
                TransferMethod::Status,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
            Self::Deliver {
                component,
                reference,
            } => (
                TransferMethod::Deliver,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
            Self::Release {
                component,
                reference,
            } => (
                TransferMethod::Release,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
        };
        if component.trim().is_empty()
            || destination.as_ref().is_some_and(|id| id.trim().is_empty())
            || reference.as_ref().is_some_and(|id| id.trim().is_empty())
        {
            return Err(invalid_operation(
                "private transfer component, destinationComponent and reference must be nonempty when required; no operation was created",
            ));
        }
        Ok(PrivateTransferAction {
            recipient_grant_id,
            transfer_method: method,
            component: component.clone(),
            destination_component: destination,
            reference,
            maximum_bytes: maximum,
        })
    }
}

fn validate_private_transfer_endpoints(
    transfer: &proofstorm_kube::PrivateTransferAction,
    revision: &PublishedRevision,
) -> Result<(), ErrorData> {
    for id in std::iter::once(&transfer.component).chain(transfer.destination_component.iter()) {
        component_image_any(revision, id, ComponentKind::Wallet)?;
    }
    if let Some(destination) = &transfer.destination_component {
        let is_cdk = revision
            .cell
            .components
            .iter()
            .any(|c| &c.id == destination && c.implementation == "cdk-cli-wallet");
        if is_cdk
            && transfer
                .maximum_bytes
                .is_some_and(|maximum| maximum > proofstorm_core::private_io::MAX_PRIVATE_ARG_BYTES)
        {
            return Err(invalid_operation(
                "prepare.maximumBytes must be at most 65536 for a CDK CLI recipient's native argv input; no operation was created",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateTransferRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub transfer: PrivateTransferInput,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkPartitionRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub from_component: String,
    pub to_component: String,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkHealRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub partition_operation_id: String,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletBalanceRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkProbeRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub from_component: String,
    pub to_component: String,
    /// Logical destination service, such as `http`, `rpc`, or `p2p`.
    pub service: String,
    #[serde(default = "default_probe_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_probe_attempts")]
    pub attempts: u32,
    #[serde(skip)]
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
    #[serde(rename = "request_id")]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunStartRequest {
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(rename = "name")]
    pub instance_id: String,
    #[serde(rename = "request_id")]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunReadRequest {
    /// Exact experiment/run ID from a receipt or the user; distinct from workspace, cell and session IDs.
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunFinishRequest {
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(rename = "request_id")]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateAccessRequest {
    pub receive: proofstorm_core::PrivateReceiveCommand,
    #[serde(rename = "name")]
    pub instance_id: String,
    pub recipient_principal_id: String,
    pub recipient_grant_id: String,
    pub component: String,
    pub mint: String,
    pub reference: String,
    #[serde(rename = "request_id")]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateAccessIdRequest {
    pub grant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceExportRequest {
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    /// Include full artifact bodies for conservation and reachability oracles.
    #[serde(default = "default_true")]
    pub include_oracle_artifacts: bool,
    /// Optional full bodies for additional operations. Do not enumerate
    /// the experiment: every action and artifact descriptor is always in the journal.
    #[serde(default)]
    pub artifact_operation_ids: Vec<String>,
    /// Internal full-bundle assembly; MCP clients read the returned immutable resource.
    #[serde(skip)]
    pub include_content: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceExportResponse {
    pub media_type: String,
    pub digest: String,
    pub byte_length: u32,
    pub workspace_id: String,
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub journal_count: u32,
    pub artifact_count: u32,
    pub workspace_capture_count: u32,
    /// Always true: every experiment action and its artifact descriptor is in the journal.
    pub journal_complete: bool,
    /// Artifact bodies are optional enrichments; their count need not equal `journal_count`.
    pub artifact_bodies_optional: bool,
    pub guidance: String,
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
    WorkspaceCapture,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSectionReadRequest {
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    /// Must match the selection used for the evidence manifest.
    #[serde(default = "default_true")]
    pub include_oracle_artifacts: bool,
    /// Must match the selection used for the evidence manifest.
    #[serde(default)]
    pub artifact_operation_ids: Vec<String>,
    pub section: EvidenceSection,
    /// RFC 6901 pointer within the section. Artifact data lives under `/artifact/content`
    /// (e.g. `/artifact/content/exit_code`); `/artifact/digest` reads its digest. Empty reads the whole section.
    #[serde(default)]
    pub pointer: String,
    /// Required for artifact reads and ignored for other sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Required for `workspace_capture` section reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
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

const EVIDENCE_ACTION_PAGE_SIZE: u32 = 100;

const fn default_evidence_section_limit() -> u32 {
    20
}

const fn default_status_list_limit() -> u32 {
    20
}

const fn default_wait_timeout_seconds() -> u32 {
    30
}

const MAX_AGENT_RESPONSE_BYTES: usize = 32 * 1024;

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
        for capability in [Capability::CatalogRead, Capability::CellValidate] {
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
        for route in tool_router.map.values_mut() {
            route.attr.title = proofstorm_view::tool_title(&route.attr.name).map(str::to_owned);
            route.attr.input_schema = tool_schema::portable_input(&route.attr.input_schema);
        }
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
    pub fn with_kubernetes(self, client: Client, control_namespace: impl Into<String>) -> Self {
        self.with_runtime(proofstorm_app::Runtime::new(
            client,
            control_namespace.into(),
        ))
    }

    #[must_use]
    pub fn with_runtime(mut self, runtime: proofstorm_app::Runtime) -> Self {
        self.kubernetes = Some(KubernetesRuntime {
            client: runtime.client,
            control_namespace: runtime.control_namespace,
            cluster_source: runtime.cluster_source,
            candidate_registry: proofstorm_app::installation::CATALOG_REGISTRY.into(),
            installation: None,
        });
        self
    }

    /// Route candidate pushes to this installation's registry. Containerd's
    /// pull mirror cannot redirect a `BuildKit` push to the canonical hostname.
    #[must_use]
    pub fn with_installation_runtime(
        self,
        runtime: proofstorm_app::Runtime,
        installation: &proofstorm_app::installation::Installation,
    ) -> Self {
        let mut service = self.with_runtime(runtime);
        if let Some(runtime) = &mut service.kubernetes {
            runtime.candidate_registry = format!("{}:5000", installation.registry_name());
            runtime.installation = Some(installation.clone());
        }
        service
    }

    /// Explicit offline authoring and cached reads; no runtime commands are advertised.
    #[must_use]
    pub fn offline(mut self) -> Self {
        for tool in proofstorm_core::mcp::TOOLS {
            if tool.requires_runtime {
                self.tool_router.disable_route(tool.name);
            }
        }
        self
    }

    fn require_wallet_action(
        &self,
        revision: &PublishedRevision,
        wallet: &str,
        control: &str,
    ) -> Result<(), ErrorData> {
        let component = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == wallet)
            .ok_or_else(|| {
                coded_invalid_request("component_id_unknown", "wallet is not in the revision")
            })?;
        if component.implementation == "nutshell-wallet" {
            return Ok(());
        }
        let catalog = self
            .store
            .effective_catalog(&self.workspace, &self.principal)
            .map_err(store_error)?;
        require_component_runtime_control(revision, wallet, "component", control, &catalog)
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

    async fn full_cell_status(&self, instance_id: &str) -> Result<CellInstanceStatus, ErrorData> {
        self.cells()?.status(instance_id).await.map_err(app_error)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "evidence admission, selection, and final size checks stay visibly atomic"
    )]
    fn build_evidence_bundle(
        &self,
        request: &EvidenceExportRequest,
    ) -> Result<EvidenceBundle, ErrorData> {
        self.authorize_all(&[Capability::ExperimentRead, Capability::ArtifactRead])?;
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
            .sealed_run_context(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        let mut actions = Vec::new();
        let mut after = 0;
        loop {
            let page = self
                .store
                .actions(
                    &self.workspace,
                    &self.principal,
                    &request.experiment_id,
                    after,
                    EVIDENCE_ACTION_PAGE_SIZE,
                )
                .map_err(store_error)?;
            let Some(last) = page.last() else {
                break;
            };
            after = last.sequence;
            let complete = page.len() < EVIDENCE_ACTION_PAGE_SIZE as usize;
            actions.extend(page);
            if complete {
                break;
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
        let workspace_captures = self
            .store
            .workspace_captures(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        let revisions = actions
            .iter()
            .map(|a| a.revision_digest.as_str())
            .chain(
                workspace_captures
                    .iter()
                    .map(|capture| capture.content.revision_digest.as_str()),
            )
            .filter(|d| !d.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|digest| {
                self.store
                    .revision_for_evidence(&self.workspace, &self.principal, digest)
                    .map_err(store_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let content = EvidenceBundleContent {
            workspace_captures,
            revisions,
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
        Ok(EvidenceBundle::from_content(content))
    }
}

#[derive(Clone)]
struct KubernetesRuntime {
    client: Client,
    control_namespace: String,
    cluster_source: String,
    candidate_registry: String,
    installation: Option<proofstorm_app::installation::Installation>,
}

#[tool_router(router = tool_router)]
impl ProofstormMcp {
    fn cells(&self) -> Result<proofstorm_app::cell::Cells, ErrorData> {
        let runtime = self.runtime()?;
        Ok(proofstorm_app::cell::Cells::new(
            self.store.clone(),
            runtime.shared(),
            self.workspace.clone(),
            self.principal.clone(),
        )
        .with_installation(runtime.installation.clone()))
    }

    #[tool(
        name = "cell_up",
        description = "Start or live-edit a named cell, preserving unchanged components. Supply exactly one of cell, patch or plan, plus request_id. Returns a small acceptance receipt; acceptance does not mean ready. Read cell_inspect for desired_generation and instance_key, and pass them as expected_generation/expected_instance_key to fence an edit. Keep the entire request unchanged for an exact retry. Omit preconditions for creation. Publication and materialization are resumable. Backend links require flat kind-specific fields: chain_backend network; payment_backend method/unit; database_backend role; authentication_backend protocol. Canonical cell_read specifications also work. Use cell_component_status_list for readiness, cell_search for configuration and activity_search for history."
    )]
    async fn proofstorm_cell_up(
        &self,
        Parameters(request): Parameters<SubmissionRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let request_id = request.request_id.clone();
        let preview = self.prepare_submission(request, true)?;
        self.store
            .bind_cell_submission(&self.workspace, &self.principal, &request_id, &preview)
            .map_err(store_error)?;
        self.cells()?
            .up_preview(&preview)
            .await
            .map_err(app_error)
            .and_then(cell_up::result)
    }

    #[tool(
        name = "environment_read",
        description = "Read the current workspace environment. Use scan=true for compact cell headers; filter name, owner, observed phase, component_kind or implementation, or search header JSON with query/regex. Select sections (components, links, resources, sessions, activity) or RFC 6901 fields; only needed sections are loaded. Keep selectors unchanged with cursor; detail sections and their cursors require instance_id. Runtime can be stale or unavailable; consult runtime.state. Omitted sections were not requested. No commands, sessions or synchronization occur. GET /v1/environment shares these selectors; no selectors preserves the full GUI/CLI view."
    )]
    async fn proofstorm_environment_read(
        &self,
        Parameters(request): Parameters<EnvironmentRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_all(
            proofstorm_core::mcp::tool("environment_read")
                .expect("public tool")
                .capabilities,
        )?;
        if request.runs.is_some() {
            return self.run_directory(&request);
        }
        let mut value = self
            .cells()?
            .environment_read(&request.cells, MAX_AGENT_RESPONSE_BYTES / 4)
            .await
            .map_err(app_error)?;
        value["workspace"] = serde_json::json!(
            self.store
                .workspace(&self.workspace, &self.principal)
                .map_err(store_error)?
        );
        value["capabilities"] = serde_json::json!(
            self.store
                .capabilities(&self.workspace, &self.principal)
                .map_err(store_error)?
        );
        developer_result(value)
    }

    #[tool(
        name = "cell_inspect",
        description = "Read a compact named-cell summary. desired_generation fences live edits; cell.incarnation_generation is a name-handle counter that can reset after teardown; instance_key identifies the incarnation. Select detailed view fields with RFC 6901 pointers, e.g. /runtime/blockers. For larger datasets use cell_search, cell_component_status_list, activity_search or session_list; operation_read retrieves receipt fields and text slices. No commands or receipt synchronization; use cell_sync for fresh action results."
    )]
    async fn proofstorm_cell_inspect(
        &self,
        Parameters(request): Parameters<CellInspectRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        read_query::validate_fields(&request.fields)?;
        self.cells()?
            .inspect(&request.name, request.after_sequence)
            .await
            .map_err(app_error)
            .and_then(|view| cell_inspect::result(view, &request.fields))
    }

    #[tool(
        name = "cell_exec",
        description = "Run one native argv command in a named cell without managing experiment or session IDs. Reuse request_id for an exact retry, and choose a new request_id for a new action. Returns operation_id: pass that exact value to operation_wait, operation_status or operation_cancel. Output defaults to private. Check native exit_code, cleanup_verified and projection_succeeded; phase alone does not describe command success or payment settlement."
    )]
    async fn proofstorm_cell_exec(
        &self,
        Parameters(request): Parameters<CellExecRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize(Capability::ComponentExecLive)?;
        proofstorm_core::native::NativeCommand {
            private_io: None,
            script: request.script.clone(),
            argv: request.argv.clone(),
            timeout_seconds: request.timeout_seconds,
            output: request.output.clone(),
        }
        .validate()
        .map_err(native_command_error)?;
        let instance_id = self.resolve_reference(&request.name, Capability::ComponentExecLive)?;
        let operation = self
            .execute_native(Parameters(NativeExecutionRequest {
                instance_id,
                experiment_id: request.run_id,
                session_id: String::new(),
                operation_id: request.request_id.clone(),
                idempotency_key: request.request_id,
                component: request.component,
                script: request.script,
                argv: request.argv,
                private_payload: request.private_payload,
                timeout_seconds: request.timeout_seconds,
                output: request.output,
            }))
            .await?
            .0;
        operation_result(operation)
    }

    #[tool(
        name = "workspace_task",
        description = "Start, inspect, stop, list or read logs of cell-owned workspace tasks. A task outlives this call and agent disconnection; timeout_seconds omitted means until stopped. Start snapshots the source directory (default src); same task_id and identical input return the original task even after interruption, changed input is refused. Tasks are never automatically replayed after a workspace restart. Returns a bounded control operation: operation_wait then inspect exit_code and stdout JSON for task state. A successful start is not task completion. Use a new request_id for each fresh read. Logs explicitly expose raw output. Stopping the control operation does not stop the task; use task action stop and wait for terminal task state. Optional control.components grants native calls through $PROOFSTORM_CONTROL workspace call with JSON {call_id,component,command}. Reuse call_id only for exact retries; receipts are output/<task_id>/control/<call_id>.json. control.lifecycle grants component start/stop/restart; control.network grants exact temporary partition pairs. Typed calls use {call_id,operation:{kind,...}}. Lifecycle needs component.control; partitions need network.partition and network.heal. Partitions heal on task exit or expiry; task status control_cleanup reports pending_faults and observed task_phase. Lifecycle effects persist."
    )]
    async fn proofstorm_workspace_task(
        &self,
        Parameters(request): Parameters<WorkspaceTaskRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.cells()?
            .workspace_request(
                &request.name,
                &request.component,
                &proofstorm_core::workspace::WorkspaceRequest::Task(request.task),
                &request.request_id,
            )
            .await
            .map_err(app_error)
            .and_then(operation_result)
    }

    #[tool(
        name = "workspace_capture",
        description = "Attach an immutable workspace task snapshot to an open run in the same cell. selection contains task_id, optional output_paths relative to output/<task_id>, and include_logs (default false). Includes captured source, task command/environment, task state, control mailbox and current controller receipts; selected files and logs are raw data shared with run readers. Does not stop or wait for the task. Files are observed sequentially; this is not an atomic application checkpoint. Retry the entire request unchanged for the same capture. Finish after its receipt, then evidence_export includes captures automatically. Download its resource before cell removal, which purges local run history. Returns metadata only; use evidence_section_read section workspace_capture with capture_id and a JSON pointer after run_finish. Limits: 64 output paths, 4096 files, 24 MiB file content and 40 MiB total capture. Missing, linked, oversized or changing selected files fail the capture."
    )]
    async fn proofstorm_workspace_capture(
        &self,
        Parameters(request): Parameters<proofstorm_app::cell::WorkspaceCaptureRequest>,
    ) -> Result<Json<proofstorm_app::cell::WorkspaceCaptureReceipt>, ErrorData> {
        self.authorize_all(&[
            Capability::ComponentExecLive,
            Capability::ArtifactRead,
            Capability::ExperimentRead,
        ])?;
        let id = request.capture_id(&self.workspace, &self.principal);
        if let Some(previous) = self
            .store
            .workspace_capture(
                &self.workspace,
                &self.principal,
                &id,
                &digest_json(&request),
            )
            .map_err(store_error)?
        {
            return Ok(Json((&previous).into()));
        }
        self.cells()?
            .workspace_capture(&request)
            .await
            .map(Json)
            .map_err(app_error)
    }

    #[tool(
        name = "workspace_upload",
        description = "Upload a local file into a workspace component without putting its contents in MCP arguments. source_path must be readable on the MCP server host; relative paths use the server working directory (the attached project for managed agents). path is relative to /workspace, usually src/script.py. Supports binary files up to 16 MiB and preserves whether the source is executable. The destination is replaced atomically after size and SHA-256 verification. Returns a recorded operation: operation_wait, then inspect exit_code and stdout JSON for path, bytes and sha256. Retry the same request_id with unchanged destination, bytes and executable permission; a changed file needs a new request_id. Interrupted staging leaves the destination unchanged and can be retried. Files survive workspace restarts and are deleted with the cell."
    )]
    async fn proofstorm_workspace_upload(
        &self,
        Parameters(request): Parameters<proofstorm_app::cell::WorkspaceUploadRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.cells()?
            .workspace_upload(&request)
            .await
            .map_err(app_error)
            .and_then(operation_result)
    }

    #[tool(
        name = "workspace_file",
        description = "Write, read, list or remove workspace files. Paths are relative to /workspace; use src for editable code, data for shared state, output/<task_id> for results. Files survive workspace restarts and are deleted with the cell. Writes replace a UTF-8 file atomically (8192 bytes maximum); reads return 1024-byte slices with next_offset; lists paginate with next_after. Symlinks, traversal and supervisor state are refused. Returns a bounded operation; operation_wait then read stdout JSON. Read explicitly exposes file contents. Use a new request_id for each fresh read. Use workspace_upload for local scripts and binary files up to 16 MiB."
    )]
    async fn proofstorm_workspace_file(
        &self,
        Parameters(request): Parameters<WorkspaceFileRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.cells()?
            .workspace_request(
                &request.name,
                &request.component,
                &proofstorm_core::workspace::WorkspaceRequest::File(request.file),
                &request.request_id,
            )
            .await
            .map_err(app_error)
            .and_then(operation_result)
    }

    #[tool(
        name = "cell_sync",
        description = "Synchronize runtime receipts into durable activity for a named cell, without executing a new action. Returns runtime status and a byte-bounded activity page. Use activity_search for targeted recorded results and `operation_read` for receipt fields or text slices. Pass next_sequence as after_sequence to continue until next_sequence is null. Unknown outcomes remain explicit."
    )]
    async fn proofstorm_cell_sync(
        &self,
        Parameters(request): Parameters<CellSyncRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let cells = self.cells()?;
        cells.sync(&request.name).await.map_err(app_error)?;
        cells
            .inspect(&request.name, request.after_sequence)
            .await
            .map_err(app_error)
            .and_then(cell_sync::result)
    }

    #[tool(
        name = "cell_remove",
        description = "Finish a named cell: revoke actions, collect owned work and verify teardown. Export evidence first. Returns complete and a verified teardown_receipt. Each call waits at most 30 seconds; complete=false is progress, so repeat with the same name and expected_instance_key. Exact retries verify absence even after records are removed. A replaced cell is never closed by an old request."
    )]
    async fn proofstorm_cell_remove(
        &self,
        Parameters(request): Parameters<CellRemoveRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize(Capability::CellClose)?;
        if request.timeout_seconds == 0 {
            return Err(invalid_operation("timeout_seconds must be positive"));
        }
        let cells = self.cells()?;
        let wait_seconds = request.timeout_seconds.min(30);
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(u64::from(wait_seconds));
        let result = tokio::time::timeout_at(
            deadline,
            cells.down_checked(
                &request.name,
                wait_seconds,
                Some(&request.expected_instance_key),
            ),
        )
        .await;
        let receipt = match result {
            Ok(Ok(view)) => view.runtime.and_then(|status| status.teardown_receipt),
            Ok(Err(error)) if error.kind == proofstorm_app::ErrorKind::Missing => {
                // The shared waiter already fences replacement identities and
                // verifies both the exact runtime resource and namespace.
                let verified = tokio::time::timeout_at(
                    deadline,
                    cells.wait(proofstorm_app::cell::WaitRequest {
                        reference: &request.name,
                        expected_instance_key: Some(&request.expected_instance_key),
                        expected_generation: None,
                        target_phase: InstancePhase::Closed,
                        timeout_seconds: wait_seconds,
                    }),
                )
                .await;
                match verified {
                    Ok(result) => result.map_err(app_error)?.status.teardown_receipt,
                    Err(_) => None,
                }
            }
            Ok(Err(error))
                if error
                    .details
                    .as_ref()
                    .and_then(|details| details["code"].as_str())
                    == Some("cell_close_pending") =>
            {
                None
            }
            Ok(Err(error)) => return Err(app_error(error)),
            Err(_) => None,
        };
        let complete = receipt
            .as_ref()
            .is_some_and(|receipt| receipt.verified_absent);
        developer_result(CellRemoveReceipt {
            name: request.name,
            instance_key: request.expected_instance_key,
            complete,
            timed_out: !complete,
            teardown_receipt: receipt,
            next_tool: (!complete).then(|| "cell_remove".into()),
        })
    }

    #[tool(
        name = "catalog_list",
        description = "Search visible component images by literal text or bounded regex, kind and origin. query: cdk matches the CDK family; implementations selects exact IDs. Filters combine before pagination; scan and JSON-pointer fields keep reads compact. Read exact entry details only for components you select; read a config schema only for constraints on a non-default field"
    )]
    fn proofstorm_catalog_list(
        &self,
        Parameters(request): Parameters<CatalogListRequest>,
    ) -> Result<Json<proofstorm_view::CatalogPage>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        let response = proofstorm_app::catalog::read(
            &self.store,
            &self.workspace,
            &self.principal,
            &request,
            MAX_AGENT_RESPONSE_BYTES,
        )
        .map_err(app_error)?;
        bounded_json_response(response).map(Json)
    }

    #[tool(
        name = "catalog_entry_read",
        description = "Read exact authoring metadata for one selected component version: compatibility, immutable image, controls, authorable and required config fields, and safe defaults. Read its config schema only for constraints on a non-default field"
    )]
    fn proofstorm_catalog_entry_read(
        &self,
        Parameters(request): Parameters<CatalogEntryRequest>,
    ) -> Result<Json<CatalogEntryDetail>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        let catalog = self
            .store
            .effective_catalog(&self.workspace, &self.principal)
            .map_err(store_error)?;
        let entry = exact_catalog_entry(&catalog.entries, &request.id, &request.version)?;
        let preferred = catalog.implementations.iter().any(|support| {
            support.implementation == entry.id
                && support.preferred_version.as_deref() == Some(entry.version.as_str())
        });
        bounded_json_response(CatalogEntryDetail::from_entry(entry, preferred)).map(Json)
    }

    #[tool(
        name = "catalog_config_schema_read",
        description = "Read the complete configuration JSON Schema or one RFC 6901 fragment for an exact installed component version"
    )]
    fn proofstorm_catalog_config_schema_read(
        &self,
        Parameters(request): Parameters<CatalogConfigSchemaRequest>,
    ) -> Result<Json<CatalogConfigSchemaResponse>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        let catalog = self
            .store
            .effective_catalog(&self.workspace, &self.principal)
            .map_err(store_error)?;
        catalog_config_schema_with_catalog(request, &catalog)
            .and_then(bounded_json_response)
            .map(Json)
    }

    #[tool(
        name = "candidate_build",
        description = "Build a durable candidate mint or wallet from a public GitHub PR, full commit SHA or release tag; then wait. Uses container toolchains and inherits an unverified baseline contract. One CDK mint build serves cdk, cdk-ldk and cdk-bdk. On success, copy a returned catalog_entries selector into a cell_plan component; catalog_entry is the requested preset"
    )]
    async fn proofstorm_candidate_build(
        &self,
        Parameters(request): Parameters<CandidateBuildRequest>,
    ) -> Result<Json<CandidateBuildReceipt>, ErrorData> {
        let runtime = self.runtime()?;
        let candidate = proofstorm_app::candidate::admit(
            &self.store,
            &self.workspace,
            &self.principal,
            &request,
        )
        .await
        .map_err(app_error)?;
        if !candidate.phase.terminal() {
            runtime.apply_candidate_build(&candidate).await?;
        }
        Ok(Json(compact_candidate_build(&candidate, false)))
    }

    #[tool(
        name = "candidate_wait",
        description = "Wait up to 120 seconds for a candidate build; repeat after timeout"
    )]
    async fn proofstorm_candidate_wait(
        &self,
        Parameters(request): Parameters<CandidateWaitRequest>,
    ) -> Result<Json<CandidateBuildReceipt>, ErrorData> {
        validate_wait_timeout(request.timeout_seconds)?;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(u64::from(request.timeout_seconds));
        let mut backoff = std::time::Duration::from_millis(250);
        loop {
            let candidate = self
                .store
                .candidate_build(&self.workspace, &self.principal, &request.candidate_id)
                .map_err(store_error)?;
            let candidate = self.sync_candidate_build(candidate).await?;
            if candidate.phase.terminal() {
                return Ok(Json(compact_candidate_build(&candidate, false)));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Json(compact_candidate_build(&candidate, true)));
            }
            tokio::time::sleep_until((tokio::time::Instant::now() + backoff).min(deadline)).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(4));
        }
    }

    #[tool(
        name = "candidate_list",
        description = "Search recorded candidate builds by id, phase or literal/regex text. Use scan, selected fields and a bound cursor for bounded reads. candidate_wait refreshes build status; listing never starts or changes a build."
    )]
    fn proofstorm_candidate_list(
        &self,
        Parameters(request): Parameters<DirectoryQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        self.candidate_directory(&request)
    }

    #[tool(
        name = "candidate_cancel",
        description = "Cancel a pending or running durable candidate build"
    )]
    async fn proofstorm_candidate_cancel(
        &self,
        Parameters(request): Parameters<CandidateCancelRequest>,
    ) -> Result<Json<CandidateBuildReceipt>, ErrorData> {
        self.authorize_all(&[Capability::CandidateCancel, Capability::CandidateRead])?;
        let candidate = self
            .store
            .candidate_build(&self.workspace, &self.principal, &request.candidate_id)
            .map_err(store_error)?;
        if !candidate.phase.terminal() {
            let cancel_token = digest_json(&(
                "proofstorm/candidate-cancel/v1",
                &candidate.id,
                &candidate.request_digest,
            ));
            self.runtime()?
                .request_candidate_cancellation(&candidate, &cancel_token)
                .await?;
        }
        let candidate = self.sync_candidate_build(candidate).await?;
        Ok(Json(compact_candidate_build(&candidate, false)))
    }

    #[tool(
        name = "network_capabilities",
        description = "Discover the installed network-fault backend, features, directions, and bounds"
    )]
    fn proofstorm_network_capabilities(&self) -> Result<Json<NetworkFaultBackend>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        Ok(Json(network_policy_fault_backend()))
    }

    #[tool(
        name = "cell_plan",
        description = "Preview a full specification or up to 100 stable-ID patch operations without changing the runtime. Uses the same validation as cell_up. Existing cells require expected_generation and expected_instance_key from cell_inspect. Returns an immutable plan reference, change counts and digests. Search/read large plans with plan_id; apply the exact reference with cell_up. Reuse request_id only for identical input."
    )]
    fn proofstorm_cell_plan(
        &self,
        Parameters(request): Parameters<SubmissionRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        submission::receipt(&self.prepare_submission(request, false)?)
    }

    #[tool(
        name = "cell_read",
        description = "Read exact configuration pointers or Unicode/array slices from name or immutable plan_id. Bind expected_digest to cell_digest. Large objects return a scan of child paths and sizes; follow next_offset. Paths from cell_search are directly readable."
    )]
    fn proofstorm_cell_read(
        &self,
        Parameters(request): Parameters<CellReadRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.read_cell_document(&request)
    }

    #[tool(
        name = "cell_search",
        description = "Search an immutable preview or live desired topology without loading the entire document. Supports exact ID filters, literal/regex matching across component/link JSON, scan mode for IDs/paths/sizes, JSON-pointer fields, exact match counts and snapshot-bound pagination. Use this for large cells, finding configuration values, or locating links by endpoint. No runtime commands or changes are made."
    )]
    fn proofstorm_cell_search(
        &self,
        Parameters(request): Parameters<CellSearchRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let document = self.cell_document(&request.target)?;
        developer_result(cell_search::search(document, &request)?)
    }

    #[tool(
        name = "cell_component_status_list",
        description = "Search live component readiness and startup failures. Filter component/ready, use literal or regex query, scan for IDs/kinds/readiness, or select fields with JSON pointers. Full MCP responses are byte-bounded. Cursors bind filters, revision and matching IDs; readiness may change without changing membership. Image-pull failures are blocked startup, not build progress"
    )]
    async fn proofstorm_cell_component_status_list(
        &self,
        Parameters(request): Parameters<CellComponentStatusListRequest>,
    ) -> Result<Json<CellComponentStatusListResponse>, ErrorData> {
        status_search::components(self.full_cell_status(&request.instance_id).await?, &request)
            .map(Json)
    }

    #[tool(
        name = "cell_inventory_list",
        description = "Search sanitized Kubernetes inventory in byte-bounded pages. Filter kind/namespace, search literal or regex query, and select fields with JSON pointers. Keep filters unchanged when continuing a cursor"
    )]
    async fn proofstorm_cell_inventory_list(
        &self,
        Parameters(request): Parameters<CellInventoryListRequest>,
    ) -> Result<Json<CellInventoryListResponse>, ErrorData> {
        status_search::inventory(self.full_cell_status(&request.instance_id).await?, &request)
            .map(Json)
    }

    #[tool(
        name = "cell_wait",
        description = "Wait for a cell to reach a target phase. A ready wait returns early with blockers when image pulls, scheduling or container startup are failing; reached=false and timed_out=false means blocked, not still loading. Inspect blocker reasons and recovery messages instead of repeating waits. timeout_seconds must be 1..=120"
    )]
    async fn proofstorm_cell_wait(
        &self,
        Parameters(request): Parameters<CellWaitRequest>,
    ) -> Result<Json<CellWaitResult>, ErrorData> {
        validate_wait_timeout(request.timeout_seconds)?;
        let waited = self
            .cells()?
            .wait(proofstorm_app::cell::WaitRequest {
                reference: &request.instance_id,
                expected_instance_key: request.expected_instance_key.as_deref(),
                expected_generation: request.expected_generation,
                target_phase: request.target_phase,
                timeout_seconds: request.timeout_seconds,
            })
            .await
            .map_err(app_error)?;
        let mut result = compact_cell_wait(
            waited.status,
            request.target_phase,
            waited.reached,
            waited.timed_out,
        );
        result.superseded = waited.superseded;
        if waited.superseded {
            result.message = Some("The requested generation was superseded; inspect the current cell before waiting again.".into());
        }
        Ok(Json(result))
    }

    #[tool(
        name = "run_start",
        description = "Start an optional evidence run bound to one cell. Omit run_id on ordinary commands for automatic grouping. Finishing this run seals its evidence while the cell continues running."
    )]
    fn proofstorm_run_start(
        &self,
        Parameters(mut request): Parameters<RunStartRequest>,
    ) -> Result<Json<Experiment>, ErrorData> {
        self.authorize(Capability::ExperimentCreate)?;
        request.instance_id =
            self.resolve_reference(&request.instance_id, Capability::ExperimentCreate)?;
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

    #[tool(
        name = "run_read",
        description = "Read a run summary with optional JSON-pointer fields"
    )]
    fn proofstorm_run_read(
        &self,
        Parameters(request): Parameters<RunReadRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        read_query::validate_fields(&request.fields)?;
        let run = self
            .store
            .experiment(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        developer_result(read_query::project(
            &serde_json::json!(run),
            &request.fields,
        ))
    }

    #[tool(
        name = "run_finish",
        description = "Finish a run after its actions are terminal. Proofstorm first reconciles completed runtime actions into the journal; if any are still active, wait for the returned operation IDs. Finalization order: operation waits, run_finish, evidence_export; sessions do not block finalization"
    )]
    async fn proofstorm_run_finish(
        &self,
        Parameters(request): Parameters<RunFinishRequest>,
    ) -> Result<Json<Experiment>, ErrorData> {
        self.authorize(Capability::ExperimentClose)?;
        let active = self
            .reconcile_experiment_operations(&request.experiment_id)
            .await?;
        if !active.is_empty() {
            return Err(coded_invalid_request(
                "run_actions_active",
                format!(
                    "Wait for active operations before finishing the run; use activity_search for the complete list. First operation IDs: {}",
                    active
                        .iter()
                        .take(16)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
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

    /// Fold any runtime actions that have already finished into the durable
    /// journal before experiment finalization. An agent should not need to
    /// rediscover a completed action merely because it omitted a status read.
    async fn reconcile_experiment_operations(
        &self,
        experiment_id: &str,
    ) -> Result<Vec<String>, ErrorData> {
        let mut after_sequence = 0;
        let mut active = Vec::new();
        loop {
            let actions = self
                .store
                .actions(
                    &self.workspace,
                    &self.principal,
                    experiment_id,
                    after_sequence,
                    100,
                )
                .map_err(store_error)?;
            if actions.is_empty() {
                break;
            }
            after_sequence = actions
                .last()
                .map_or(after_sequence, |action| action.sequence);
            for operation in actions.iter().filter(|operation| {
                matches!(
                    operation.phase,
                    OperationPhase::Pending | OperationPhase::Running
                )
            }) {
                let terminal = self.runtime()?.action_status(operation).await?;
                if let Some((phase, artifact)) = terminal {
                    self.record_runtime_terminal_result(operation, phase, artifact)?;
                } else {
                    active.push(operation.id.clone());
                }
            }
            if actions.len() < 100 {
                break;
            }
        }
        Ok(active)
    }

    #[tool(
        name = "session_list",
        description = "Search session records by cell, exact id, actor, run, phase or time. overlaps_with selects overlapping intervals. Use scan, literal/regex query, fields and cursor for bounded targeted reads. Follow next_cursor even on an empty search page. Active means unfinished tracking, not proof of agent liveness. Reads never refresh activity or block work."
    )]
    fn proofstorm_session_list(
        &self,
        Parameters(request): Parameters<SessionListRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        session_directory::read(&self.store, &self.workspace, &self.principal, &request)
    }
    #[tool(
        name = "private_access_issue",
        description = "Authorize a different principal to receive one private transfer using the exact approved wallet command. Independent of sessions. No broad cell permissions are granted; handoff must still bind the captured transfer."
    )]
    async fn proofstorm_private_access_issue(
        &self,
        Parameters(mut request): Parameters<PrivateAccessRequest>,
    ) -> Result<Json<proofstorm_core::PrivateAccessGrant>, ErrorData> {
        self.authorize(Capability::CellOperate)?;
        request.instance_id =
            self.resolve_reference(&request.instance_id, Capability::ComponentExecLive)?;
        request.receive.validate().map_err(invalid_operation)?;
        let scope = proofstorm_core::PrivateTransferScope {
            issuer_principal_id: self.principal.clone(),
            component: request.component,
            mint: request.mint,
            reference: request.reference,
            receive_command_digest: request.receive.digest(),
        };
        let grant = self
            .store
            .issue_private_access(
                &self.workspace,
                &self.principal,
                &request.recipient_principal_id,
                &request.recipient_grant_id,
                &request.instance_id,
                &scope,
                &request.idempotency_key,
            )
            .map_err(store_error)?;
        self.runtime()?.private_access(&grant).await?;
        Ok(Json(grant))
    }
    #[tool(
        name = "private_access_revoke",
        description = "Revoke one private-transfer permission. Does not finish or restrict any session."
    )]
    async fn proofstorm_private_access_revoke(
        &self,
        Parameters(request): Parameters<PrivateAccessIdRequest>,
    ) -> Result<Json<proofstorm_core::PrivateAccessGrant>, ErrorData> {
        let grant = self
            .store
            .revoke_private_access(&self.workspace, &self.principal, &request.grant_id)
            .map_err(store_error)?;
        self.runtime()?.private_access(&grant).await?;
        Ok(Json(grant))
    }
    #[tool(
        name = "private_access_read",
        description = "Read one private-transfer permission and its explicit revocation state."
    )]
    fn proofstorm_private_access_read(
        &self,
        Parameters(request): Parameters<PrivateAccessIdRequest>,
    ) -> Result<Json<proofstorm_core::PrivateAccessGrant>, ErrorData> {
        self.store
            .private_access(&self.workspace, &self.principal, &request.grant_id)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        name = "component_restart",
        description = "Restart any running cell component, whether its workload is a Deployment or StatefulSet, and wait for the exact accepted rollout to become ready. Use this for mints and wallets as well as Bitcoin and Lightning nodes"
    )]
    async fn proofstorm_component_restart(
        &self,
        Parameters(request): Parameters<ComponentControlRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.submit_component_control(request, OperationKind::ComponentRestart)
            .await
    }

    #[tool(
        name = "component_start",
        description = "Start any stopped cell component. Preserves its storage; poll operation_status for readiness."
    )]
    async fn proofstorm_component_start(
        &self,
        Parameters(request): Parameters<ComponentControlRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.submit_component_control(request, OperationKind::ComponentStart)
            .await
    }

    #[tool(
        name = "component_stop",
        description = "Stop any cell component without deleting its storage. The stop persists across cell edits and controller restarts; poll operation_status for completion."
    )]
    async fn proofstorm_component_stop(
        &self,
        Parameters(request): Parameters<ComponentControlRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.submit_component_control(request, OperationKind::ComponentStop)
            .await
    }

    #[tool(
        name = "component_logs",
        description = "Read current/previous logs, selecting a blocking initializer first. Reports log availability and serving pods. Works while unready; run/session attribution is automatic"
    )]
    async fn proofstorm_component_logs(
        &self,
        Parameters(mut request): Parameters<ComponentLogsRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.authorize(Capability::ComponentLogs)?;
        if !(1..=2_000).contains(&request.tail_lines) {
            return Err(invalid_operation("tail_lines must be in 1..=2000"));
        }
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::ComponentLogs,
        )?;

        let (instance, revision) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::ComponentLogs,
            )
            .map_err(store_error)?;
        let component = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this cell revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let operation = self.create_operation(
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
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
            CellAction::ComponentLogs(ComponentLogsAction {
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
        name = "component_forensics",
        description = "Run bounded offline forensics in a disposable pod built from a component's pinned image and declared data mounts. This is not the running component and does not promise its localhost, Unix sockets, process identity, or live CLI connectivity. Use it for source and database inspection; use cell_exec for a running component's native CLI"
    )]
    async fn proofstorm_component_forensics(
        &self,
        Parameters(mut request): Parameters<ComponentExecRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::ComponentForensics,
        )?;
        self.authorize(Capability::ComponentForensics)?;
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
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::ComponentForensics,
            )
            .map_err(store_error)?;
        let component = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this cell revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let target_component = request
            .target_component
            .as_deref()
            .unwrap_or(&request.component)
            .to_owned();
        let target = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == target_component)
            .ok_or_else(|| {
                invalid_operation("target_component is not part of this cell revision")
            })?;
        component_image_any(&revision, &target_component, target.kind)?;
        let operation = self.create_operation(
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
            &request.operation_id,
            OperationKind::ComponentForensics,
            &request,
            &request.idempotency_key,
            Capability::ComponentForensics,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            CellAction::ComponentForensics(ComponentForensicsAction {
                component: request.component,
                target_component,
                script: request.script,
                timeout_seconds: request.timeout_seconds,
            }),
        );
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        name = "private_transfer",
        description = "Reserve, inspect, deliver or release private byte custody between wallets with independent private-transfer permissions. prepare requires component, destinationComponent and maximumBytes; status/deliver/release require component and reference; handoff also requires recipientGrantId from private_access_issue, and binds a completed capture before delivery. Invalid input creates no operation. Returns an operation whose artifact contains metadata and an opaque reference. Use cell_exec.private_payload for native export/import. Delivery and native exit do not establish redemption."
    )]
    async fn proofstorm_private_transfer(
        &self,
        Parameters(mut request): Parameters<PrivateTransferRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.authorize(Capability::ComponentExecLive)?;
        let transfer = request.transfer.action()?;
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::ComponentExecLive,
        )?;

        let (instance, revision) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::ComponentExecLive,
            )
            .map_err(store_error)?;
        validate_private_transfer_endpoints(&transfer, &revision)?;
        let operation = self.create_operation(
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
            &request.operation_id,
            OperationKind::PrivateTransfer,
            &request,
            &request.idempotency_key,
            Capability::ComponentExecLive,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let mut action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            CellAction::PrivateTransfer(transfer),
        );
        action.spec.access_scope = self
            .store
            .operation_access_scope(&operation)
            .map_err(store_error)?;
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    async fn execute_native(
        &self,
        Parameters(request): Parameters<NativeExecutionRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.authorize(Capability::ComponentExecLive)?;
        proofstorm_core::native::NativeCommand {
            private_io: None,
            script: request.script.clone(),
            argv: request.argv.clone(),
            timeout_seconds: request.timeout_seconds,
            output: request.output.clone(),
        }
        .validate()
        .map_err(native_command_error)?;
        let (instance, revision) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::ComponentExecLive,
            )
            .map_err(store_error)?;
        let component = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this cell revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let operation = self.create_operation(
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
            &request.operation_id,
            OperationKind::ComponentExecLive,
            &request,
            &request.idempotency_key,
            Capability::ComponentExecLive,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let mut action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            CellAction::ComponentExecLive(ComponentExecLiveAction {
                private_payload: request.private_payload,
                component: request.component,
                script: request.script,
                argv: request.argv,
                timeout_seconds: request.timeout_seconds,
                output: request.output,
            }),
        );
        action.spec.access_scope = self
            .store
            .operation_access_scope(&operation)
            .map_err(store_error)?;
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the recipe matrix keeps one auditable, dependency-ordered experiment together"
    )]
    #[tool(
        name = "network_partition",
        description = "Bidirectionally partition two components with a durable bounded fault. Existing connections can survive; for immediate interruption use native disconnect or restart, then verify application state"
    )]
    async fn proofstorm_network_partition(
        &self,
        Parameters(mut request): Parameters<NetworkPartitionRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::NetworkPartition,
        )?;
        self.authorize(Capability::NetworkPartition)?;
        if request.from_component == request.to_component {
            return Err(invalid_operation("partition endpoints must be distinct"));
        }
        let (instance, revision) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::NetworkPartition,
            )
            .map_err(store_error)?;
        for component in [&request.from_component, &request.to_component] {
            if !revision
                .cell
                .components
                .iter()
                .any(|item| item.id == *component)
            {
                return Err(invalid_operation(&format!(
                    "component {component:?} is not part of this cell revision"
                )));
            }
        }
        let operation = self.create_operation(
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
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
            CellAction::NetworkPartition(NetworkPartitionAction {
                from_component: request.from_component,
                to_component: request.to_component,
            }),
        );
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        name = "network_heal",
        description = "Heal the durable network partition created by a prior operation"
    )]
    async fn proofstorm_network_heal(
        &self,
        Parameters(mut request): Parameters<NetworkHealRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::NetworkHeal,
        )?;
        self.authorize(Capability::NetworkHeal)?;
        let mut request = request;
        request.experiment_id = self
            .store
            .operation_run_id(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.experiment_id,
                Capability::NetworkHeal,
            )
            .map_err(store_error)?;
        let (instance, _) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
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
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
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
            CellAction::NetworkHeal(NetworkHealAction {
                partition_operation_id: request.partition_operation_id,
            }),
        );
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        name = "wallet_balance",
        description = "Read a sanitized wallet-local balance using its locked observation adapter; CDK uses a passive SQLite transaction and Nutshell uses a disposable snapshot"
    )]
    async fn proofstorm_wallet_balance(
        &self,
        Parameters(mut request): Parameters<WalletBalanceRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::WalletControl,
        )?;
        self.authorize(Capability::WalletControl)?;
        let (instance, revision) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::WalletControl,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.wallet, ComponentKind::Wallet)?;
        component_image_any(&revision, &request.mint, ComponentKind::Mint)?;
        self.require_wallet_action(&revision, &request.wallet, "wallet_balance")?;
        let operation = self.create_operation(
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
            &request.operation_id,
            OperationKind::WalletBalance,
            &request,
            &request.idempotency_key,
            Capability::WalletControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let mut action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            CellAction::WalletBalance(WalletBalanceAction {
                wallet: request.wallet.clone(),
                mint: request.mint.clone(),
            }),
        );
        action.spec.access_scope = self
            .store
            .operation_access_scope(&operation)
            .map_err(store_error)?;
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        name = "network_probe",
        description = "Observe bounded service reachability between two cell components using the source component's actual network-policy identity"
    )]
    async fn proofstorm_network_probe(
        &self,
        Parameters(mut request): Parameters<NetworkProbeRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::OracleRun,
        )?;
        self.authorize(Capability::OracleRun)?;
        validate_reachability_oracle_bounds(&request)?;
        let (instance, revision) = self
            .store
            .operation_context_for(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                &request.operation_id,
                Capability::OracleRun,
            )
            .map_err(store_error)?;
        if !revision
            .cell
            .components
            .iter()
            .any(|component| component.id == request.from_component)
        {
            return Err(invalid_operation(&format!(
                "component {:?} is not part of this cell revision",
                request.from_component
            )));
        }
        let destination = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.to_component)
            .ok_or_else(|| {
                invalid_operation(&format!(
                    "component {:?} is not part of this cell revision",
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
            &instance.revision_digest,
            &request.instance_id,
            &request.experiment_id,
            &request.session_id,
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
            CellAction::ReachabilityOracle(ReachabilityOracleAction {
                from_component: request.from_component.clone(),
                to_component: request.to_component.clone(),
                service: request.service.clone(),
                timeout_seconds: request.timeout_seconds,
                attempts: request.attempts,
            }),
        );
        if let Some(grant) = &action.spec.access_scope {
            self.runtime()?.private_access(grant).await?;
        }
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        name = "activity_search",
        description = "Search recorded operations across all actors and runs in a cell. Filter by component, phase, kind, actor, run/session and acceptance time; match literal/regex JSON scalar values. Returns operation IDs/digests, summaries, matching JSON pointers and excerpts, plus requested fields. Use operation_read for omitted values or more text. Newest first; scans at most 200 records per call. Continue next_cursor even if items is empty; null means exhausted. Cursors reject changed history or filters. Read-only: no runtime polling; use cell_sync first for fresh receipts."
    )]
    fn proofstorm_activity_search(
        &self,
        Parameters(request): Parameters<ActivitySearchRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_all(&[
            Capability::CellStatus,
            Capability::ExperimentRead,
            Capability::ArtifactRead,
        ])?;
        activity_search::search(&self.store, &self.workspace, &self.principal, &request)
    }

    #[tool(
        name = "operation_read",
        description = "Read a recorded operation's selected JSON pointer without runtime polling. Returns value and operation_digest. Copy expected_digest from activity_search to reject changed data. For strings and arrays, follow next_offset with the same digest and pointer; offsets count Unicode characters or array items. Null next_offset means complete. Objects must fit; select a deeper pointer if too large. Only already recorded data is available, including the original output visibility and truncation limits."
    )]
    fn proofstorm_operation_read(
        &self,
        Parameters(request): Parameters<OperationReadRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        operation_read::read(&self.store, &self.workspace, &self.principal, &request)
    }

    #[tool(
        name = "operation_status",
        description = "Refresh one operation and return compact status, native exit and cleanup facts. Large artifact bodies remain available through operation_read."
    )]
    async fn proofstorm_operation_status(
        &self,
        Parameters(request): Parameters<OperationRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let operation = self.refresh_operation(Parameters(request)).await?.0;
        operation_result(operation)
    }

    async fn refresh_operation(
        &self,
        Parameters(request): Parameters<OperationRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
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
        let completed = self.record_runtime_terminal_result(&operation, phase, artifact)?;
        Ok(Json(completed))
    }

    /// Validate adapter output before committing it to the canonical journal.
    /// Invalid terminal output is itself a terminal operation failure: it must
    /// never leave a completed runtime job occupying an active-operation slot.
    fn record_runtime_terminal_result(
        &self,
        operation: &CellOperation,
        phase: OperationPhase,
        artifact: serde_json::Value,
    ) -> Result<CellOperation, ErrorData> {
        proofstorm_app::journal::record(&self.store, &self.workspace, operation, phase, artifact)
            .map_err(app_error)
    }

    #[tool(
        name = "operation_wait",
        description = "Wait for independent operations together. Polling concurrency and complete response bytes are bounded; split large batches if compact receipts exceed the wire budget. Per-ID errors preserve other results. Native exit, cleanup, projection and truncation facts survive omitted artifact bodies; inspect native_result rather than phase alone. timeout_seconds must be 1..=120"
    )]
    async fn proofstorm_operation_wait(
        &self,
        Parameters(request): Parameters<OperationWaitRequest>,
    ) -> Result<Json<OperationWaitResult>, ErrorData> {
        use futures::StreamExt;

        validate_operation_wait_request(&request)?;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(u64::from(request.timeout_seconds));
        let mut backoff = std::time::Duration::from_millis(250);
        let mut last_operations = None;
        loop {
            let statuses = request
                .operation_ids
                .clone()
                .into_iter()
                .map(|operation_id| {
                    self.refresh_operation(Parameters(OperationRequest { operation_id }))
                });
            let (operations, errors) = match tokio::time::timeout_at(
                deadline,
                futures::stream::iter(statuses)
                    .buffered(8)
                    .collect::<Vec<_>>(),
            )
            .await
            {
                Ok(results) => partition_operation_results(&request.operation_ids, results),
                Err(_) => {
                    return last_operations.map_or_else(
                        || {
                            Err(coded_invalid_request(
                                "operation_wait_deadline_exceeded",
                                "the runtime action backend did not answer before the requested batch wait deadline",
                            ))
                        },
                        |(operations, errors)| compact_operation_wait_many(operations, errors, true).map(Json),
                    );
                }
            };
            if operations
                .iter()
                .all(|operation| operation_terminal(operation.phase))
            {
                return compact_operation_wait_many(operations, errors, false).map(Json);
            }
            last_operations = Some((operations.clone(), errors.clone()));
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return compact_operation_wait_many(operations, errors, true).map(Json);
            }
            tokio::time::sleep(backoff.min(deadline - now)).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(2));
        }
    }

    #[tool(
        name = "operation_cancel",
        description = "Request idempotent cancellation of an owned non-terminal action"
    )]
    async fn proofstorm_operation_cancel(
        &self,
        Parameters(request): Parameters<CancelOperationRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        operation_result(self.request_cancellation(Parameters(request)).await?.0)
    }

    async fn request_cancellation(
        &self,
        Parameters(request): Parameters<CancelOperationRequest>,
    ) -> Result<Json<CellOperation>, ErrorData> {
        self.authorize(Capability::ActionCancel)?;
        let operation = self
            .store
            .operation_for_cancel(&self.workspace, &self.principal, &request.operation_id)
            .map_err(store_error)?;
        if matches!(
            operation.phase,
            OperationPhase::Succeeded | OperationPhase::Failed | OperationPhase::Cancelled
        ) {
            self.finish_workspace_upload(&operation).await?;
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
        let (phase, artifact) = if operation.phase == OperationPhase::Pending {
            (
                OperationPhase::Cancelled,
                serde_json::json!({"code":"action_cancelled", "cancelled":true, "submitted":false}),
            )
        } else {
            (OperationPhase::Failed, missing_action_artifact(&operation))
        };
        let finalized = self
            .store
            .record_operation_result(&self.workspace, &operation.id, phase, artifact)
            .map_err(store_error)?;
        self.finish_workspace_upload(&finalized).await?;
        Ok(Json(finalized))
    }

    async fn finish_workspace_upload(&self, operation: &CellOperation) -> Result<(), ErrorData> {
        if proofstorm_app::cell::Cells::is_workspace_upload(operation) {
            self.cells()?
                .finish_workspace_upload(operation)
                .await
                .map_err(app_error)?;
        }
        Ok(())
    }

    #[tool(
        name = "evidence_export",
        description = "Export deterministic evidence for a closed experiment, including offline archives. Every action and artifact descriptor is in the journal; select optional full bodies with artifact_operation_ids. No journal-count or bundle-size admission cap. Bulk content stays at resource_uri; use evidence_section_read for bounded inspection"
    )]
    fn proofstorm_evidence_export(
        &self,
        Parameters(request): Parameters<EvidenceExportRequest>,
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
        name = "evidence_section_read",
        description = "Read one bounded semantic section of a closed experiment's deterministic evidence bundle. Use JSON Pointer for large revision, lock, or artifact documents"
    )]
    fn proofstorm_evidence_section_read(
        &self,
        Parameters(request): Parameters<EvidenceSectionReadRequest>,
    ) -> Result<Json<EvidenceSectionReadResponse>, ErrorData> {
        let export_request = EvidenceExportRequest {
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
                if read_query::wire_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
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
            EvidenceSection::WorkspaceCapture => {
                evidence_capture_section(&bundle, request.capture_id.as_deref(), &request.pointer)?
            }
            EvidenceSection::Journal => unreachable!("journal returned above"),
        };
        bounded_json_response(EvidenceSectionReadResponse {
            evidence_digest: bundle.digest,
            section: request.section,
            data,
            next_after_sequence: None,
        })
        .map(Json)
    }
}

impl CatalogEntryDetail {
    fn from_entry(entry: &CatalogEntry, preferred: bool) -> Self {
        Self {
            origin: entry.origin(),
            build_profile: entry.source.as_ref().map_or_else(
                || proofstorm_core::candidate_build_profile(&entry.id),
                |s| s.provenance.as_ref().map(|p| p.profile.clone()),
            ),
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
            runtime_endpoints: public_endpoints(&entry.runtime_endpoints),
            image: entry.image.clone(),
            source_digest: entry.source_digest.clone(),
            source: entry.source.clone(),
            allowed_control: entry.allowed_control.clone(),
            recommended_control: recommended_control(entry),
            authorable_config_fields: authorable_config_fields(entry),
            required_config_fields: required_config_fields(entry),
            config_defaults: config_defaults(entry),
        }
    }
}

struct TopologySummary {
    component_count: u32,
    link_count: u32,
    component_ids: Vec<String>,
    link_ids: Vec<String>,
    backend_link_count: u32,
    bound_backend_link_count: u32,
    topology_digest: String,
    warnings: Vec<String>,
}

fn topology_summary(cell: &CellSpec) -> TopologySummary {
    let mut components = cell.components.clone();
    components.sort_by(|left, right| left.id.cmp(&right.id));
    let mut links = cell.links.clone();
    links.sort_by(|left, right| left.id.cmp(&right.id));

    let component_ids = components
        .iter()
        .map(|component| component.id.clone())
        .collect::<Vec<_>>();
    let link_ids = links.iter().map(|link| link.id.clone()).collect::<Vec<_>>();
    let backend_links = links.iter().filter(|link| {
        matches!(
            link.kind,
            LinkKind::ChainBackend
                | LinkKind::PaymentBackend
                | LinkKind::DatabaseBackend
                | LinkKind::AuthenticationBackend
        )
    });
    let backend_link_count = backend_links.clone().count();
    let bound_backend_link_count = backend_links.filter(|link| link.binding.is_some()).count();
    let mut warnings = Vec::new();
    if components.is_empty() {
        warnings.push("empty_topology: the cell contains no components".into());
    } else if components.len() > 1 && links.is_empty() {
        warnings.push("disconnected_topology: multiple components have no links".into());
    }
    if backend_link_count != bound_backend_link_count {
        warnings.push(format!(
            "unbound_backend_links: {bound_backend_link_count}/{backend_link_count} carry typed bindings"
        ));
    }
    let payment_backends = links
        .iter()
        .filter(|link| link.kind == LinkKind::PaymentBackend)
        .map(|link| link.to.as_str())
        .collect::<BTreeSet<_>>();
    let intermediary_lightning = components.iter().any(|component| {
        component.kind == ComponentKind::Lightning
            && !payment_backends.contains(component.id.as_str())
    });
    let direct_backend_peers = links
        .iter()
        .filter(|link| {
            link.kind == LinkKind::LightningPeer
                && payment_backends.contains(link.from.as_str())
                && payment_backends.contains(link.to.as_str())
        })
        .map(|link| link.id.as_str())
        .collect::<Vec<_>>();
    if intermediary_lightning && !direct_backend_peers.is_empty() {
        warnings.push(format!(
            "direct_mint_backend_peer: link(s) {} bypass the available forwarding node; routing-fee experiments need backend <-> router <-> backend with no direct backend peer",
            direct_backend_peers.iter().take(16).copied().collect::<Vec<_>>().join(",")
        ));
    }
    let mint_count = components
        .iter()
        .filter(|component| component.kind == ComponentKind::Mint)
        .count();
    let wallet_count = components
        .iter()
        .filter(|component| component.kind == ComponentKind::Wallet)
        .count();
    if mint_count >= 2 && wallet_count < 2 {
        warnings.push(format!(
            "distinct_payment_wallets_required: {mint_count} mints but only {wallet_count} wallet component(s); bidirectional cross-mint payments require distinct payer and recipient wallet components"
        ));
    }

    TopologySummary {
        component_count: u32::try_from(components.len()).unwrap_or(u32::MAX),
        link_count: u32::try_from(links.len()).unwrap_or(u32::MAX),
        component_ids,
        link_ids,
        backend_link_count: u32::try_from(backend_link_count).unwrap_or(u32::MAX),
        bound_backend_link_count: u32::try_from(bound_backend_link_count).unwrap_or(u32::MAX),
        topology_digest: digest_json(&(components, links)),
        warnings,
    }
}

#[cfg(test)]
fn cell_validation_result(cell: &CellSpec) -> CellValidationResult {
    cell_validation_result_with_catalog(cell, default_catalog(), 0)
}

fn cell_validation_result_with_catalog(
    cell: &CellSpec,
    catalog: &CatalogResponse,
    issue_offset: usize,
) -> CellValidationResult {
    let mut validation = validate_cell(cell);
    if validation.valid {
        if let Err(message) = proofstorm_core::resolve_lock(cell, catalog) {
            validation.valid = false;
            validation.issues.push(ValidationIssue {
                code: "publication_preflight_failed".into(),
                path: "/".into(),
                message,
            });
        }
    }
    let summary = topology_summary(cell);
    let issue_count = validation.issues.len();
    let mut result = CellValidationResult {
        valid: validation.valid,
        component_count: cell.components.len(),
        link_count: cell.links.len(),
        issue_count,
        next_issue_offset: None,
        details_omitted: false,
        issues: validation.issues.into_iter().skip(issue_offset).collect(),
        component_ids: summary.component_ids,
        link_ids: summary.link_ids,
        warnings: summary.warnings,
    };
    if serialized_size(&result).is_ok_and(|size| size > MAX_AGENT_RESPONSE_BYTES) {
        result.component_ids.clear();
        result.link_ids.clear();
        result.details_omitted = true;
        // Keep at least one actionable issue per page, even if a backend
        // includes an unusually long native validation message.
        for issue in &mut result.issues {
            if issue.message.len() > 4096 {
                issue.message = format!(
                    "{} [message truncated]",
                    issue.message.chars().take(1024).collect::<String>()
                );
            }
        }
        let issues = std::mem::take(&mut result.issues);
        let mut available = MAX_AGENT_RESPONSE_BYTES
            .saturating_sub(serialized_size(&result).unwrap_or(MAX_AGENT_RESPONSE_BYTES) + 128);
        for issue in issues {
            let size = serialized_size(&issue).unwrap_or(MAX_AGENT_RESPONSE_BYTES) + 1;
            if size > available && !result.issues.is_empty() {
                break;
            }
            available = available.saturating_sub(size);
            result.issues.push(issue);
        }
    }
    let next = issue_offset.saturating_add(result.issues.len());
    if next < issue_count {
        result.next_issue_offset = Some(next);
    }
    result
}

fn evidence_capture_section(
    bundle: &EvidenceBundle,
    capture_id: Option<&str>,
    pointer: &str,
) -> Result<serde_json::Value, ErrorData> {
    let id = capture_id.ok_or_else(|| {
        coded_invalid_request(
            "evidence_capture_id_required",
            "capture_id is required for workspace capture reads",
        )
    })?;
    let capture = bundle
        .content
        .workspace_captures
        .iter()
        .find(|capture| capture.content.capture_id == id)
        .ok_or_else(|| {
            coded_invalid_request(
                "evidence_capture_unknown",
                "capture_id is not attached to this run",
            )
        })?;
    evidence_pointer(evidence_json(capture)?, pointer, "workspace_capture")
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
        workspace_capture_count: u32::try_from(bundle.content.workspace_captures.len()).unwrap_or(u32::MAX),
        journal_complete: true,
        artifact_bodies_optional: true,
        guidance: "Evidence is complete: the journal covers every action and artifact descriptor. Do not retry merely to make artifact_count equal journal_count; explicit artifact IDs only embed optional full bodies."
            .into(),
        resource_uri,
        content_included: include_content,
        content: include_content
            .then(|| serde_json::to_value(bundle.content).expect("typed evidence serializes")),
    }
}

fn evidence_resource_uri(request: &EvidenceExportRequest, digest: &str) -> String {
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

fn parse_evidence_resource_uri(uri: &str) -> Result<(EvidenceExportRequest, String), ErrorData> {
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
        EvidenceExportRequest {
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
        let mut parent = pointer;
        while value.pointer(parent).is_none() {
            parent = parent.rsplit_once('/').map_or("", |(prefix, _)| prefix);
        }
        let available = value.pointer(parent).expect("existing pointer ancestor")
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .take(16)
                    .map(|key| format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1")))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|keys| !keys.is_empty())
            .unwrap_or_else(|| "the empty pointer for the whole section".to_owned());
        coded_invalid_request(
            "evidence_pointer_not_found",
            format!(
                "[evidence_pointer_not_found] JSON Pointer {pointer:?} does not exist in the {section} section; no changes were made. Recovery: choose one of the available pointers beneath {parent:?} ({available}), or use an empty pointer to read the whole section"
            ),
        )
    })
}

fn recommended_control(entry: &CatalogEntry) -> ControlClass {
    [
        ControlClass::Target,
        ControlClass::Cell,
        ControlClass::Attacker,
        ControlClass::Oracle,
    ]
    .into_iter()
    .find(|control| entry.allowed_control.contains(control))
    .expect("catalog contract requires at least one allowed control")
}

fn authorable_config_fields(entry: &CatalogEntry) -> Vec<String> {
    let mut fields = entry
        .config_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    fields.sort();
    fields
}

fn required_config_fields(entry: &CatalogEntry) -> Vec<String> {
    let mut fields = entry
        .config_schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    fields.sort();
    fields
}

fn config_defaults(entry: &CatalogEntry) -> BTreeMap<String, serde_json::Value> {
    entry
        .config_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.iter())
        .filter_map(|(name, schema)| {
            schema
                .get("default")
                .cloned()
                .map(|default| (name.clone(), default))
        })
        .collect()
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
            let available_versions = entries
                .iter()
                .filter(|entry| entry.id == id)
                .map(|entry| entry.version.as_str())
                .collect::<Vec<_>>();
            let alternatives = if available_versions.is_empty() {
                "no versions are installed for that implementation".to_owned()
            } else {
                format!("available exact versions: {}", available_versions.join(", "))
            };
            ErrorData::resource_not_found(
                format!(
                    "[catalog_entry_not_found] catalog entry {id:?} version {version:?} was not found; no changes were made. Recovery: use one of the {alternatives}"
                ),
                Some(serde_json::json!({"code": "catalog_entry_not_found"})),
            )
        })
}

fn catalog_config_schema_with_catalog(
    request: CatalogConfigSchemaRequest,
    catalog: &CatalogResponse,
) -> Result<CatalogConfigSchemaResponse, ErrorData> {
    if !request.pointer.is_empty() && !request.pointer.starts_with('/') {
        return Err(coded_invalid_request(
            "catalog_schema_pointer_invalid",
            "configuration schema pointer must be empty or begin with '/'",
        ));
    }
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

fn bounded_json_response<T: Serialize>(value: T) -> Result<T, ErrorData> {
    bounded_agent_response(activity_search::wire(&value)?)?;
    Ok(value)
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
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        // Recheck the full contract on every call, including grants revoked after discovery.
        // Handlers retain their finer cell/component/private-custody checks.
        if let Some(tool) = proofstorm_core::mcp::tool(&request.name) {
            self.authorize_all(tool.capabilities)?;
        }
        let installation = self
            .kubernetes
            .as_ref()
            .and_then(|runtime| runtime.installation.as_ref());
        let runtime_tool =
            proofstorm_core::mcp::tool(&request.name).is_some_and(|tool| tool.requires_runtime);
        let _access =
            proofstorm_app::bootstrap::lifecycle::access(installation.filter(|_| runtime_tool))
                .map_err(|error| {
                    app_error(proofstorm_app::Error::problem(
                        "runtime_suspended",
                        error.to_string(),
                    ))
                })?;
        self.tool_router
            .call(rmcp::handler::server::tool::ToolCallContext::new(
                self, request, context,
            ))
            .await
    }

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
        .with_instructions("One Proofstorm toolset covers discovery, cells, native execution, lifecycle, faults and evidence. Read catalog entries for exact versions, configuration and native CLI guidance. Preview optionally with cell_plan; submit full specifications, stable-ID patches or bound plans through cell_up. Copy desired_generation and instance_key from cell_inspect to fence edits. Reuse the entire request and request_id for exact retries. Acceptance is separate from readiness: use cell_wait and component status. Use cell_exec for native commands; verify native exit, process cleanup and application effects separately. Stop/start/restart through component controls; use network_partition, network_probe and network_heal for faults. Sessions and runs are automatic; optional run_start/run_finish group sealed evidence without stopping the cell. Use environment_read and session_list to discover records; scan/filter/search first, then read exact configuration or operation pointers and slices. Follow continuations even on empty pages and retain digests. wallet_balance is a passive local checkpoint; native CLI balance remains available. Private transfers use opaque custody and explicit access grants. Export evidence after run_finish and before cell_remove; removal deletes cell-owned state. Permissions and runtime availability filter this same contract; no profile switching is required.")
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(
                "proofstorm://evidence/{run_id}/{digest}{?oracles,artifacts}",
                "proofstorm-evidence-bundle",
            )
            .with_title("Proofstorm evidence bundle")
            .with_description(
                "Complete deterministic evidence bundle identified by a manifest returned from evidence_export",
            )
            .with_mime_type("application/vnd.proofstorm.evidence.v1alpha1+json"),
            ResourceTemplate::new("proofstorm://candidate-build/{candidate_id}/record{?path,offset,digest}", "proofstorm-candidate-build-record")
                .with_description("Read frozen source, recipe and build evidence by JSON pointer; follow next_uri for complete text")
                .with_mime_type("application/json"),
            ResourceTemplate::new(
                "proofstorm://candidate-build/{candidate_id}/logs{?offset,digest}",
                "proofstorm-candidate-build-logs",
            )
            .with_title("Proofstorm candidate build logs")
            .with_description(
                "Retained source and BuildKit diagnostics, paged as text with next_uri; available after evidence capture",
            )
            .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri.starts_with("proofstorm://candidate-build/") {
            let url = reqwest::Url::parse(&request.uri)
                .map_err(|e| coded_invalid_request("candidate_resource_invalid", e.to_string()))?;
            let path = url
                .path()
                .trim_start_matches('/')
                .split('/')
                .collect::<Vec<_>>();
            if path.len() != 2 || !matches!(path[1], "logs" | "record") {
                return Err(coded_invalid_request(
                    "candidate_resource_invalid",
                    "Expected candidate ID followed by logs or record",
                ));
            }
            let mut query = proofstorm_app::candidate::CandidateReadQuery {
                id: path[0].into(),
                path: if path[1] == "logs" {
                    "/diagnostics".into()
                } else {
                    String::new()
                },
                ..Default::default()
            };
            for (key, value) in url.query_pairs() {
                match key.as_ref() {
                    "path" => query.path = value.into_owned(),
                    "offset" => {
                        query.offset = value.parse().map_err(|_| {
                            coded_invalid_request("candidate_resource_invalid", "Invalid offset")
                        })?;
                    }
                    "digest" => query.expected_digest = Some(value.into_owned()),
                    _ => {
                        return Err(coded_invalid_request(
                            "candidate_resource_invalid",
                            "Unknown resource selector",
                        ));
                    }
                }
            }
            let mut page = proofstorm_app::candidate::read(
                &self.store,
                &self.workspace,
                &self.principal,
                &query,
            )
            .map_err(app_error)?;
            if let Some(next) = page["next_offset"].as_u64() {
                let mut continuation = url;
                continuation.set_query(None);
                continuation
                    .query_pairs_mut()
                    .append_pair("path", &query.path)
                    .append_pair("offset", &next.to_string())
                    .append_pair("digest", page["digest"].as_str().unwrap_or_default());
                page["next_uri"] = serde_json::json!(continuation.as_str());
            }
            let text = serde_json::to_string(&page)
                .map_err(|e| coded_invalid_request("candidate_resource_invalid", e.to_string()))?;
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, request.uri).with_mime_type("application/json"),
            ])
            .into());
        }
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
    proofstorm_core::mcp::TOOLS
        .iter()
        .map(|tool| (tool.name, tool.capabilities))
        .collect()
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

    fn normalize_action(
        &self,
        name: &mut String,
        request_id: &str,
        key: &mut String,
        capability: Capability,
    ) -> Result<(), ErrorData> {
        self.authorize(capability)?;
        *name = self.resolve_reference(name, capability)?;
        request_id.clone_into(key);
        Ok(())
    }

    fn resolve_reference(&self, name: &str, capability: Capability) -> Result<String, ErrorData> {
        self.store
            .resolve_cell_reference_for(&self.workspace, &self.principal, name, capability)
            .map_err(store_error)
    }

    async fn submit_component_control(
        &self,
        mut request: ComponentControlRequest,
        kind: OperationKind,
    ) -> Result<CallToolResult, ErrorData> {
        self.normalize_action(
            &mut request.instance_id,
            &request.operation_id,
            &mut request.idempotency_key,
            Capability::ComponentControl,
        )?;
        self.cells()?
            .control_component(
                proofstorm_app::cell::ComponentControlRequest {
                    instance_id: request.instance_id,
                    experiment_id: request.experiment_id,
                    session_id: String::new(),
                    operation_id: request.operation_id,
                    component: request.component,
                    idempotency_key: request.idempotency_key,
                },
                kind,
            )
            .await
            .map_err(app_error)
            .and_then(developer_result)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the MCP-to-journal boundary passes every authority and immutable identity explicitly"
    )]
    fn create_operation<R: Serialize>(
        &self,
        expected_revision: &str,
        instance_id: &str,
        experiment_id: &str,
        session_id: &str,
        operation_id: &str,
        kind: OperationKind,
        request: &R,
        idempotency_key: &str,
        capability: Capability,
    ) -> Result<CellOperation, ErrorData> {
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
            .create_operation_at_revision(
                expected_revision,
                &self.workspace,
                &self.principal,
                instance_id,
                experiment_id,
                session_id,
                operation_id,
                kind,
                &value,
                idempotency_key,
                capability,
            )
            .map_err(store_error)
    }
}

fn component_image_any(
    revision: &PublishedRevision,
    id: &str,
    kind: ComponentKind,
) -> Result<String, ErrorData> {
    let valid_component_ids = || valid_component_ids(revision, kind, None);
    let Some(component) = revision
        .cell
        .components
        .iter()
        .find(|component| component.id == id)
    else {
        let valid_component_ids = valid_component_ids();
        return Err(ErrorData::invalid_request(
            format!(
                "component {id:?} is not in this revision; expected {}; valid component IDs: {valid_component_ids:?}",
                component_kind_name(kind)
            ),
            Some(serde_json::json!({
                "code": "component_id_unknown",
                "requested_id": id,
                "expected_kind": kind,
                "valid_component_ids": valid_component_ids,
            })),
        ));
    };
    if component.kind != kind {
        let valid_component_ids = valid_component_ids();
        return Err(ErrorData::invalid_request(
            format!(
                "component {id:?} is {}; expected {}; valid component IDs: {valid_component_ids:?}",
                component_kind_name(component.kind),
                component_kind_name(kind)
            ),
            Some(serde_json::json!({
                "code": "component_kind_mismatch",
                "requested_id": id,
                "actual_kind": component.kind,
                "expected_kind": kind,
                "valid_component_ids": valid_component_ids,
            })),
        ));
    }
    revision
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id && entry.catalog_id == component.implementation)
        .map(|entry| entry.image.clone())
        .ok_or_else(|| revision_integrity_error(id))
}

fn require_component_runtime_control(
    revision: &PublishedRevision,
    component_id: &str,
    endpoint_id: &str,
    control: &str,
    catalog: &CatalogResponse,
) -> Result<(), ErrorData> {
    let component = revision
        .cell
        .components
        .iter()
        .find(|component| component.id == component_id)
        .ok_or_else(|| {
            coded_invalid_request(
                "component_id_unknown",
                format!("component {component_id:?} is not in this revision"),
            )
        })?;
    let entry = exact_catalog_entry(
        &catalog.entries,
        &component.implementation,
        component.version.as_deref().unwrap_or_default(),
    )?;
    let endpoint = entry
        .runtime_endpoints
        .iter()
        .find(|endpoint| endpoint.id == endpoint_id)
        .ok_or_else(|| {
            coded_invalid_request(
                "runtime_endpoint_not_found",
                format!("component {component_id:?} has no runtime endpoint {endpoint_id:?}"),
            )
        })?;
    if endpoint.controls.contains(control) {
        return Ok(());
    }
    Err(ErrorData::invalid_request(
        format!(
            "component {component_id:?} endpoint {endpoint_id:?} cannot execute {control:?}; no operation was created. {}",
            endpoint.limitations.join("; ")
        ),
        Some(serde_json::json!({
            "code": "runtime_control_unsupported",
            "component_id": component_id,
            "implementation": entry.id,
            "endpoint": endpoint,
            "requested_control": control,
            "recovery": "choose a component endpoint that advertises the required runtime control",
        })),
    ))
}

fn valid_component_ids(
    revision: &PublishedRevision,
    kind: ComponentKind,
    implementation: Option<&str>,
) -> Vec<String> {
    let mut ids = revision
        .cell
        .components
        .iter()
        .filter(|component| {
            component.kind == kind
                && implementation.is_none_or(|expected| component.implementation == expected)
        })
        .map(|component| component.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

const fn component_kind_name(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Bitcoin => "bitcoin",
        ComponentKind::Lightning => "lightning",
        ComponentKind::Mint => "mint",
        ComponentKind::Database => "database",
        ComponentKind::IdentityProvider => "identity_provider",
        ComponentKind::Wallet => "wallet",
        ComponentKind::Attacker => "workspace",
        ComponentKind::Proxy => "proxy",
        ComponentKind::Oracle => "oracle",
    }
}

fn revision_integrity_error(component_id: &str) -> ErrorData {
    ErrorData::internal_error(
        format!(
            "published revision has no matching immutable lock entry for component {component_id:?}"
        ),
        Some(serde_json::json!({
            "code": "revision_integrity_error",
            "component_id": component_id,
        })),
    )
}

fn validate_reachability_oracle_bounds(request: &NetworkProbeRequest) -> Result<(), ErrorData> {
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

fn validate_operation_wait_request(request: &OperationWaitRequest) -> Result<(), ErrorData> {
    validate_wait_timeout(request.timeout_seconds)?;
    if request.operation_ids.is_empty() {
        return Err(ErrorData::invalid_request(
            "operation_ids must contain at least one ID".to_owned(),
            Some(serde_json::json!({
                "code": "operation_wait_count_invalid",
                "minimum": 1,
                "requested": request.operation_ids.len(),
            })),
        ));
    }
    let unique = request.operation_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != request.operation_ids.len() {
        return Err(ErrorData::invalid_request(
            "operation_ids must be unique".to_owned(),
            Some(serde_json::json!({
                "code": "operation_wait_duplicate_id",
            })),
        ));
    }
    Ok(())
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

fn component_status_identity(status: &CellInstanceStatus) -> String {
    let mut ids = status
        .components
        .iter()
        .map(|component| component.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    digest_json(&(
        &status.instance.instance_key,
        &status.instance.revision_digest,
        status.instance.generation,
        &status.observed_revision_digest,
        status.observed_generation,
        ids,
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeveloperCellView {
    pub instance_key: Option<String>,
    /// Desired configuration version, distinct from `cell.incarnation_generation`.
    pub desired_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconciliation_error: Option<CellReconciliationError>,
    pub cell: proofstorm_store::CellHandle,
    pub runtime: Option<CellStatusSummary>,
    pub run: Option<Experiment>,
    pub sessions: proofstorm_store::SessionPage,
    pub activity: Vec<proofstorm_app::cell::Activity>,
    pub next_sequence: Option<u64>,
    pub observed_at_unix: i64,
}

#[cfg(test)]
fn environment_result(
    mut view: proofstorm_app::environment::EnvironmentView,
) -> Result<CallToolResult, ErrorData> {
    // Text-only clients need the actual page, not a pointer to structuredContent.
    // Reserve room for both copies, JSON string escaping, and the MCP envelope.
    proofstorm_app::environment::bound_page_bytes(&mut view, MAX_AGENT_RESPONSE_BYTES / 4)
        .map_err(app_error)?;
    developer_result(view)
}

// Mutations return small receipts instead of duplicating full status and history.
fn developer_result(value: impl Serialize) -> Result<CallToolResult, ErrorData> {
    let value = serde_json::to_value(value)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    bounded_agent_response(CallToolResult::structured(value))
}

fn compact_developer_view(view: proofstorm_app::cell::CellView) -> DeveloperCellView {
    DeveloperCellView {
        instance_key: view.instance_key,
        desired_generation: view.desired_generation,
        reconciliation_error: view.reconciliation_error,
        cell: view.cell,
        runtime: view.runtime.map(|status| {
            let mut summary = compact_cell_status(status);
            summary.runtime_guidance = Some("Readiness describes infrastructure availability, not funding or payment settlement. Inspect individual components for recovery; execute native commands in the selected cell.".into());
            summary
        }),
        run: view.run,
        sessions: view.sessions,
        activity: view.activity,
        next_sequence: view.next_sequence,
        observed_at_unix: view.observed_at_unix,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartupBlocker {
    pub component_id: String,
    pub reason: proofstorm_core::ComponentConditionReason,
    pub message: String,
}

fn startup_blockers(status: &CellInstanceStatus) -> Vec<StartupBlocker> {
    status
        .components
        .iter()
        .filter_map(|component| {
            component
                .conditions
                .iter()
                .find(|condition| {
                    condition.state == proofstorm_core::ComponentConditionState::False
                        && condition.reason.blocks_startup()
                })
                .map(|condition| StartupBlocker {
                    component_id: component.id.clone(),
                    reason: condition.reason,
                    message: condition.message.clone(),
                })
        })
        .take(8)
        .collect()
}

fn compact_cell_status(mut status: CellInstanceStatus) -> CellStatusSummary {
    let blockers = startup_blockers(&status);
    if status.phase == InstancePhase::Pending && !blockers.is_empty() {
        status.message = Some("Component startup is blocked. Follow the blocker recovery guidance; another wait alone will not fix the failure.".into());
    }
    let ready_components = status
        .components
        .iter()
        .filter(|component| component.ready)
        .count();
    status.inventory.sort_by_key(inventory_key);
    CellStatusSummary {
        instance_key: status.instance.instance_key.clone(),
        observed_generation: status.observed_generation,
        observed_revision_digest: status.observed_revision_digest,
        last_converged_revision: status.last_converged_revision,
        retained_storage: status.retained_storage,
        generation: status.instance.generation,
        blockers,
        instance_id: status.instance.id,
        revision_digest: status.instance.revision_digest,
        lock_digest: status.instance.lock_digest,
        phase: status.phase,
        instance_namespace: status.instance_namespace,
        ready_components: u32::try_from(ready_components).unwrap_or(u32::MAX),
        total_components: u32::try_from(status.components.len()).unwrap_or(u32::MAX),
        inventory_count: u32::try_from(status.inventory.len()).unwrap_or(u32::MAX),
        inventory_digest: digest_json(&status.inventory),
        runtime_guidance: runtime_guidance(status.phase).map(str::to_owned),
        teardown_receipt: status.teardown_receipt,
        message: status.message,
    }
}

fn compact_cell_wait(
    mut status: CellInstanceStatus,
    target_phase: InstancePhase,
    reached: bool,
    timed_out: bool,
) -> CellWaitResult {
    let blockers = startup_blockers(&status);
    if status.phase == InstancePhase::Pending && !blockers.is_empty() {
        status.message = Some("Component startup is blocked. Follow the blocker recovery guidance; another wait alone will not fix the failure.".into());
    }
    let ready_components = status
        .components
        .iter()
        .filter(|component| component.ready)
        .count();
    CellWaitResult {
        instance_key: status.instance.instance_key.clone(),
        observed_generation: status.observed_generation,
        observed_revision_digest: status.observed_revision_digest,
        last_converged_revision: status.last_converged_revision,
        retained_storage: status.retained_storage,
        generation: status.instance.generation,
        superseded: false,
        blockers,
        instance_id: status.instance.id,
        phase: status.phase,
        target_phase,
        reached,
        timed_out,
        ready_components: u32::try_from(ready_components).unwrap_or(u32::MAX),
        total_components: u32::try_from(status.components.len()).unwrap_or(u32::MAX),
        runtime_guidance: runtime_guidance(status.phase).map(str::to_owned),
        teardown_receipt: status.teardown_receipt,
        message: status.message,
    }
}

const fn runtime_guidance(phase: InstancePhase) -> Option<&'static str> {
    match phase {
        InstancePhase::Ready => Some(
            "Ready means infrastructure/protocol availability, not mature regtest blocks, Lightning liquidity, or payment settlement. Use catalog guidance and available native commands to provision and verify component state. Native commands and logs supply run/session attribution automatically; run_id is optional and session attribution is automatic. Wait for operation results and independently verify effects.",
        ),
        _ => None,
    }
}

fn operation_result(operation: CellOperation) -> Result<CallToolResult, ErrorData> {
    let mut result = compact_operation_wait(operation, false);
    if read_query::wire_size(&result)? > MAX_AGENT_RESPONSE_BYTES {
        result.artifact = None;
    }
    developer_result(result)
}

fn compact_operation_wait(operation: CellOperation, timed_out: bool) -> OperationResult {
    let native_result = operation.artifact.as_ref().and_then(|artifact| {
        let content = &artifact.content;
        content.get("exit_code")?;
        Some(serde_json::Value::Object(
            [
                "exit_code",
                "exit_signal",
                "exit_scope",
                "cleanup_verified",
                "projection_succeeded",
                "output_truncated",
                "streams_complete",
                "timed_out",
                "cancelled",
            ]
            .into_iter()
            .filter_map(|key| content.get(key).map(|value| (key.into(), value.clone())))
            .collect(),
        ))
    });
    OperationResult {
        operation_digest: digest_json(&serde_json::json!(operation)),
        run_id: operation.experiment_id.clone(),
        operation_id: operation.id,
        sequence: operation.sequence,
        kind: operation.kind,
        phase: operation.phase,
        terminal: operation_terminal(operation.phase),
        timed_out,
        artifact_digest: operation
            .artifact
            .as_ref()
            .map(|artifact| artifact.digest.clone()),
        native_result,
        artifact: operation.artifact,
    }
}

fn partition_operation_results(
    ids: &[String],
    results: Vec<Result<Json<CellOperation>, ErrorData>>,
) -> (Vec<CellOperation>, Vec<OperationWaitError>) {
    let mut operations = Vec::new();
    let mut errors = Vec::new();
    for (id, result) in ids.iter().zip(results) {
        match result {
            Ok(operation) => operations.push(operation.0),
            Err(error) => errors.push(OperationWaitError {
                operation_id: id.clone(),
                error: serde_json::json!(error),
            }),
        }
    }
    (operations, errors)
}

fn compact_operation_wait_many(
    operations: Vec<CellOperation>,
    errors: Vec<OperationWaitError>,
    timed_out: bool,
) -> Result<OperationWaitResult, ErrorData> {
    let all_terminal = errors.is_empty()
        && operations
            .iter()
            .all(|operation| operation_terminal(operation.phase));
    let mut result = OperationWaitResult {
        operations: operations
            .into_iter()
            .map(|operation| {
                let operation_timed_out = timed_out && !operation_terminal(operation.phase);
                compact_operation_wait(operation, operation_timed_out)
            })
            .collect(),
        all_terminal,
        errors,
        timed_out,
        artifact_bodies_omitted: false,
    };
    if read_query::wire_size(&serde_json::json!(result))? > MAX_AGENT_RESPONSE_BYTES {
        for operation in &mut result.operations {
            operation.artifact = None;
        }
        result.artifact_bodies_omitted = true;
    }
    if read_query::wire_size(&serde_json::json!(result))? > MAX_AGENT_RESPONSE_BYTES {
        return Err(coded_invalid_request(
            "operation_wait_response_too_large",
            "The compact receipts exceed the wire budget; split operation_ids into smaller independent batches",
        ));
    }
    Ok(result)
}

/// One coded invalid-request error.
///
/// The `code` travels in the error payload so agents can branch on a stable
/// identifier rather than parsing prose.
fn coded_invalid_request(code: &str, message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_request(message.into(), Some(serde_json::json!({"code": code})))
}

fn native_command_error(message: &str) -> ErrorData {
    if message.contains("field") {
        return ErrorData::invalid_params(
            format!(
                "{message}. The command was not executed. json_fields only supports the fixed receipt fields listed in output's schema. For addresses, node IDs, help, or other native JSON, use output: {{\"mode\":\"public\"}} with no fields. For LND addinvoice, use output: {{\"mode\":\"lnd_invoice\"}}."
            ),
            Some(serde_json::json!({
                "code": "native_output_invalid",
                "executed": false,
                "public_output_example": {"mode": "public"},
                "receipt_output_example": {"mode": "json_fields", "fields": ["confirmed_balance"]}
            })),
        );
    }
    invalid_operation(message)
}

fn invalid_operation(message: &str) -> ErrorData {
    coded_invalid_request("invalid_operation", message)
}

impl ProofstormMcp {
    async fn sync_candidate_build(
        &self,
        candidate: CandidateBuild,
    ) -> Result<CandidateBuild, ErrorData> {
        proofstorm_app::candidate::observe(
            &self.runtime()?.shared(),
            &self.store,
            &self.workspace,
            candidate,
        )
        .await
        .map_err(app_error)
    }
}

fn candidate_build_resource(
    candidate: &CandidateBuild,
    namespace: &str,
    registry: &str,
) -> Result<ProofstormCandidateBuild, ErrorData> {
    let repository = candidate.repository.clone().ok_or_else(|| {
        coded_invalid_request(
            "candidate_source_missing",
            "candidate repository is missing",
        )
    })?;
    let commit_sha = candidate.commit_sha.clone().ok_or_else(|| {
        coded_invalid_request(
            "candidate_source_missing",
            "candidate commit SHA is missing",
        )
    })?;
    let version = candidate.version.clone().ok_or_else(|| {
        coded_invalid_request("candidate_version_missing", "candidate version is missing")
    })?;
    let mut resource = ProofstormCandidateBuild::new(
        &candidate.resource_name,
        ProofstormCandidateBuildSpec {
            workspace_id: candidate.workspace_id.clone(),
            candidate_id: candidate.id.clone(),
            principal_id: candidate.principal_id.clone(),
            implementation: candidate.implementation.clone(),
            base_version: candidate.base_version.clone(),
            pull_request_url: candidate.pull_request_url.clone(),
            repository,
            commit_sha,
            version,
            request_digest: candidate.request_digest.clone(),
            accepted_at_unix: candidate.accepted_at_unix,
            provenance: candidate.provenance.clone(),
            image_repository: format!(
                "{registry}/{}/{}",
                if candidate.provenance.is_some() {
                    "candidates"
                } else {
                    "proofstorm-candidates"
                },
                candidate.implementation
            ),
            dockerfile: candidate
                .provenance
                .as_ref()
                .map_or_else(
                    || match candidate.implementation.as_str() {
                        "cdk" | "cdk-bdk" | "nutshell" | "nutshell-wallet" => {
                            Some("Dockerfile".into())
                        }
                        "cdk-ldk" => Some("Dockerfile.ldk-node".into()),
                        _ => None,
                    },
                    |p| Some(p.profile.dockerfile.clone()),
                )
                .ok_or_else(|| {
                    coded_invalid_request(
                        "candidate_implementation_unsupported",
                        "candidate implementation has no build adapter",
                    )
                })?,
        },
    );
    resource.metadata.namespace = Some(namespace.into());
    Ok(resource)
}

use proofstorm_app::candidate::receipt as compact_candidate_build;

#[cfg(test)]
fn candidate_build_adapter(implementation: &str) -> Option<proofstorm_core::CandidateBuildProfile> {
    proofstorm_core::candidate_build_profile(implementation)
}

impl KubernetesRuntime {
    fn shared(&self) -> proofstorm_app::Runtime {
        proofstorm_app::Runtime {
            client: self.client.clone(),
            control_namespace: self.control_namespace.clone(),
            cluster_source: self.cluster_source.clone(),
        }
    }

    async fn private_access(
        &self,
        grant: &proofstorm_core::PrivateAccessGrant,
    ) -> Result<(), ErrorData> {
        self.shared().private_access(grant).await.map_err(app_error)
    }

    async fn apply_candidate_build(&self, candidate: &CandidateBuild) -> Result<(), ErrorData> {
        let builds = Api::<ProofstormCandidateBuild>::namespaced(
            self.client.clone(),
            &self.control_namespace,
        );
        let resource =
            candidate_build_resource(candidate, &self.control_namespace, &self.candidate_registry)?;
        if let Some(existing) = builds
            .get_opt(&candidate.resource_name)
            .await
            .map_err(kube_error)?
        {
            if existing.spec != resource.spec {
                return Err(coded_invalid_request(
                    "candidate_identity_conflict",
                    "candidate build resource exists with a different immutable request",
                ));
            }
            return Ok(());
        }
        builds
            .patch(
                &candidate.resource_name,
                &PatchParams::apply("proofstorm-mcp").force(),
                &Patch::Apply(&resource),
            )
            .await
            .map_err(kube_error)?;
        Ok(())
    }

    async fn request_candidate_cancellation(
        &self,
        candidate: &CandidateBuild,
        token: &str,
    ) -> Result<(), ErrorData> {
        let builds = Api::<ProofstormCandidateBuild>::namespaced(
            self.client.clone(),
            &self.control_namespace,
        );
        builds
            .patch(
                &candidate.resource_name,
                &PatchParams::default(),
                &Patch::Merge(serde_json::json!({
                    "metadata": {"annotations": {(CANDIDATE_CANCEL_ANNOTATION): token}}
                })),
            )
            .await
            .map_err(kube_error)?;
        Ok(())
    }

    async fn apply_action(
        &self,
        instance: &CellInstance,
        action: &ProofstormCellAction,
    ) -> Result<(), ErrorData> {
        self.shared()
            .apply_action(instance, action)
            .await
            .map_err(app_error)
    }

    async fn action_status(
        &self,
        operation: &CellOperation,
    ) -> Result<Option<(OperationPhase, serde_json::Value)>, ErrorData> {
        self.shared()
            .action_status(operation)
            .await
            .map_err(app_error)
    }

    /// Request cancellation of a runtime action. Returns `false` when the
    /// runtime resource no longer exists, so the caller finalizes the journal
    /// entry itself instead of leaving it non-terminal forever.
    async fn request_action_cancellation(
        &self,
        operation: &CellOperation,
        token: &str,
    ) -> Result<bool, ErrorData> {
        self.shared()
            .request_action_cancellation(operation, token)
            .await
            .map_err(app_error)
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

fn app_error(error: proofstorm_app::Error) -> ErrorData {
    match error.kind {
        proofstorm_app::ErrorKind::Invalid => {
            ErrorData::invalid_request(error.message, error.details)
        }
        proofstorm_app::ErrorKind::Missing => {
            ErrorData::resource_not_found(error.message, error.details)
        }
        proofstorm_app::ErrorKind::Failure => {
            ErrorData::internal_error(error.message, error.details)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn private_transfer_methods_preserve_the_controller_wire_contract() {
        for input in [
            serde_json::json!({"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":65536}),
            serde_json::json!({"transferMethod":"handoff","component":"wallet-a","reference":"opaque-ref","recipientGrantId":"child"}),
            serde_json::json!({"transferMethod":"status","component":"wallet-a","reference":"opaque-ref"}),
            serde_json::json!({"transferMethod":"deliver","component":"wallet-a","reference":"opaque-ref"}),
            serde_json::json!({"transferMethod":"release","component":"wallet-a","reference":"opaque-ref"}),
        ] {
            let old: proofstorm_kube::PrivateTransferAction =
                serde_json::from_value(input.clone()).unwrap();
            let public: PrivateTransferInput = serde_json::from_value(input.clone()).unwrap();
            assert_eq!(public.action().unwrap(), old);
            assert_eq!(serde_json::to_value(public).unwrap(), input);
        }
    }

    #[test]
    fn private_transfer_preflight_enforces_native_input_limits_and_endpoints() {
        let prepare = |size, destination: &str| PrivateTransferInput::Prepare {
            component: "wallet-a".into(),
            destination_component: destination.into(),
            maximum_bytes: size,
        };
        for size in [0, 1_048_577, u32::MAX] {
            assert!(
                prepare(size, "wallet-b")
                    .action()
                    .unwrap_err()
                    .message
                    .contains("maximumBytes")
            );
        }
        assert!(
            prepare(65536, "wallet-a")
                .action()
                .unwrap_err()
                .message
                .contains("must differ")
        );
        assert!(prepare(65536, " ").action().is_err());
        let recipient = |implementation: &str| {
            let mut revision = component_reference_revision();
            let entry = default_catalog()
                .entries
                .iter()
                .find(|e| e.id == implementation)
                .unwrap();
            let component = revision
                .cell
                .components
                .iter_mut()
                .find(|c| c.id == "wallet-b")
                .unwrap();
            component.implementation = entry.id.clone();
            component.version = Some(entry.version.clone());
            component.config_version = entry.config_version.clone();
            component.control = entry.allowed_control[0];
            revision.lock =
                proofstorm_core::resolve_lock(&revision.cell, default_catalog()).unwrap();
            revision
        };
        let revision = recipient("cdk-cli-wallet");
        for size in [1, 65536] {
            validate_private_transfer_endpoints(
                &prepare(size, "wallet-b").action().unwrap(),
                &revision,
            )
            .unwrap();
        }
        for size in [65537, 1_048_576] {
            let error = validate_private_transfer_endpoints(
                &prepare(size, "wallet-b").action().unwrap(),
                &revision,
            )
            .unwrap_err();
            assert!(
                error.message.contains("65536")
                    && error.message.contains("no operation was created")
            );
        }
        for destination in ["missing", "chain"] {
            assert!(
                validate_private_transfer_endpoints(
                    &prepare(1, destination).action().unwrap(),
                    &revision
                )
                .is_err()
            );
        }
        let revision = recipient("cocod-wallet");
        validate_private_transfer_endpoints(
            &prepare(1_048_576, "wallet-b").action().unwrap(),
            &revision,
        )
        .unwrap();
        assert!(
            PrivateTransferInput::Status {
                component: "wallet-a".into(),
                reference: String::new()
            }
            .action()
            .is_err()
        );
    }

    #[test]
    fn escaped_native_output_cannot_overflow_the_journal_receipt() {
        for text in [
            "\0".repeat(16384),
            "界\n\\\"".repeat(8192),
            "x".repeat(16384),
        ] {
            let receipt = serde_json::json!({"stdout":text,"stderr":text,
                "cleanup_verified":true,"exit_code":7,"output_truncated":false});
            let status = proofstorm_kube::ProofstormCellActionStatus {
                phase: ActionPhase::Succeeded,
                artifact: Some(serde_json::from_value(receipt).unwrap()),
                ..Default::default()
            };
            let (_, artifact) = terminal_action_observation(status, true).unwrap();
            assert!(serde_json::to_vec(&artifact).unwrap().len() < 28 * 1024);
            assert_eq!(artifact["cleanup_verified"], true);
            assert_eq!(artifact["exit_code"], 7);
            assert_eq!(artifact["output_truncated"], true);
            assert!(!artifact["stdout"].as_str().unwrap().is_empty());
        }
    }
    #[test]
    fn terminal_native_receipts_survive_cancellation_and_failure() {
        for (phase, expected) in [
            (ActionPhase::Cancelled, OperationPhase::Cancelled),
            (ActionPhase::Failed, OperationPhase::Failed),
        ] {
            let receipt = serde_json::json!({"cleanup_verified": phase == ActionPhase::Cancelled,
                "cancelled":true,"exit_signal":15,"stdout":"","stderr":""});
            let status = proofstorm_kube::ProofstormCellActionStatus {
                phase,
                artifact: Some(serde_json::from_value(receipt.clone()).unwrap()),
                error: Some(
                    serde_json::from_value(serde_json::json!({"code":"generic_error"})).unwrap(),
                ),
                ..Default::default()
            };
            assert_eq!(
                terminal_action_observation(status.clone(), true),
                Some((expected, receipt))
            );
            let legacy = if phase == ActionPhase::Failed {
                serde_json::json!({"code":"generic_error"})
            } else {
                serde_json::json!({"code":"action_cancelled"})
            };
            assert_eq!(
                terminal_action_observation(status, false),
                Some((expected, legacy))
            );
        }
        let status = proofstorm_kube::ProofstormCellActionStatus {
            phase: ActionPhase::Cancelled,
            ..Default::default()
        };
        assert_eq!(
            terminal_action_observation(status, true),
            Some((
                OperationPhase::Cancelled,
                serde_json::json!({"code":"action_cancelled"})
            ))
        );
    }
    use super::*;
    use proofstorm_core::{API_VERSION, CellPolicy, NetworkFaultFeature};

    fn cell(name: &str) -> CellSpec {
        CellSpec {
            api_version: API_VERSION.into(),
            name: name.into(),
            components: vec![],
            links: vec![],
            policy: CellPolicy::default(),
        }
    }

    fn component_reference_revision() -> PublishedRevision {
        let catalog = default_catalog();
        let component = |id: &str, implementation: &str| {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == implementation)
                .expect("fixture implementation");
            ComponentSpec {
                id: id.into(),
                kind: entry.kind,
                implementation: entry.id.clone(),
                version: Some(entry.version.clone()),
                config_version: entry.config_version.clone(),
                control: entry.allowed_control[0],
                config: BTreeMap::new(),
            }
        };
        let authored = CellSpec {
            api_version: API_VERSION.into(),
            name: "component-references".into(),
            components: vec![
                component("wallet-b", "nutshell-wallet"),
                component("chain", "bitcoin-core"),
                component("wallet-a", "nutshell-wallet"),
            ],
            links: vec![],
            policy: CellPolicy::default(),
        };
        let cell = proofstorm_core::resolve_effective_cell(&authored, catalog)
            .expect("effective component fixture");
        let lock = proofstorm_core::resolve_lock(&cell, catalog).expect("locked component fixture");
        PublishedRevision {
            workspace_id: "alpha".into(),
            digest: digest_json(&(&cell, &lock)),
            cell,
            lock,
        }
    }

    #[test]
    fn component_references_return_typed_recovery_alternatives() {
        let revision = component_reference_revision();
        let expected_image = default_catalog()
            .entries
            .iter()
            .find(|entry| entry.id == "nutshell-wallet")
            .expect("wallet catalog entry")
            .image
            .clone();
        assert_eq!(
            component_image_any(&revision, "wallet-a", ComponentKind::Wallet)
                .expect("known typed component"),
            expected_image
        );

        let unknown = component_image_any(&revision, "invented-wallet", ComponentKind::Wallet)
            .expect_err("unknown component must fail closed");
        let unknown_data = unknown.data.expect("structured unknown-ID error");
        assert_eq!(unknown_data["code"], "component_id_unknown");
        assert_eq!(unknown_data["requested_id"], "invented-wallet");
        assert_eq!(unknown_data["expected_kind"], "wallet");
        assert_eq!(
            unknown_data["valid_component_ids"],
            serde_json::json!(["wallet-a", "wallet-b"])
        );

        let wrong_kind = component_image_any(&revision, "chain", ComponentKind::Wallet)
            .expect_err("wrong component kind must fail closed");
        let wrong_kind_data = wrong_kind.data.expect("structured kind error");
        assert_eq!(wrong_kind_data["code"], "component_kind_mismatch");
        assert_eq!(wrong_kind_data["actual_kind"], "bitcoin");
        assert_eq!(wrong_kind_data["expected_kind"], "wallet");
        assert_eq!(
            wrong_kind_data["valid_component_ids"],
            serde_json::json!(["wallet-a", "wallet-b"])
        );
    }

    #[test]
    fn authored_cell_policy_is_optional_and_defaults_safely() {
        let authored = serde_json::from_value::<AuthoredCellSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "default-policy",
            "components": [],
            "links": []
        }))
        .expect("policy may be omitted");
        assert_eq!(authored.policy, CellPolicy::default());
    }

    #[test]
    fn evidence_pointer_error_is_self_correcting_without_structured_error_data() {
        let error = evidence_pointer(
            serde_json::json!({"cell": {}, "components": []}),
            "/missing",
            "lock",
        )
        .expect_err("unknown pointer must fail");
        let message = error.message.to_string();
        assert!(message.contains("[evidence_pointer_not_found]"));
        assert!(message.contains("no changes were made"));
        assert!(message.contains("Recovery:"));
        assert!(message.contains("/components"));
        assert!(message.contains("/cell"));
        let error = evidence_pointer(
            serde_json::json!({"artifact": {"content": {"exit_code": 0}, "digest": "abc"}}),
            "/artifact/exit_code",
            "artifact",
        )
        .unwrap_err();
        assert!(error.message.contains("/artifact/content"));
        assert!(error.message.contains("/artifact/digest"));
    }

    #[test]
    fn runtime_admission_rejects_a_catalog_unsupported_control_before_operation() {
        let catalog = default_catalog();
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "cdk-ldk")
            .expect("CDK-LDK catalog entry");
        let authored = CellSpec {
            api_version: API_VERSION.into(),
            name: "runtime-admission".into(),
            components: vec![ComponentSpec {
                id: "mint".into(),
                kind: entry.kind,
                implementation: entry.id.clone(),
                version: Some(entry.version.clone()),
                config_version: entry.config_version.clone(),
                control: ControlClass::Target,
                config: BTreeMap::new(),
            }],
            links: vec![],
            policy: CellPolicy::default(),
        };
        let cell = proofstorm_core::resolve_effective_cell(&authored, catalog)
            .expect("effective runtime-admission fixture");
        let lock = proofstorm_core::resolve_lock(&cell, catalog).expect("fixture lock");
        let revision = PublishedRevision {
            workspace_id: "alpha".into(),
            digest: digest_json(&(&cell, &lock)),
            cell,
            lock,
        };

        require_component_runtime_control(
            &revision,
            "mint",
            "component",
            "wallet_initialize",
            default_catalog(),
        )
        .expect("declared runtime control");
        let error = require_component_runtime_control(
            &revision,
            "mint",
            "component",
            "wallet_fund",
            default_catalog(),
        )
        .expect_err("unsupported runtime control must fail before operation creation");
        assert!(error.message.contains("no operation was created"));
        assert_eq!(
            error.data.expect("structured runtime admission error")["code"],
            "runtime_control_unsupported"
        );
    }

    pub(super) fn seeded_store() -> Store {
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
            Capability::CellRead,
            Capability::CellCreate,
            Capability::CellEdit,
            Capability::CellClone,
            Capability::CellValidate,
            Capability::CellPublish,
            Capability::CellMaterialize,
            Capability::CellStatus,
            Capability::CellClose,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("designer grant");
        }
        store
            .grant("alpha", "reader", Capability::CellRead)
            .expect("reader grant");
        store
    }

    #[tokio::test]
    async fn environment_tool_uses_shared_read_model_and_rechecks_permissions() {
        let store = seeded_store();
        store
            .grant("alpha", "designer", Capability::ExperimentRead)
            .unwrap();
        let _cell = store
            .reserve_cell("alpha", "designer", "pending", "digest")
            .unwrap();
        let client = kube::Client::new(
            tower::service_fn(|_: http::Request<kube::client::Body>| {
                std::future::ready(Ok::<_, std::io::Error>(http::Response::new(kube::client::Body::from(
                    serde_json::json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormCellList","metadata":{},"items":[]}).to_string().into_bytes()
                ))))
            }),
            "system",
        );
        let service = ProofstormMcp::new(store.clone(), "alpha", "designer")
            .unwrap()
            .with_kubernetes(client, "system");
        assert!(
            service
                .tool_names()
                .contains(&"environment_read".to_owned())
        );
        let result = service
            .proofstorm_environment_read(Parameters(EnvironmentRequest::default()))
            .await
            .unwrap();
        assert_eq!(
            result.structured_content.as_ref().unwrap()["cells"]["items"],
            serde_json::json!([])
        );
        assert_environment_text_matches_structured(&result);
        assert!(serde_json::to_vec(&result).unwrap().len() < MAX_AGENT_RESPONSE_BYTES);
        store
            .replace_grants("alpha", "designer", [Capability::CellRead])
            .unwrap();
        assert!(
            service
                .proofstorm_environment_read(Parameters(EnvironmentRequest::default()))
                .await
                .is_err()
        );
    }

    fn assert_environment_text_matches_structured(result: &CallToolResult) -> serde_json::Value {
        // OpenCode 1.18.30 does not synthesize text from structuredContent when
        // content already exists. Check exactly what a text-only consumer sees.
        let wire = serde_json::to_value(result).unwrap();
        let text = wire["content"][0]["text"].as_str().unwrap();
        let visible: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(Some(&visible), result.structured_content.as_ref());
        assert!(serde_json::to_vec(result).unwrap().len() <= MAX_AGENT_RESPONSE_BYTES);
        visible
    }

    fn environment_response_fixture() -> proofstorm_app::environment::EnvironmentView {
        serde_json::from_value(serde_json::json!({
            "api_version":"proofstorm/environment/v1alpha1", "workspace_id":"alpha",
            "scope":"test workspace", "observation_started_at_unix":1,"observation_finished_at_unix":2,
            "coverage":{"topology":"declared", "activity":"cached", "resource_demand":"desired",
                "resource_usage":"not collected", "protocol_traffic":"not collected", "attached_clients":"not tracked"},
            "cells":{"next_cursor":null,"items":[{
                "id":"vm-alpha-smoke", "handle":null, "revision_digest":null,
                "journal_read_at_unix":1,"last_recorded_activity_at_unix":null,
                "runtime":{"state":"available","fetched_at_unix":1,"source_updated_at_unix":null,
                    "resource_version":null,"generation":1,"observed_generation":1,"phase":"ready","error":null},
                "components":{"items":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core",
                    "version":"31.1","ready":true,"conditions":[],"endpoints":[]}],"next_cursor":null},
                "links":{"items":[],"next_cursor":null},"resources":null,"resource_error":null,
                "sessions":{"items":[],"next_cursor":null},"activity":{"items":[],"next_cursor":null}
            }]}
        })).unwrap()
    }

    #[test]
    fn environment_text_only_clients_receive_ready_cell_facts() {
        let result = environment_result(environment_response_fixture()).unwrap();
        let visible = assert_environment_text_matches_structured(&result);
        assert_eq!(visible["cells"]["items"][0]["id"], "vm-alpha-smoke");
        assert_eq!(visible["cells"]["items"][0]["runtime"]["phase"], "ready");
        assert_eq!(
            visible["cells"]["items"][0]["components"]["items"][0]["version"],
            "31.1"
        );
        assert_eq!(
            visible["cells"]["items"][0]["components"]["items"][0]["ready"],
            true
        );
    }

    #[test]
    fn environment_dual_content_pages_keep_cursors_and_fit_with_escaped_text() {
        let mut view = environment_response_fixture();
        let mut cell = view.cells.items[0].clone();
        cell.runtime.message = Some("\"\\\n".repeat(250));
        view.cells.items = (0..24)
            .map(|n| {
                let mut item = cell.clone();
                item.id = format!("cell-{n:02}");
                item
            })
            .collect();
        let result = environment_result(view).unwrap();
        let visible = assert_environment_text_matches_structured(&result);
        let items = visible["cells"]["items"].as_array().unwrap();
        assert!(!items.is_empty() && items.len() < 24);
        assert_eq!(visible["cells"]["next_cursor"], items.last().unwrap()["id"]);
        assert_eq!(
            items[0]["runtime"]["message"],
            cell.runtime.message.unwrap()
        );
    }

    #[test]
    fn environment_dual_content_keeps_component_continuation_and_rejects_oversized_items() {
        let mut view = environment_response_fixture();
        let component = view.cells.items[0].components.items[0].clone();
        view.cells.items[0].components.items = (0..64)
            .map(|n| {
                let mut item = component.clone();
                item.id = format!("chain-{n:02}");
                item
            })
            .collect();
        let result = environment_result(view).unwrap();
        let visible = assert_environment_text_matches_structured(&result);
        let components = &visible["cells"]["items"][0]["components"];
        let items = components["items"].as_array().unwrap();
        assert!(!items.is_empty() && items.len() < 64);
        assert_eq!(components["next_cursor"], items.last().unwrap()["id"]);
        let mut oversized = environment_response_fixture();
        oversized.cells.items[0].runtime.message = Some("x".repeat(MAX_AGENT_RESPONSE_BYTES));
        assert!(environment_result(oversized).is_err());
    }

    #[test]
    fn discovery_is_filtered_for_two_principals() {
        let store = seeded_store();
        let designer =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("designer session");
        let reader = ProofstormMcp::new(store, "alpha", "reader").expect("reader session");
        assert_eq!(designer.tool_names().len(), 10);
        assert!(!designer.tool_names().contains(&"cell_edit".to_owned()));
        assert!(designer.tool_names().contains(&"cell_wait".to_owned()));
        let backend = designer
            .proofstorm_network_capabilities()
            .expect("network backend discovery")
            .0;
        assert_eq!(backend.id, "kubernetes-network-policy");
        assert!(backend.supports(NetworkFaultFeature::Partition));
        assert!(!backend.supports(NetworkFaultFeature::Delay));
        let catalog = default_catalog();
        assert_eq!(catalog.entries.len(), 21);
        assert!(catalog.entries.iter().all(|entry| {
            entry.config_version.contains('/')
                && entry.config_schema_digest.starts_with("sha256:")
                && entry.image.contains("@sha256:")
        }));
        assert_eq!(catalog.implementations.len(), 14);
        assert_support_defaults(catalog);
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
        assert_eq!(reader.tool_names(), vec!["cell_read", "cell_search"]);
    }

    fn assert_support_defaults(catalog: &proofstorm_core::CatalogResponse) {
        assert!(catalog.implementations.iter().all(|support| {
            support.implementation == "cocod-wallet"
                || support.preferred_version.as_ref().is_some_and(|version| {
                    support.supported_versions.contains(version)
                        && (matches!(
                            support.implementation.as_str(),
                            "lnd" | "nutshell" | "nutshell-wallet"
                        ) || support.minimum_supported == support.preferred_version
                            && support.supported_versions.len() == 1)
                })
        }));
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
        let embedded = cdk_ldk
            .runtime_endpoints
            .iter()
            .find(|endpoint| endpoint.id == "ldk-node")
            .expect("embedded LDK runtime endpoint is discoverable");
        assert_eq!(embedded.kind, "lightning");
        assert!(embedded.controls.is_empty());
        assert!(!embedded.limitations.is_empty());
    }

    #[test]
    fn catalog_summary_then_exact_detail_and_schema_is_progressive() {
        let service = ProofstormMcp::new(seeded_store(), "alpha", "designer").expect("service");
        let mut page = service
            .proofstorm_catalog_list(Parameters(CatalogListRequest::default()))
            .expect("catalog discovery")
            .0;
        let mut items = page.items.clone();
        while let Some(cursor) = page.next_cursor.clone() {
            page = service
                .proofstorm_catalog_list(Parameters(CatalogListRequest {
                    cursor: Some(cursor),
                    ..Default::default()
                }))
                .unwrap()
                .0;
            assert!(serde_json::to_vec(&page).unwrap().len() <= 8 * 1024);
            assert!(
                read_query::wire_size(&serde_json::to_value(&page).unwrap()).unwrap()
                    <= MAX_AGENT_RESPONSE_BYTES
            );
            items.extend(page.items.clone());
        }
        page.items = items;
        let page: CatalogListResponse =
            serde_json::from_value(serde_json::to_value(page).unwrap()).unwrap();
        assert_eq!(page.items.len(), 17);
        assert!(page.next_cursor.is_none());
        assert!(read_query::wire_size(&page).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
        assert!(page.items.iter().all(|entry| {
            entry.config_version.contains('/')
                && entry.config_schema_digest.starts_with("sha256:")
                && (entry.support_lifecycle == SupportLifecycle::Preferred
                    || entry.id == "cocod-wallet"
                        && entry.support_lifecycle == SupportLifecycle::Experimental
                    || entry.id == "lnd"
                        && entry.version == "0.20.4-beta"
                        && entry.support_lifecycle == SupportLifecycle::Supported
                    || matches!(entry.id.as_str(), "nutshell" | "nutshell-wallet")
                        && entry.version == "0.20.3"
                        && entry.support_lifecycle == SupportLifecycle::Supported)
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
        assert_eq!(detail.recommended_control, ControlClass::Target);
        assert!(detail.required_config_fields.is_empty());
        assert!(
            detail
                .authorable_config_fields
                .contains(&"lightning_fee_percent".into())
        );
        assert_eq!(detail.config_defaults["lightning_reserve_fee_min_sat"], 2);
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
    fn missing_catalog_version_reports_exact_installed_alternatives() {
        let catalog = default_catalog();
        let error = exact_catalog_entry(&catalog.entries, "lnd", "0.21.3-beta4")
            .expect_err("near-match must not silently select another version");
        let message = error.message.to_string();
        assert!(message.contains("[catalog_entry_not_found]"));
        assert!(message.contains("no changes were made"));
        assert!(message.contains("Recovery:"));
        assert!(message.contains("0.20.4-beta"));
        assert!(message.contains("0.21.3-beta"));
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
        let first: CatalogListResponse =
            serde_json::from_value(serde_json::to_value(first).unwrap()).unwrap();
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
        let second: CatalogListResponse =
            serde_json::from_value(serde_json::to_value(second).unwrap()).unwrap();
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
        let filtered: CatalogListResponse =
            serde_json::from_value(serde_json::to_value(filtered).unwrap()).unwrap();
        assert_eq!(filtered.items.len(), 2);
        assert_eq!(filtered.items[0].id, "nutshell");
        assert_eq!(filtered.items[0].allowed_control, [ControlClass::Target]);
        assert_eq!(filtered.items[0].recommended_control, ControlClass::Target);
        assert!(filtered.next_cursor.is_none());

        let oversized_limit = service
            .proofstorm_catalog_list(Parameters(CatalogListRequest {
                limit: 100,
                ..CatalogListRequest::default()
            }))
            .expect("harmless oversized page limit is saturated")
            .0;
        let oversized_limit: CatalogListResponse =
            serde_json::from_value(serde_json::to_value(oversized_limit).unwrap()).unwrap();
        assert!(!oversized_limit.items.is_empty() && oversized_limit.items.len() < 15);
        assert!(oversized_limit.next_cursor.is_some());

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

    #[test]
    fn native_operation_schemas_do_not_require_bookkeeping() {
        let schemas = [
            schemars::schema_for!(ComponentLogsRequest),
            schemars::schema_for!(CellExecRequest),
            schemars::schema_for!(ComponentExecRequest),
            schemars::schema_for!(ComponentControlRequest),
            schemars::schema_for!(PrivateTransferRequest),
            schemars::schema_for!(NetworkPartitionRequest),
            schemars::schema_for!(NetworkHealRequest),
            schemars::schema_for!(NetworkProbeRequest),
            schemars::schema_for!(WalletBalanceRequest),
        ];
        for schema in schemas {
            let value = serde_json::to_value(schema).unwrap();
            let required = value["required"].as_array().unwrap();
            assert!(
                !required.contains(&serde_json::json!("experiment_id")),
                "{value}"
            );
            assert!(
                !required.contains(&serde_json::json!("session_id")),
                "{value}"
            );
            assert!(required.contains(&serde_json::json!("request_id")));
            assert!(
                !value["properties"]
                    .as_object()
                    .unwrap()
                    .contains_key("idempotency_key")
            );
            assert!(
                !value["properties"]
                    .as_object()
                    .unwrap()
                    .contains_key("session_id")
            );
        }
    }

    #[tokio::test]
    async fn invalid_native_projection_explains_recovery_before_cell_lookup() {
        let store = seeded_store();
        store
            .grant("alpha", "designer", Capability::ComponentExecLive)
            .unwrap();
        let service = ProofstormMcp::new(store, "alpha", "designer").unwrap();
        let error = service
            .proofstorm_cell_exec(Parameters(CellExecRequest {
                script: String::new(),
                private_payload: None,
                run_id: String::new(),
                name: "no-cell-was-created".into(),
                component: "node".into(),
                request_id: "read-address".into(),
                argv: vec!["lncli".into(), "newaddress".into(), "p2wkh".into()],
                timeout_seconds: 30,
                output: proofstorm_core::native::NativeOutput {
                    mode: proofstorm_core::native::OutputMode::JsonFields,
                    fields: vec!["address".into()],
                },
            }))
            .await
            .expect_err("invalid projection must be rejected before execution");
        assert!(error.message.contains("command was not executed"));
        assert!(error.message.contains("public"));
        let data = error.data.unwrap();
        assert_eq!(data["executed"], false);
        assert_eq!(
            data["public_output_example"],
            serde_json::json!({"mode": "public"})
        );
    }

    #[tokio::test]
    async fn cancelling_before_submission_is_cancelled_but_missing_running_action_is_failed() {
        let store = seeded_store();
        for capability in [
            Capability::ComponentExecLive,
            Capability::CellOperate,
            Capability::ExperimentRead,
            Capability::ActionCancel,
            Capability::ArtifactRead,
        ] {
            store.grant("alpha", "designer", capability).unwrap();
        }
        let spec = serde_json::from_value(serde_json::json!({
            "api_version":"proofstorm/v1alpha1","name":"cancel","links":[],
            "components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]
        })).unwrap();
        store
            .create_draft("alpha", "designer", "cancel", &spec, "draft")
            .unwrap();
        let revision = store
            .publish("alpha", "designer", "cancel", 1, "publish")
            .unwrap();
        store
            .materialize("alpha", "designer", "cancel", &revision.digest, "apply")
            .unwrap();
        let client = kube::Client::new(
            tower::service_fn(|_: http::Request<kube::client::Body>| async {
                Ok::<_, std::io::Error>(http::Response::builder().status(404)
                .body(kube::client::Body::from(r#"{"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404}"#.as_bytes().to_vec())).unwrap())
            }),
            "system",
        );
        let service = ProofstormMcp::new(store.clone(), "alpha", "designer")
            .unwrap()
            .with_kubernetes(client, "system");
        for (id, phase, expected, code) in [
            (
                "pending",
                OperationPhase::Pending,
                OperationPhase::Cancelled,
                "action_cancelled",
            ),
            (
                "running",
                OperationPhase::Running,
                OperationPhase::Failed,
                "action_runtime_not_found",
            ),
        ] {
            store
                .create_operation(
                    "alpha",
                    "designer",
                    "cancel",
                    "",
                    "",
                    id,
                    OperationKind::ComponentExecLive,
                    &serde_json::json!({"component":"chain","argv":["true"]}),
                    id,
                    Capability::ComponentExecLive,
                )
                .unwrap();
            if phase == OperationPhase::Running {
                store.update_operation_phase("alpha", id, phase).unwrap();
            }
            for _ in 0..2 {
                let reply = service.request_cancellation(Parameters(serde_json::from_value(
                    serde_json::json!({"operation_id":id,"request_id":format!("cancel-{id}")})
                ).unwrap())).await.unwrap().0;
                assert_eq!(reply.phase, expected);
                assert_eq!(reply.artifact.unwrap().content["code"], code);
            }
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "raw requests and a mock cluster verify automatic admission, unready logs and replay end to end"
    )]
    async fn raw_native_requests_admit_without_experiment_setup_even_when_cell_is_unready() {
        let store = seeded_store();
        let spec = serde_json::from_value(serde_json::json!({
            "api_version":"proofstorm/v1alpha1", "name":"automatic", "links":[],
            "components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]
        })).unwrap();
        store
            .create_draft("alpha", "designer", "automatic", &spec, "draft")
            .unwrap();
        let revision = store
            .publish("alpha", "designer", "automatic", 1, "publish")
            .unwrap();
        let instance = store
            .materialize("alpha", "designer", "automatic", &revision.digest, "apply")
            .unwrap();
        for capability in [
            Capability::ComponentLogs,
            Capability::ComponentExecLive,
            Capability::CellOperate,
            Capability::ExperimentRead,
        ] {
            store.grant("alpha", "designer", capability).unwrap();
        }
        assert!(
            !store
                .capabilities("alpha", "designer")
                .unwrap()
                .contains(&Capability::ExperimentCreate)
        );
        let resource = proofstorm_kube::ProofstormCell::new(
            &instance.resource_name,
            proofstorm_kube::ProofstormCellSpec {
                workspace_id: "alpha".into(),
                instance_id: instance.id,
                instance_key: instance.instance_key,
                revision_digest: revision.digest,
                lock: revision.lock,
                cell: revision.cell,
            },
        ); // No Ready status: diagnosis must still work.
        let cell_json = serde_json::to_vec(&resource).unwrap();
        let submissions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = submissions.clone();
        let client = kube::Client::new(
            tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let cell_json = cell_json.clone();
                let count = count.clone();
                async move {
                    let response = if request.uri().path().contains("/proofstormcells/") {
                        http::Response::new(kube::client::Body::from(cell_json))
                    } else if request.method() == http::Method::GET {
                        http::Response::builder().status(404).body(kube::client::Body::from(r#"{"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"missing","code":404}"#.as_bytes().to_vec())).unwrap()
                    } else {
                        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let bytes = request.into_body().collect_bytes().await.unwrap();
                        http::Response::new(kube::client::Body::from(bytes))
                    };
                    Ok::<_, std::io::Error>(response)
                }
            }),
            "system",
        );
        let service = ProofstormMcp::new(store.clone(), "alpha", "designer")
            .unwrap()
            .with_kubernetes(client.clone(), "system");
        let logs = serde_json::json!({"name":"automatic","component":"chain","tail_lines":20,"request_id":"logs"});
        let first = service
            .proofstorm_component_logs(Parameters(serde_json::from_value(logs.clone()).unwrap()))
            .await
            .unwrap()
            .0;
        assert!(!first.experiment_id.is_empty());
        let exec = serde_json::json!({"name":"automatic","component":"chain","argv":["bitcoin-cli","-version"],"timeout_seconds":10,"request_id":"exec"});
        service
            .proofstorm_cell_exec(Parameters(serde_json::from_value(exec).unwrap()))
            .await
            .unwrap();
        store
            .grant("alpha", "designer", Capability::ArtifactRead)
            .unwrap();
        let second = store.operation("alpha", "designer", "exec").unwrap();
        assert_eq!(first.experiment_id, second.experiment_id);
        store
            .finish_session("alpha", "designer", &first.session_id, "finish")
            .unwrap();
        let reconnect = ProofstormMcp::new(store, "alpha", "designer")
            .unwrap()
            .with_kubernetes(client, "system");
        let replay = reconnect
            .proofstorm_component_logs(Parameters(serde_json::from_value(logs).unwrap()))
            .await
            .unwrap()
            .0;
        assert_eq!(first, replay);
        assert_eq!(submissions.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn component_logs_requires_its_capability_and_bounded_lines() {
        let store = seeded_store();
        let unauthorized = ProofstormMcp::new(store.clone(), "alpha", "designer")
            .expect("session without component.logs");
        let request = |tail_lines: u32| ComponentLogsRequest {
            instance_id: "instance-one".into(),
            experiment_id: "experiment-one".into(),
            session_id: "session-one".into(),
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

    #[test]
    fn topology_receipts_expose_stable_identities_and_binding_coverage() {
        let mut authored = serde_json::from_value::<CellSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "receipt-topology",
            "components": [
                {
                    "id": "lnd",
                    "kind": "lightning",
                    "implementation": "lnd",
                    "version": "0.20",
                    "config_version": "lnd/0.20/v1",
                    "control": "target",
                    "config": {"alias": "receipt-lnd"}
                },
                {
                    "id": "chain",
                    "kind": "bitcoin",
                    "implementation": "bitcoin-core",
                    "version": "31.1",
                    "config_version": "bitcoin-core/31/v1",
                    "control": "cell",
                    "config": {}
                }
            ],
            "links": [{
                "id": "lnd-chain",
                "kind": "chain_backend",
                "from": "lnd",
                "to": "chain",
                "binding": {"type": "chain", "network": "regtest"}
            }],
            "policy": {}
        }))
        .expect("typed cell");
        let first = cell_validation_result(&authored);
        assert_eq!(first.component_ids, ["chain", "lnd"]);
        assert_eq!(first.link_ids, ["lnd-chain"]);
        assert!(first.warnings.is_empty());

        let first_digest = topology_summary(&authored).topology_digest;
        authored.components.reverse();
        authored.links.reverse();
        let reordered = cell_validation_result(&authored);
        assert_eq!(first.component_ids, reordered.component_ids);
        assert_eq!(first_digest, topology_summary(&authored).topology_digest);
    }

    #[test]
    fn topology_receipt_warns_when_a_direct_backend_link_bypasses_a_router() {
        let authored = serde_json::from_value::<CellSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "routing-hazard",
            "components": [
                {"id":"mint-a","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"mint-b","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"backend-a","kind":"lightning","implementation":"lnd","version":"0.20","config_version":"lnd/0.20/v1","control":"cell","config":{}},
                {"id":"backend-b","kind":"lightning","implementation":"cln","version":"26.06","config_version":"cln/26.06/v1","control":"cell","config":{}},
                {"id":"router","kind":"lightning","implementation":"lnd","version":"0.20","config_version":"lnd/0.20/v1","control":"cell","config":{}}
            ],
            "links": [
                {"id":"pay-a","kind":"payment_backend","from":"mint-a","to":"backend-a","binding":{"type":"payment","method":"bolt11","unit":"sat"}},
                {"id":"pay-b","kind":"payment_backend","from":"mint-b","to":"backend-b","binding":{"type":"payment","method":"bolt11","unit":"sat"}},
                {"id":"direct","kind":"lightning_peer","from":"backend-a","to":"backend-b"}
            ],
            "policy": {}
        }))
        .expect("typed cell");
        let summary = topology_summary(&authored);
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.starts_with("direct_mint_backend_peer:"))
        );
    }

    #[test]
    fn topology_receipt_warns_when_cross_mint_work_has_only_one_wallet() {
        let authored = serde_json::from_value::<CellSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "wallet-hazard",
            "components": [
                {"id":"mint-a","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"mint-b","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"wallet","kind":"wallet","implementation":"nutshell-wallet","version":"0.20.3","config_version":"nutshell-wallet/0.20/v1","control":"cell","config":{}}
            ],
            "links": [],
            "policy": {}
        }))
        .expect("typed cell");
        let summary = topology_summary(&authored);
        assert!(summary.warnings.iter().any(|warning| {
            warning.starts_with("distinct_payment_wallets_required:")
                && warning.contains("bidirectional cross-mint payments")
        }));
    }

    fn assert_optional_tracking(service: &ProofstormMcp) {
        assert!(
            service
                .tool_names()
                .iter()
                .all(|name| !name.starts_with("proofstorm_lease_"))
        );
        for tool in service.tool_router.list_all() {
            assert_eq!(
                tool.title.as_deref(),
                proofstorm_view::tool_title(&tool.name)
            );
            assert!(
                tool.title.is_some(),
                "missing display name for {}",
                tool.name
            );
            let schema = serde_json::to_value(&tool.input_schema).unwrap();
            if schema["properties"].get("session_id").is_some() {
                assert!(
                    !schema["required"]
                        .as_array()
                        .is_some_and(|keys| keys.contains(&serde_json::json!("session_id"))),
                    "session attribution must be optional for {}",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn all_advertised_input_unions_are_portable_and_keep_candidate_constraints() {
        let store = seeded_store();
        proofstorm_app::developer::configure(&store, "alpha", "designer").unwrap();
        let service = ProofstormMcp::new(store, "alpha", "designer").unwrap();
        let tools = service.tool_router.list_all();
        for tool in &tools {
            let encoded = serde_json::to_string(&tool.input_schema).unwrap();
            assert!(
                !encoded.contains("\"oneOf\":"),
                "{} has a union that needs a portable representation",
                tool.name
            );
        }
        let tool = tools
            .into_iter()
            .find(|tool| tool.name == "candidate_build")
            .unwrap();
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        let source = &schema["properties"]["source"];
        assert_eq!(source["anyOf"][1], serde_json::json!({"type":"null"}));
        let reference = source["anyOf"][0]["$ref"].as_str().unwrap();
        let variants = schema
            .pointer(reference.strip_prefix('#').unwrap())
            .unwrap();
        let branches = variants["anyOf"].as_array().unwrap();
        assert_eq!(branches.len(), 3);
        for branch in branches {
            assert_eq!(branch["additionalProperties"], false);
            let expected = match branch["properties"]["type"]["const"].as_str().unwrap() {
                "pull_request" => serde_json::json!(["type", "url"]),
                "commit" => serde_json::json!(["type"]),
                "tag" => serde_json::json!(["type", "tag"]),
                other => panic!("unexpected source variant {other}"),
            };
            assert_eq!(branch["required"], expected);
        }
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("source"))
        );
    }

    #[test]
    fn fully_authorized_tool_discovery_has_a_regression_budget() {
        let store = seeded_store();
        proofstorm_app::developer::configure(&store, "alpha", "designer").unwrap();
        let service = ProofstormMcp::new(store, "alpha", "designer").unwrap();
        let expected = proofstorm_core::mcp::TOOLS
            .iter()
            .map(|tool| tool.name.to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            service.tool_names().into_iter().collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(expected.len(), 48);
        assert_optional_tracking(&service);
        let wire=serde_json::to_vec(&serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"tools":service.tool_router.list_all()}})).unwrap();
        eprintln!(
            "single toolset: {} tools, {} bytes",
            expected.len(),
            wire.len()
        );
        assert!(
            wire.len() <= 128 * 1024,
            "discovery envelope exceeds 128 KiB: {}",
            wire.len()
        );
        let offline = service
            .offline()
            .tool_names()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            offline,
            proofstorm_core::mcp::TOOLS
                .iter()
                .filter(|tool| !tool.requires_runtime)
                .map(|tool| tool.name.to_owned())
                .collect()
        );
    }

    #[test]
    fn candidate_build_pushes_to_the_selected_installation_registry() {
        let mut candidate: CandidateBuild = serde_json::from_value(serde_json::json!({
            "api_version": "proofstorm/candidate-build/v1alpha1",
            "id": "candidate-test", "workspace_id": "workspace", "principal_id": "developer",
            "implementation": "nutshell", "base_version": "0.19.0",
            "pull_request_url": "https://github.com/cashubtc/nutshell/pull/1095",
            "resource_name": "candidate-test", "request_digest": "digest",
            "phase": "pending", "accepted_at_unix": 1,
            "repository": "cashubtc/nutshell", "commit_sha": "abc123", "version": "test"
        }))
        .unwrap();
        let first = candidate_build_resource(
            &candidate,
            "proofstorm-system",
            "k3d-pst-first-registry:5000",
        )
        .unwrap();
        let second = candidate_build_resource(
            &candidate,
            "proofstorm-system",
            "k3d-pst-second-registry:5000",
        )
        .unwrap();
        assert_eq!(
            first.spec.image_repository,
            "k3d-pst-first-registry:5000/proofstorm-candidates/nutshell"
        );
        assert_eq!(
            second.spec.image_repository,
            "k3d-pst-second-registry:5000/proofstorm-candidates/nutshell"
        );
        assert_ne!(first.spec.image_repository, second.spec.image_repository);
        let mut profile = candidate_build_adapter("nutshell").unwrap();
        profile.dockerfile = "Saved.Dockerfile".into();
        candidate.provenance = Some(proofstorm_core::CandidateProvenance {
            schema_version: 1,
            input_digest: "input".into(),
            requested_source: proofstorm_core::CandidateInput::Tag {
                tag: "v0.20.3".into(),
            },
            platform: "linux/arm64".into(),
            source_image: proofstorm_kube::images::GIT_IMAGE.into(),
            builder_image: proofstorm_kube::images::BUILDKIT_IMAGE.into(),
            baseline_digest: "baseline".into(),
            profile_digest: profile.digest(),
            profile,
        });
        let new_build = candidate_build_resource(
            &candidate,
            "proofstorm-system",
            "k3d-pst-first-registry:5000",
        )
        .unwrap();
        assert_eq!(
            new_build.spec.image_repository,
            "k3d-pst-first-registry:5000/candidates/nutshell"
        );
        assert_eq!(new_build.spec.dockerfile, "Saved.Dockerfile");
        assert_eq!(new_build.spec.provenance, candidate.provenance);
    }

    #[test]
    fn candidate_build_surface_covers_all_cashu_profiles_and_bounded_waits() {
        for implementation in [
            "cdk",
            "cdk-bdk",
            "cdk-ldk",
            "nutshell",
            "nutshell-wallet",
            "cdk-cli-wallet",
            "cocod-wallet",
        ] {
            assert!(candidate_build_adapter(implementation).is_some());
        }
        let nutshell = candidate_build_adapter("nutshell").expect("Nutshell build adapter");
        assert_eq!(nutshell.repository, "cashubtc/nutshell");
        assert_eq!(nutshell.dockerfile, "Dockerfile");
        assert!(candidate_build_adapter("postgresql").is_none());
        assert!(validate_wait_timeout(120).is_ok());
        assert!(validate_wait_timeout(121).is_err());
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
    fn component_execution_modes_are_hidden_without_their_distinct_capabilities() {
        let store = seeded_store();
        let restricted =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("restricted session");
        assert!(!restricted.tool_names().contains(&"cell_exec".to_owned()));
        assert!(
            !restricted
                .tool_names()
                .contains(&"component_forensics".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::ComponentExecLive)
            .expect("live exec grant");
        let live = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("live session");
        assert!(live.tool_names().contains(&"cell_exec".to_owned()));
        assert!(
            !live
                .tool_names()
                .contains(&"component_forensics".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::ComponentForensics)
            .expect("forensics grant");
        let both = ProofstormMcp::new(store, "alpha", "designer").expect("execution session");
        assert!(
            both.tool_names()
                .contains(&"component_forensics".to_owned())
        );
    }

    #[test]
    fn batch_wait_preserves_native_failure_facts_and_successful_reads() {
        let operation = |index: u64| {
            let content = serde_json::json!({
                "exit_code": if index == 4 { 7 } else { 0 },
                "cleanup_verified": true, "output_truncated": true,
                "streams_complete": true, "stdout": format!("{{\"status\":\"FAILED\"}}{}", "x".repeat(14_000)),
                "private_output_ref": "private-reference",
            });
            CellOperation {
                revision_digest: String::new(),
                id: format!("diag-{index}"),
                workspace_id: "alpha".into(),
                instance_id: "instance".into(),
                experiment_id: "experiment".into(),
                session_id: "session".into(),
                principal_id: "designer".into(),
                sequence: index,
                kind: OperationKind::ComponentExecLive,
                capability: Capability::ComponentExecLive,
                resource_name: format!("resource-{index}"),
                request_digest: format!("sha256:{index}"),
                request: serde_json::json!({}),
                phase: OperationPhase::Succeeded,
                accepted_at_unix: 1,
                started_at_unix: Some(2),
                completed_at_unix: Some(3),
                artifact: Some(OperationArtifact {
                    media_type: "application/json".into(),
                    digest: proofstorm_core::digest_json(&content),
                    byte_length: u32::try_from(content.to_string().len()).unwrap(),
                    content,
                }),
            }
        };
        // A successful history query may return a failed payment. Raw output
        // need not have a projection result; neither fact changes command exit.
        let query = compact_operation_wait(operation(1), false);
        assert_eq!(query.phase, OperationPhase::Succeeded);
        let native = query.native_result.unwrap();
        assert_eq!(native["exit_code"], 0);
        assert!(native.get("projection_succeeded").is_none());
        let mut missing = operation(7);
        missing.phase = OperationPhase::Failed;
        missing.artifact = None;
        let unknown = compact_operation_wait(missing, false);
        assert_eq!(unknown.phase, OperationPhase::Failed);
        assert!(unknown.terminal && unknown.native_result.is_none());
        let ids = vec!["diag-1".into(), "missing".into(), "diag-4".into()];
        let (good, errors) = partition_operation_results(
            &ids,
            vec![
                Ok(Json(operation(1))),
                Err(coded_invalid_request("missing", "unknown operation")),
                Ok(Json(operation(4))),
            ],
        );
        assert_eq!(
            good.iter().map(|op| op.id.as_str()).collect::<Vec<_>>(),
            ["diag-1", "diag-4"]
        );
        assert_eq!(errors[0].operation_id, "missing");
        let result =
            compact_operation_wait_many((1..=6).map(operation).collect(), errors, false).unwrap();
        assert!(result.artifact_bodies_omitted);
        assert!(!result.all_terminal);
        assert!(!result.timed_out);
        assert_eq!(result.operations[3].phase, OperationPhase::Succeeded);
        assert_eq!(
            result.operations[3].native_result.as_ref().unwrap()["exit_code"],
            7
        );
        assert_eq!(
            result.operations[3].native_result.as_ref().unwrap()["cleanup_verified"],
            true
        );
        assert!(
            result
                .operations
                .iter()
                .all(|op| op.artifact.is_none() && op.artifact_digest.is_some())
        );
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("private-reference"));
        assert!(json.len() < MAX_AGENT_RESPONSE_BYTES);
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
        let valid_batch = OperationWaitRequest {
            operation_ids: vec!["operation-a".into(), "operation-b".into()],
            timeout_seconds: 120,
        };
        assert!(validate_operation_wait_request(&valid_batch).is_ok());
        assert!(
            validate_operation_wait_request(&OperationWaitRequest {
                operation_ids: (0..150).map(|index| format!("operation-{index}")).collect(),
                timeout_seconds: 30,
            })
            .is_ok()
        );
        let empty_error = validate_operation_wait_request(&OperationWaitRequest {
            operation_ids: Vec::new(),
            timeout_seconds: 30,
        })
        .expect_err("empty batch must refuse");
        assert_eq!(
            empty_error.data.unwrap()["code"],
            "operation_wait_count_invalid"
        );
        let duplicate_error = validate_operation_wait_request(&OperationWaitRequest {
            operation_ids: vec!["same".into(), "same".into()],
            timeout_seconds: 30,
        })
        .expect_err("duplicate IDs must refuse");
        assert_eq!(
            duplicate_error.data.expect("structured duplicate error")["code"],
            "operation_wait_duplicate_id"
        );
        let schema = serde_json::to_string(&schemars::schema_for!(OperationWaitRequest))
            .expect("batch wait schema");
        assert!(schema.contains("\"minItems\":1"));
        assert!(!schema.contains("\"maxItems\""));
        assert!(!proofstorm_app::cell::wait_terminal(InstancePhase::Ready));
        assert!(proofstorm_app::cell::wait_terminal(InstancePhase::Closed));
        assert!(proofstorm_app::cell::wait_terminal(
            InstancePhase::CleanupBlocked
        ));
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
                .contains(&"operation_wait".to_owned())
        );
        assert!(
            !restricted
                .tool_names()
                .contains(&"operation_wait".to_owned())
        );
        store
            .grant("alpha", "designer", Capability::ArtifactRead)
            .expect("artifact grant");
        let authorized = ProofstormMcp::new(store, "alpha", "designer").expect("wait session");
        assert!(
            authorized
                .tool_names()
                .contains(&"operation_wait".to_owned())
        );
        assert!(
            authorized
                .tool_names()
                .contains(&"operation_wait".to_owned())
        );
    }

    #[test]
    fn cell_status_receipt_and_page_cursors_are_compact_and_snapshot_bound() {
        let status = CellInstanceStatus {
            observed_generation: 1,
            observed_revision_digest: String::new(),
            last_converged_revision: None,
            retained_storage: BTreeMap::new(),
            instance: CellInstance {
                generation: 1,
                id: "instance-one".into(),
                workspace_id: "alpha".into(),
                revision_digest: "sha256:revision".into(),
                lock_digest: "sha256:lock".into(),
                instance_key: "i0123456789abcdef0123".into(),
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
        let receipt = compact_cell_status(status.clone());
        assert_eq!(receipt.total_components, 0);
        assert_eq!(receipt.inventory_count, 1);
        assert!(receipt.inventory_digest.starts_with("sha256:"));
        assert!(receipt.runtime_guidance.as_deref().is_some_and(|guidance| {
            guidance.contains("run_id is optional and session attribution is automatic")
        }));
        let encoded = serde_json::to_string(&receipt).expect("status receipt");
        assert!(!encoded.contains("\"components\":["));
        assert!(!encoded.contains("inventory\":"));
        assert_eq!(receipt.instance_key, "i0123456789abcdef0123");
        assert!(serialized_size(&receipt).expect("status size") < 1024);

        let mut live = status.clone();
        live.components.push(
            serde_json::from_value(serde_json::json!({
                "id":"chain", "kind":"bitcoin", "observed_revision_digest":"revision",
                "observed_rollout_digest":"rollout", "conditions":[], "ready":false,
                "service":"chain", "ports":{},
            }))
            .unwrap(),
        );
        let identity = component_status_identity(&live);
        let observation = digest_json(&live.components);
        live.components[0].ready = true;
        assert_eq!(component_status_identity(&live), identity);
        assert_ne!(digest_json(&live.components), observation);
        live.instance.generation += 1;
        assert_ne!(component_status_identity(&live), identity);
        live.instance.generation -= 1;
        live.components[0].id = "replacement".into();
        assert_ne!(component_status_identity(&live), identity);

        let close_receipt = compact_cell_wait(status, InstancePhase::Closed, false, false);
        assert_eq!(close_receipt.phase, InstancePhase::Ready);
        assert_eq!(close_receipt.target_phase, InstancePhase::Closed);
        assert!(!close_receipt.reached);
        let encoded = serde_json::to_string(&close_receipt).expect("close receipt");
        assert!(!encoded.contains("\"components\":["));
        assert!(!encoded.contains("inventory\":"));
        assert_eq!(receipt.instance_key, "i0123456789abcdef0123");
        assert!(serialized_size(&close_receipt).expect("close receipt size") < 1024);

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

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one complete startup blocker fixture exercises the wait contract"
    )]
    async fn cell_wait_returns_startup_blockers_without_waiting_for_timeout() {
        use proofstorm_core::{
            ComponentCondition, ComponentConditionReason as Reason, ComponentConditionState,
            ComponentConditionType,
        };
        let store = seeded_store();
        store
            .create_draft("alpha", "designer", "blocked", &cell("blocked"), "create")
            .unwrap();
        let revision = store
            .publish("alpha", "designer", "blocked", 1, "publish")
            .unwrap();
        let instance = store
            .materialize(
                "alpha",
                "designer",
                "blocked",
                &revision.digest,
                "materialize",
            )
            .unwrap();
        let component = ComponentStatus {
            protocol_observation: None,
            id: "wallet-cdk".into(), kind: proofstorm_core::ComponentKind::Wallet,
            observed_revision_digest: revision.digest.clone(), observed_rollout_digest: "rollout".into(),
            ready: false, service: String::new(), ports: BTreeMap::new(),
            conditions: vec![ComponentCondition {
                condition_type: ComponentConditionType::WorkloadReady, state: ComponentConditionState::False,
                reason: Reason::ImagePullBackoff,
                message: "Image pull is failing, not building. Run storm doctor for this installation; verify image availability and registry access.".into(),
                last_transition_unix: 1,
            }],
        };
        let mut resource = proofstorm_kube::ProofstormCell::new(
            &instance.resource_name,
            proofstorm_kube::ProofstormCellSpec {
                workspace_id: "alpha".into(),
                instance_id: instance.id.clone(),
                instance_key: instance.instance_key,
                revision_digest: revision.digest.clone(),
                lock: revision.lock,
                cell: revision.cell,
            },
        );
        resource.status = Some(proofstorm_kube::ProofstormCellStatus {
            components: vec![component],
            observed_revision_digest: revision.digest,
            ..Default::default()
        });
        let body = serde_json::to_string(&resource).unwrap();
        let client = kube::Client::new(
            tower::service_fn(move |_: http::Request<kube::client::Body>| {
                std::future::ready(Ok::<_, std::io::Error>(http::Response::new(
                    kube::client::Body::from(body.clone().into_bytes()),
                )))
            }),
            "system",
        );
        let service = ProofstormMcp::new(store, "alpha", "designer")
            .unwrap()
            .with_kubernetes(client, "system");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.proofstorm_cell_wait(Parameters(CellWaitRequest {
                expected_instance_key: None,
                expected_generation: None,
                instance_id: "blocked".into(),
                target_phase: InstancePhase::Ready,
                timeout_seconds: 120,
            })),
        )
        .await
        .expect("blocked startup must return immediately")
        .unwrap()
        .0;
        assert!(!result.reached);
        assert!(!result.timed_out);
        assert_eq!(result.blockers[0].component_id, "wallet-cdk");
        assert_eq!(result.blockers[0].reason, Reason::ImagePullBackoff);
        assert!(result.blockers[0].message.contains("storm doctor"));
        let message = result.message.as_deref().unwrap();
        assert!(message.contains("startup is blocked"));
        let summary = compact_cell_status(service.full_cell_status("blocked").await.unwrap());
        assert_eq!(summary.blockers.len(), 1);
        let detail = service
            .proofstorm_cell_component_status_list(Parameters(CellComponentStatusListRequest {
                instance_id: "blocked".into(),
                limit: 20,
                cursor: None,
                component: None,
                ready: None,
                query: String::new(),
                regex: false,
                case_insensitive: false,
                scan: false,
                fields: vec![],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            detail.components[0]["conditions"][0]["reason"],
            serde_json::json!(Reason::ImagePullBackoff)
        );
    }

    #[test]
    fn artifact_export_agent_schema_cannot_request_bulk_content() {
        let schema = schemars::schema_for!(EvidenceExportRequest);
        let encoded = serde_json::to_string(&schema).expect("artifact export schema");
        assert!(!encoded.contains("include_content"));
        assert!(encoded.contains("artifact_operation_ids"));
        assert!(!encoded.contains("\"maxItems\""));
        assert!(encoded.contains("Do not enumerate"));
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the lifecycle regression covers empty, active, and terminal journal finalization"
    )]
    async fn experiment_close_reconciles_before_finalization_and_fails_closed() {
        let store = seeded_store();
        for capability in [
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::ExperimentClose,
            Capability::CellOperate,
            Capability::ExperimentRead,
            Capability::WalletControl,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("experiment finalization grant");
        }
        store
            .create_draft(
                "alpha",
                "designer",
                "finalization-cell",
                &cell("finalization-cell"),
                "create-finalization-cell",
            )
            .expect("draft");
        let revision = store
            .publish(
                "alpha",
                "designer",
                "finalization-cell",
                1,
                "publish-finalization-cell",
            )
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "finalization-instance",
                &revision.digest,
                "materialize-finalization-cell",
            )
            .expect("instance");
        store
            .create_experiment(
                "alpha",
                "designer",
                "empty-finalization",
                "finalization-instance",
                "create-empty-finalization",
            )
            .expect("empty experiment");
        let service =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("finalization session");
        let closed = service
            .proofstorm_run_finish(Parameters(RunFinishRequest {
                experiment_id: "empty-finalization".into(),
                idempotency_key: "close-empty-finalization".into(),
            }))
            .await
            .expect("an experiment without active actions closes")
            .0;
        assert_eq!(closed.phase, ExperimentPhase::Closed);

        store
            .create_experiment(
                "alpha",
                "designer",
                "active-finalization",
                "finalization-instance",
                "create-active-finalization",
            )
            .expect("active experiment");
        store
            .start_session(
                "alpha",
                "designer",
                "active-finalization",
                "active-finalization-session",
                "acquire-active-finalization-session",
            )
            .expect("session");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "finalization-instance",
                "active-finalization",
                "active-finalization-session",
                "active-finalization-balance",
                OperationKind::WalletBalance,
                &serde_json::json!({"wallet": "wallet", "mint": "mint"}),
                "create-active-finalization-balance",
                Capability::WalletControl,
            )
            .expect("active operation");
        store
            .finish_session(
                "alpha",
                "designer",
                "active-finalization-session",
                "release-active-finalization-session",
            )
            .expect("release session");
        let Err(error) = service
            .proofstorm_run_finish(Parameters(RunFinishRequest {
                experiment_id: "active-finalization".into(),
                idempotency_key: "close-active-finalization".into(),
            }))
            .await
        else {
            panic!("an unreconciled action must prevent experiment close");
        };
        assert_eq!(
            error.data.expect("runtime error data")["code"],
            "runtime_unavailable"
        );
        assert_eq!(
            store
                .experiment("alpha", "designer", "active-finalization")
                .expect("active experiment remains readable")
                .phase,
            ExperimentPhase::Active
        );

        store
            .record_operation_result(
                "alpha",
                &operation.id,
                OperationPhase::Succeeded,
                serde_json::json!({"balance_sat": 100}),
            )
            .expect("terminal result");
        let closed = service
            .proofstorm_run_finish(Parameters(RunFinishRequest {
                experiment_id: "active-finalization".into(),
                idempotency_key: "close-active-finalization".into(),
            }))
            .await
            .expect("terminal journal closes without a runtime")
            .0;
        assert_eq!(closed.phase, ExperimentPhase::Closed);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression fixture proves terminalization across the complete durable workflow"
    )]
    fn invalid_runtime_quote_artifact_fails_terminally_without_stranding_a_slot() {
        let store = seeded_store();
        for capability in [
            Capability::ExperimentCreate,
            Capability::CellOperate,
            Capability::WalletControl,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("runtime result grant");
        }
        store
            .create_draft(
                "alpha",
                "designer",
                "artifact-contract-cell",
                &cell("artifact-contract-cell"),
                "create-artifact-contract-cell",
            )
            .expect("draft");
        let revision = store
            .publish(
                "alpha",
                "designer",
                "artifact-contract-cell",
                1,
                "publish-artifact-contract-cell",
            )
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "artifact-contract-instance",
                &revision.digest,
                "materialize-artifact-contract-cell",
            )
            .expect("instance");
        store
            .create_experiment(
                "alpha",
                "designer",
                "artifact-contract-experiment",
                "artifact-contract-instance",
                "create-artifact-contract-experiment",
            )
            .expect("experiment");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "artifact-contract-instance",
                "artifact-contract-experiment",
                "artifact-contract-session",
                "artifact-contract-pay",
                OperationKind::WalletPay,
                &serde_json::json!({
                    "wallet": "payer",
                    "mint": "payer-mint",
                    "recipient_wallet": "recipient",
                    "recipient_mint": "recipient-mint",
                    "mint_quote_id": "quote"
                }),
                "create-artifact-contract-pay",
                Capability::WalletControl,
            )
            .expect("wallet payment operation");
        let service = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        let completed = service
            .record_runtime_terminal_result(
                &operation,
                OperationPhase::Succeeded,
                serde_json::json!({
                    "input_fee_sat": 1,
                    "input_proof_count": 1,
                    "quote_observations": [{
                        "role": "payment_melt",
                        "wallet_id": "payer",
                        "mint_id": "payer-mint",
                        "direction": "pay",
                        "quote_id": "melt",
                        "amount_sat": 1_000,
                        "state": "PAID",
                        "fee_reserve_sat": 10,
                        "fee_paid_sat": 1,
                        "input_fee_sat": 1
                    }]
                }),
            )
            .expect("invalid producer output is recorded as a terminal failure");
        assert_eq!(completed.phase, OperationPhase::Failed);
        assert_eq!(
            completed.artifact.expect("failure artifact").content["code"],
            "invalid_wallet_quote_observation"
        );
        assert!(
            store
                .active_operations("alpha", "artifact-contract-instance")
                .expect("active operations")
                .is_empty(),
            "a bad terminal artifact must not consume an active-operation slot"
        );
    }

    #[test]
    fn reachability_oracle_is_capability_filtered_and_bounded() {
        let store = seeded_store();
        let denied =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("denied session");
        assert!(!denied.tool_names().contains(&"network_probe".to_owned()));
        store
            .grant("alpha", "designer", Capability::OracleRun)
            .expect("oracle grant");
        let allowed = ProofstormMcp::new(store, "alpha", "designer").expect("allowed session");
        assert!(allowed.tool_names().contains(&"network_probe".to_owned()));
        assert!(
            validate_reachability_oracle_bounds(&NetworkProbeRequest {
                instance_id: String::new(),
                experiment_id: String::new(),
                session_id: String::new(),
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
            validate_reachability_oracle_bounds(&NetworkProbeRequest {
                instance_id: String::new(),
                experiment_id: String::new(),
                session_id: String::new(),
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

    fn closed_archive_fixture() -> (Store, PublishedRevision) {
        let store = seeded_store();
        for capability in [
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::ExperimentClose,
            Capability::CellOperate,
            Capability::OracleRun,
            Capability::ArtifactRead,
        ] {
            store.grant("alpha", "designer", capability).unwrap();
        }
        store
            .create_draft("alpha", "designer", "archive", &cell("archive"), "create")
            .unwrap();
        let revision = store
            .publish("alpha", "designer", "archive", 1, "publish")
            .unwrap();
        store
            .materialize(
                "alpha",
                "designer",
                "archive-instance",
                &revision.digest,
                "materialize",
            )
            .unwrap();
        store
            .create_experiment(
                "alpha",
                "designer",
                "archive-experiment",
                "archive-instance",
                "experiment",
            )
            .unwrap();
        store
            .start_session(
                "alpha",
                "designer",
                "archive-experiment",
                "archive-session",
                "session",
            )
            .unwrap();
        for index in 1..=125 {
            let id = format!("observation-{index}");
            let operation = store
                .create_operation(
                    "alpha",
                    "designer",
                    "archive-instance",
                    "archive-experiment",
                    "archive-session",
                    &id,
                    OperationKind::ConservationOracle,
                    &serde_json::json!({"expected_sat":100,"tolerance_sat":0}),
                    &id,
                    Capability::OracleRun,
                )
                .unwrap();
            store
                .record_operation_result(
                    "alpha",
                    &operation.id,
                    OperationPhase::Succeeded,
                    serde_json::json!({"synthetic_fixture":true,"diagnostic":"x".repeat(8192)}),
                )
                .unwrap();
        }
        store
            .finish_session("alpha", "designer", "archive-session", "finish")
            .unwrap();
        store
            .close_experiment("alpha", "designer", "archive-experiment", "close")
            .unwrap();

        (store, revision)
    }

    #[test]
    fn large_closed_archives_export_offline_and_remain_pageable() {
        let (store, revision) = closed_archive_fixture();
        let reader = ProofstormMcp::new(store.clone(), "alpha", "reader")
            .unwrap()
            .offline();
        let read = reader
            .proofstorm_cell_read(Parameters(
                serde_json::from_value(serde_json::json!({"name":"archive-instance"})).unwrap(),
            ))
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(read["value"], serde_json::json!(revision.cell));
        assert_eq!(read["version"], 1);
        let service = ProofstormMcp::new(store, "alpha", "designer")
            .unwrap()
            .offline();
        assert!(service.tool_names().contains(&"evidence_export".into()));
        assert!(
            service
                .tool_names()
                .contains(&"evidence_section_read".into())
        );
        let ids = (1..=40)
            .map(|index| format!("observation-{index}"))
            .collect::<Vec<_>>();
        let request = EvidenceExportRequest {
            experiment_id: "archive-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: ids.clone(),
            include_content: false,
        };
        let manifest = service
            .proofstorm_evidence_export(Parameters(request.clone()))
            .unwrap()
            .0;
        let repeated = service
            .proofstorm_evidence_export(Parameters(request))
            .unwrap()
            .0;
        assert_eq!(manifest, repeated);
        assert_eq!(manifest.journal_count, 125);
        assert_eq!(manifest.artifact_count, 125);
        assert!(manifest.byte_length > 512 * 1024);
        assert!(serialized_size(&manifest).unwrap() < MAX_AGENT_RESPONSE_BYTES);
        let mut after_sequence = 100;
        let mut sequences = Vec::new();
        loop {
            let page = service
                .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
                    experiment_id: "archive-experiment".into(),
                    include_oracle_artifacts: true,
                    artifact_operation_ids: ids.clone(),
                    section: EvidenceSection::Journal,
                    pointer: String::new(),
                    operation_id: None,
                    capture_id: None,
                    after_sequence,
                    limit: 50,
                }))
                .unwrap()
                .0;
            assert_eq!(page.evidence_digest, manifest.digest);
            assert!(read_query::wire_size(&page).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
            sequences.extend(
                page.data
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| entry["sequence"].as_u64().unwrap()),
            );
            let Some(next) = page.next_after_sequence else {
                break;
            };
            assert!(next > after_sequence);
            after_sequence = next;
        }
        assert_eq!(sequences, (101..=125).collect::<Vec<_>>());
        let marker = service
            .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
                experiment_id: "archive-experiment".into(),
                include_oracle_artifacts: true,
                artifact_operation_ids: ids,
                section: EvidenceSection::Artifact,
                pointer: "/artifact/content/synthetic_fixture".into(),
                operation_id: Some("observation-125".into()),
                capture_id: None,
                after_sequence: 0,
                limit: 50,
            }))
            .unwrap()
            .0;
        assert_eq!(marker.data, true);
        assert_eq!(marker.evidence_digest, manifest.digest);
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
            Capability::CellOperate,
            Capability::ExperimentRead,
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
                "evidence-cell",
                &cell("evidence-cell"),
                "create-evidence-cell",
            )
            .expect("draft");
        let revision = store
            .publish(
                "alpha",
                "designer",
                "evidence-cell",
                1,
                "publish-evidence-cell",
            )
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "evidence-instance",
                &revision.digest,
                "materialize-evidence-cell",
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
            .start_session(
                "alpha",
                "designer",
                "evidence-experiment",
                "evidence-session",
                "acquire-evidence-session",
            )
            .expect("session");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "evidence-instance",
                "evidence-experiment",
                "evidence-session",
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
        let Err(error) = active.proofstorm_evidence_export(Parameters(EvidenceExportRequest {
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
            .finish_session(
                "alpha",
                "designer",
                "evidence-session",
                "release-evidence-session",
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
        let request = EvidenceExportRequest {
            experiment_id: "evidence-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: vec![],
            include_content: true,
        };
        let first = restarted
            .proofstorm_evidence_export(Parameters(request.clone()))
            .expect("first export")
            .0;
        let second = restarted
            .proofstorm_evidence_export(Parameters(request))
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
        assert!(first.byte_length as usize <= MAX_AGENT_RESPONSE_BYTES);
        let encoded = serde_json::to_string(&first).expect("serialize evidence");
        assert!(!encoded.contains("resource_name"));
        assert!(!encoded.contains("instance_key"));
        assert!(!encoded.contains("kubernetes"));

        let journal=restarted.proofstorm_evidence_section_read(Parameters(serde_json::from_value(serde_json::json!({"run_id":"evidence-experiment","section":"journal","limit":50})).unwrap())).unwrap().0;
        assert_eq!(journal.data.as_array().unwrap().len(), 1);
        assert!(journal.next_after_sequence.is_none());
        assert!(serialized_size(&journal).unwrap() <= MAX_AGENT_RESPONSE_BYTES);

        let manifest = restarted
            .proofstorm_evidence_export(Parameters(EvidenceExportRequest {
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
        assert!(manifest.journal_complete);
        assert!(manifest.artifact_bodies_optional);
        assert!(manifest.guidance.contains("Do not retry"));
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
                capture_id: None,
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
                capture_id: None,
                after_sequence: 0,
                limit: 1,
            }))
            .expect("journal section")
            .0;
        assert_eq!(journal_section.data.as_array().map(Vec::len), Some(1));
        assert!(journal_section.next_after_sequence.is_none());
    }
}

#[cfg(test)]
mod lifecycle_tests;

fn public_endpoints(endpoints: &[CatalogRuntimeEndpoint]) -> Vec<CatalogRuntimeEndpoint> {
    endpoints
        .iter()
        .cloned()
        .map(|mut endpoint| {
            endpoint.controls = endpoint
                .controls
                .iter()
                .filter_map(|name| {
                    let name = match name.as_str() {
                        "component_exec_live" => "cell_exec",
                        "node_start" => "component_start",
                        "node_stop" => "component_stop",
                        "node_restart" => "component_restart",
                        "reachability_oracle" => "network_probe",
                        other => other,
                    };
                    proofstorm_core::mcp::tool(name).map(|_| name.to_owned())
                })
                .collect();
            if endpoint.id == "component" {
                endpoint.controls.extend(
                    [
                        "cell_exec",
                        "component_forensics",
                        "component_start",
                        "component_stop",
                        "component_restart",
                    ]
                    .map(str::to_owned),
                );
            }
            if endpoint.kind != "wallet" {
                endpoint.controls.remove("wallet_balance");
            }
            endpoint.limitations = endpoint
                .limitations
                .into_iter()
                .filter(|line| {
                    !line.starts_with("liquidity_bootstrap ")
                        && !line.starts_with("wallet_fund is unavailable")
                })
                .map(|line| {
                    line.replace("component_exec_live", "cell_exec")
                        .replace("All native and typed operations", "Native operations")
                })
                .collect();
            endpoint
        })
        .collect()
}
