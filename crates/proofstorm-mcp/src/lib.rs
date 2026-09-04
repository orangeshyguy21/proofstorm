use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Patch, PatchParams},
};
use proofstorm_core::{
    API_VERSION, AuthenticationProtocol, BitcoinNetwork, Capability, CatalogDependencySupport,
    CatalogEntry, CatalogFeature, CatalogResponse, CatalogRuntimeEndpoint, CatalogSupportMatrix,
    ComponentKind, ComponentSpec, ComponentStatus, ControlClass, DatabaseRole, DependencyBinding,
    DraftMutation, EVIDENCE_API_VERSION, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance, Experiment, ExperimentLease, ExperimentPhase,
    InstancePhase, InventoryEntry, LabInstance, LabInstanceStatus, LabOperation, LabPolicy,
    LabSpec, LinkKind, LinkSpec, MAX_NETWORK_DELAY_MS, MAX_NETWORK_JITTER_MS,
    MAX_NETWORK_LOSS_BASIS_POINTS, NetworkFaultBackend, NetworkFaultDirection, NetworkFaultFeature,
    OperationArtifact, OperationKind, OperationPhase, PaymentMethod, PublishedRevision,
    ReleaseChannel, SupportLifecycle, TeardownReceipt as CoreTeardownReceipt, ValidationIssue,
    WalletQuoteDirection, WalletQuoteObservation, WalletQuoteObservationInput,
    WalletQuoteObservationRole, default_catalog, digest_json, network_policy_fault_backend,
    validate_lab, wallet_quote_observations_from_artifact,
};
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionPhase, AuthenticationConformanceAction,
    AuthenticationProtectedSpendAction, AuthenticationReplayAction, BootstrapLiquidityAction,
    ChannelCloseAction, ChannelOpenAction, ChannelPolicySetAction, ChannelRebalanceAction,
    ComponentExecLiveAction, ComponentForensicsAction, ComponentLogsAction, LabAction, LabPhase,
    NetworkHealAction, NetworkPartitionAction, NodeControlAction, PeerConnectAction,
    PeerDisconnectAction, ProofstormLab, ProofstormLabAction, ProofstormLabActionSpec,
    ProofstormLabSpec, ReachabilityOracleAction, WalletBalanceAction, WalletFundAction,
    WalletInitializeAction, WalletInvoiceAction, WalletMeltQuoteRefreshAction, WalletPayAction,
    WalletQuoteClaimAction, WalletRoundTripAction, component_ports,
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
    #[serde(deserialize_with = "deserialize_authored_lab")]
    pub lab: AuthoredLabSpec,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LabRecipe {
    NutshellLndClnRoutingFees,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLabRecipeRequest {
    pub draft_id: String,
    pub idempotency_key: String,
    pub recipe: LabRecipe,
    /// Human-readable lab name. Defaults to `draft_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Catalog-backed component role for a generic lab plan. Kind, adapter
/// contract, and preferred version are resolved by Proofstorm.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPlanComponentInput {
    pub id: String,
    /// Catalog implementation ID, for example bitcoin-core, lnd, cln,
    /// nutshell, cdk-ldk, or nutshell-wallet. This remains an open string so
    /// newly registered implementations require no MCP schema change.
    pub implementation: String,
    /// Omit to select the catalog's preferred supported version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Omit to use the implementation's safe role default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlClass>,
    /// Agent-authorable configuration overrides. Backend defaults and
    /// topology-derived settings are resolved during publication.
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
}

/// Generic topology edge. Dependency bindings are inferred from the selected
/// catalog entries whenever there is one compatible choice.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LabPlanConnectionInput {
    BitcoinPeer {
        id: String,
        node_a: String,
        node_b: String,
    },
    LightningPeer {
        id: String,
        node_a: String,
        node_b: String,
    },
    ChainBackend {
        id: String,
        /// The Lightning or other chain-dependent component.
        component: String,
        /// The Bitcoin implementation serving this component.
        chain: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<BitcoinNetwork>,
    },
    PaymentBackend {
        id: String,
        /// The mint whose invoices and payments use the backend.
        mint: String,
        /// The Lightning node serving the mint.
        lightning: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<PaymentMethod>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    DatabaseBackend {
        id: String,
        /// The component storing data in the database.
        component: String,
        /// The database implementation serving the component.
        database: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<DatabaseRole>,
    },
    AuthenticationBackend {
        id: String,
        /// The component protected by authentication.
        component: String,
        /// The identity provider serving the component.
        identity_provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<AuthenticationProtocol>,
    },
    NetworkPath {
        id: String,
        source: String,
        target: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPlanRequest {
    pub plan_id: String,
    pub components: Vec<LabPlanComponentInput>,
    pub connections: Vec<LabPlanConnectionInput>,
    /// Required. Runtime controls the experiment will need, grouped by
    /// component and logical endpoint. Use an empty array only for a plan-only
    /// request with no intended runtime work. Proofstorm checks these before
    /// storage so an unavailable driver cannot become a live-lab failure.
    pub runtime_requirements: Vec<LabPlanRuntimeRequirement>,
    #[serde(default)]
    pub policy: LabPolicy,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPlanRuntimeRequirement {
    pub component: String,
    /// Defaults to the component's primary `component` endpoint. Embedded
    /// endpoint IDs are discovered from `catalog_entry_read`.
    #[serde(default = "default_runtime_endpoint")]
    pub endpoint: String,
    /// Open control identifiers such as `channel_open`, `wallet_pay`,
    /// `node_restart`, or `authentication_replay`.
    pub controls: BTreeSet<String>,
}

fn default_runtime_endpoint() -> String {
    "component".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPlanResolvedComponent {
    pub id: String,
    pub kind: ComponentKind,
    pub implementation: String,
    pub version: String,
    pub config_version: String,
    pub control: ControlClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPlanResolvedRuntimeEndpoint {
    pub component: String,
    pub endpoint: String,
    pub kind: String,
    pub controls: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPlanReceipt {
    pub plan_id: String,
    pub plan_digest: String,
    pub version: u64,
    pub components: Vec<LabPlanResolvedComponent>,
    pub runtime_endpoints: Vec<LabPlanResolvedRuntimeEndpoint>,
    pub connections: Vec<LinkSpec>,
    pub validation: LabValidationResult,
    pub next_tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabApplyRequest {
    pub plan_id: String,
    pub expected_plan_digest: String,
    pub instance_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabApplyReceipt {
    pub plan_id: String,
    pub plan_digest: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub instance_id: String,
    pub phase: InstancePhase,
    pub component_count: u32,
    pub next_tool: String,
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
    /// Controls accepted by publication for this exact component release.
    pub allowed_control: Vec<ControlClass>,
    /// Safe default control for ordinary lab authoring.
    pub recommended_control: ControlClass,
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
    pub runtime_endpoints: Vec<CatalogRuntimeEndpoint>,
    pub image: String,
    pub source_digest: String,
    pub allowed_control: Vec<ControlClass>,
    /// Safe default control for ordinary lab authoring.
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditDraftRequest {
    pub draft_id: String,
    pub expected_version: u64,
    #[serde(deserialize_with = "deserialize_authored_lab")]
    pub lab: AuthoredLabSpec,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftMutationResult {
    pub draft_id: String,
    pub version: u64,
    pub component_count: u32,
    pub link_count: u32,
    pub structure: String,
    pub topology_digest: String,
    pub valid: bool,
    pub warnings: Vec<String>,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabValidationResult {
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
    pub component_ids: Vec<String>,
    pub link_ids: Vec<String>,
    pub warnings: Vec<String>,
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
    let value = match value {
                    from,
            serde_json::from_str(&encoded).map_err(serde::de::Error::custom)?
                    method,
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
                })
    let canonical: LabSpec = match serde_json::from_value(value) {
        Ok(canonical) => canonical,
        Err(_) => return Err(serde::de::Error::custom(authored_error)),
    };
    Ok(AuthoredLabSpec {
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
    let value = match value {
                    from,
            serde_json::from_str(&encoded).map_err(serde::de::Error::custom)?
                    method,
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
                })
    let canonical: LabSpec = match serde_json::from_value(value) {
        Ok(canonical) => canonical,
        Err(_) => return Err(serde::de::Error::custom(authored_error)),
    };
    Ok(AuthoredLabSpec {
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

/// Complete lab input accepted at the MCP boundary. Unlike the persisted core
/// model, backend-link variants require their binding fields as flat scalars.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoredLabSpec {
    pub api_version: String,
    pub name: String,
    pub components: Vec<ComponentSpec>,
    pub links: Vec<AddLinkInput>,
    /// Optional capability restrictions. Omit this field for the safe default
    /// policy unless the experiment deliberately needs a narrower envelope.
    #[serde(default)]
    pub policy: LabPolicy,
}

/// Accept the canonical JSON object and the common MCP-client failure mode where
/// that object is encoded one extra time as a JSON string. Parsing still lands
/// in the same strict `AuthoredLabSpec` contract, including unknown-field and
/// required-field checks; malformed or incomplete strings remain fail-closed.
fn deserialize_authored_lab<'de, D>(deserializer: D) -> Result<AuthoredLabSpec, D::Error>
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
    let canonical: LabSpec = match serde_json::from_value(value) {
        Ok(canonical) => canonical,
        Err(_) => return Err(serde::de::Error::custom(authored_error)),
    };
    Ok(AuthoredLabSpec {
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

impl TryFrom<AuthoredLabSpec> for LabSpec {
    type Error = String;

    fn try_from(input: AuthoredLabSpec) -> Result<Self, Self::Error> {
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
pub struct ValidateLabRequest {
    #[serde(deserialize_with = "deserialize_authored_lab")]
    pub lab: AuthoredLabSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MutateLinkRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub link: AddLinkInput,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveLinkRequest {
    pub draft_id: String,
    pub expected_version: u64,
    pub link_id: String,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentExecLiveRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub component: String,
    /// An unrestricted non-interactive shell program executed inside the
    /// selected running component container.
    pub script: String,
    pub timeout_seconds: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletMeltQuoteRefreshRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub melt_quote_id: String,
    /// Bounded wallet-to-database/mint round-trip deadline.
    #[schemars(range(min = 1, max = 30))]
    pub timeout_seconds: u32,
    pub idempotency_key: String,
}

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
pub struct OperationWaitManyRequest {
    /// Unique operation IDs to await together. Start independent operations
    /// first, then prefer this over repeated single-operation waits.
    #[schemars(length(min = 1, max = 8))]
    pub operation_ids: Vec<String>,
    /// Shared server-side wait bound in 1..=120 seconds.
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationWaitManyResult {
    /// Results preserve the request order.
    pub operations: Vec<OperationWaitResult>,
    pub all_terminal: bool,
    pub timed_out: bool,
    /// True only when optional artifact bodies were removed to keep the batch
    /// response within the agent response budget. Use `operation_wait` for any
    /// one omitted body.
    pub artifact_bodies_omitted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentExecLiveRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub component: String,
    /// An unrestricted non-interactive shell program executed inside the
    /// selected running component container.
    pub script: String,
    pub timeout_seconds: u32,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletMeltQuoteRefreshRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    pub melt_quote_id: String,
    /// Bounded wallet-to-database/mint round-trip deadline.
    #[schemars(range(min = 1, max = 30))]
    pub timeout_seconds: u32,
    pub idempotency_key: String,
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

/// Runtime identifiers for a server-owned lab recipe. Recipe-specific
/// component identities and safe liquidity values are deliberately not
/// caller-controlled.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabRecipeSetupRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    pub recipe: LabRecipe,
    pub idempotency_key: String,
}

/// One auditable, server-orchestrated payment matrix for a built-in recipe.
/// All component roles, wallet roles, amounts, and below/above-reserve fee
/// levels are recipe-owned so they cannot be accidentally omitted or crossed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabRecipeFeeMatrixRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    /// Stable correlation ID for the 26 journaled child operations. Proofstorm
    /// derives bounded internal operation IDs from it. Replaying the same
    /// matrix ID and idempotency key is idempotent.
    pub matrix_id: String,
    pub recipe: LabRecipe,
    pub idempotency_key: String,
}

const ROUTING_FEE_RECIPE_FUNDING_SAT: u64 = 10_000_000;
const ROUTING_FEE_RECIPE_CHANNEL_SAT: u64 = 2_000_000;
const ROUTING_FEE_RECIPE_CLN_PUSH_SAT: u64 = 1_000_000;
const ROUTING_FEE_RECIPE_WALLET_FUNDING_SAT: u64 = 50_000;
const ROUTING_FEE_RECIPE_PAYMENT_SAT: u64 = 5_000;
const ROUTING_FEE_RECIPE_LOW_BASE_FEE_SAT: u64 = 1;
const ROUTING_FEE_RECIPE_LOW_FEE_RATE_PPM: u32 = 100;
const ROUTING_FEE_RECIPE_HIGH_BASE_FEE_SAT: u64 = 100;
const ROUTING_FEE_RECIPE_HIGH_FEE_RATE_PPM: u32 = 100_000;

#[derive(Clone, Copy)]
struct RecipePaymentDirection {
    id: &'static str,
    label: &'static str,
    payer_wallet: &'static str,
    payer_mint: &'static str,
    recipient_wallet: &'static str,
    recipient_mint: &'static str,
    oracle_endpoint: &'static str,
}

const ROUTING_FEE_RECIPE_PAYMENT_DIRECTIONS: [RecipePaymentDirection; 2] = [
    RecipePaymentDirection {
        id: "lnd-to-cln",
        label: "lnd_to_cln",
        payer_wallet: "payer-lnd",
        payer_mint: "mint-lnd",
        recipient_wallet: "recipient-cln",
        recipient_mint: "mint-cln",
        oracle_endpoint: "lnd",
    },
    RecipePaymentDirection {
        id: "cln-to-lnd",
        label: "cln_to_lnd",
        payer_wallet: "payer-cln",
        payer_mint: "mint-cln",
        recipient_wallet: "recipient-lnd",
        recipient_mint: "mint-lnd",
        oracle_endpoint: "cln",
    },
];

fn recipe_fee_matrix_operation_prefix(request: &LabRecipeFeeMatrixRequest) -> String {
    let digest = digest_json(&(
        request.instance_id.as_str(),
        request.experiment_id.as_str(),
        request.lease_id.as_str(),
        request.matrix_id.as_str(),
        request.recipe,
    ));
    let hex = digest
        .strip_prefix("sha256:")
        .expect("digest_json always returns a sha256-prefixed digest");
    format!("matrix-{}", &hex[..16])
}

fn recipe_bootstrap_request(request: LabRecipeSetupRequest) -> BootstrapLiquidityRequest {
    match request.recipe {
        LabRecipe::NutshellLndClnRoutingFees => BootstrapLiquidityRequest {
            instance_id: request.instance_id,
            experiment_id: request.experiment_id,
            lease_id: request.lease_id,
            operation_id: request.operation_id,
            chain: "bitcoin-core".into(),
            mint_lightning: "lnd-backend".into(),
            payer_lightning: "lnd-router".into(),
            funding_sat: ROUTING_FEE_RECIPE_FUNDING_SAT,
            channel_sat: ROUTING_FEE_RECIPE_CHANNEL_SAT,
            push_sat: 0,
            idempotency_key: request.idempotency_key,
        },
    }
}

fn recipe_route_channel_request(request: LabRecipeSetupRequest) -> ChannelOpenRequest {
    match request.recipe {
        LabRecipe::NutshellLndClnRoutingFees => ChannelOpenRequest {
            instance_id: request.instance_id,
            experiment_id: request.experiment_id,
            lease_id: request.lease_id,
            operation_id: request.operation_id,
            chain: "bitcoin-core".into(),
            from_lightning: "lnd-router".into(),
            to_lightning: "cln-backend".into(),
            channel_sat: ROUTING_FEE_RECIPE_CHANNEL_SAT,
            push_sat: ROUTING_FEE_RECIPE_CLN_PUSH_SAT,
            idempotency_key: request.idempotency_key,
        },
    }
}

/// Projection of the canonical journal request used by later admission
/// checks. The journal deliberately omits transport-only idempotency keys.
#[derive(Debug, Clone, Deserialize)]
struct StoredBootstrapFunding {
    mint_lightning: String,
    payer_lightning: String,
    funding_sat: u64,
    channel_sat: u64,
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
    #[schemars(range(min = 1, max = 30))]
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
pub struct ChannelPolicySetRequest {
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub operation_id: String,
    /// Lightning node whose outgoing policy is updated.
    pub from_lightning: String,
    /// Peer that identifies the channel or channels to update.
    pub to_lightning: String,
    /// Base routing fee in satoshis. Proofstorm converts this to the
    /// milli-satoshi unit required by Lightning implementations.
    #[schemars(range(min = 0, max = 100_000))]
    pub base_fee_sat: u64,
    #[schemars(range(min = 0, max = 1_000_000))]
    pub fee_rate_ppm: u32,
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
    /// Earlier successful `wallet_balance` operation captured before treatment.
    pub baseline_operation_id: String,
    /// Successful `wallet_pay` operation after the baseline. A round trip mints
    /// external value first and is not a valid balance-invariance treatment.
    pub treatment_operation_id: String,
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
    /// Optional full bodies for at most 16 additional operations. Do not enumerate
    /// the experiment: every action and artifact descriptor is always in the journal.
    #[serde(default)]
    #[schemars(length(max = 16))]
    pub artifact_operation_ids: Vec<String>,
    /// Compatibility-only bulk response switch. Agent discovery intentionally
    /// omits this field; full content is available at the returned resource URI.
    #[serde(default)]
    #[schemars(skip)]
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
            | "proofstorm_component_restart"
            | "proofstorm_component_exec_live"
            | "proofstorm_component_forensics"

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSectionReadResponse {
    pub evidence_digest: String,
    pub section: EvidenceSection,
    pub data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_sequence: Option<u64>,
}

            | "proofstorm_wallet_melt_quote_refresh"
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
    /// One compact, cross-phase surface for an agent that must design, run,
    /// evidence, and tear down an experiment without restarting its session.
    Experiment,
    Design,
    Runtime,
    Evidence,
}

impl std::str::FromStr for ProofstormToolset {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "experiment" => Ok(Self::Experiment),
            "design" => Ok(Self::Design),
            "runtime" => Ok(Self::Runtime),
            "evidence" => Ok(Self::Evidence),
            _ => Err(format!(
                "invalid PROOFSTORM_TOOLSET {value:?}; expected all, experiment, design, runtime, or evidence"
            )),
        }
    }
}

impl ProofstormToolset {
    fn includes(self, tool: &str) -> bool {
        match self {
            Self::All => true,
            Self::Experiment => experiment_tool(tool),
            Self::Design => matches!(
                tool,
                "proofstorm_workspace_read"
            | "proofstorm_component_restart"
            | "proofstorm_component_exec_live"
            | "proofstorm_component_forensics"
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
                "proofstorm_lab_plan"
                    | "proofstorm_lab_apply"
                    | "proofstorm_lab_create"
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
                    | "proofstorm_operation_wait_many"
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

fn experiment_tool(tool: &str) -> bool {
    matches!(
        tool,
        // Stable generic one-session control plane. Catalog and scenario
        // growth must not require new MCP tools.
        "proofstorm_workspace_read"
            | "proofstorm_catalog_list"
            | "proofstorm_catalog_entry_read"
            | "proofstorm_lab_plan"
            | "proofstorm_lab_apply"
            | "proofstorm_lab_status"
            | "proofstorm_lab_component_status_list"
            | "proofstorm_lab_wait"
            | "proofstorm_lab_close"
            | "proofstorm_experiment_create"
            | "proofstorm_experiment_read"
            | "proofstorm_experiment_close"
            | "proofstorm_lease_acquire"
            | "proofstorm_lease_read"
            | "proofstorm_lease_release"
            | "proofstorm_node_restart"
            | "proofstorm_component_logs"
            | "proofstorm_liquidity_bootstrap"
            | "proofstorm_peer_connect"
            | "proofstorm_channel_open"
            | "proofstorm_channel_policy_set"
            | "proofstorm_network_partition"
            | "proofstorm_network_heal"
            | "proofstorm_wallet_initialize"
            | "proofstorm_wallet_balance"
            | "proofstorm_wallet_fund"
            | "proofstorm_wallet_invoice"
            | "proofstorm_wallet_pay"
            | "proofstorm_conservation_oracle"
            | "proofstorm_reachability_oracle"
            | "proofstorm_authentication_conformance"
            | "proofstorm_authentication_protected_spend"
            | "proofstorm_authentication_replay"
            | "proofstorm_operation_status"
            | "proofstorm_operation_wait_many"
            | "proofstorm_action_cancel"
            | "proofstorm_action_list"
            | "proofstorm_artifact_export"
            | "proofstorm_evidence_section_read"
    )
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
        // Whole-document replacement is retained in the store for non-agent
        // callers, but is intentionally absent from MCP. Stable-ID component
        // and link mutations are the safe agent editing contract.
        tool_router.disable_route("proofstorm_lab_edit");
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
            return Err(ErrorData::invalid_request(
                "at most 16 optional artifact bodies may be selected; do not enumerate the journal because every action and artifact descriptor is already included",
                Some(serde_json::json!({
                    "code": "evidence_artifact_limit",
                    "maximum": MAX_EXPLICIT_EVIDENCE_ARTIFACTS,
                    "requested": request.artifact_operation_ids.len(),
                    "journal_complete_without_explicit_ids": true,
                    "recovery": "leave artifact_operation_ids empty, or select at most 16 specific bodies",
                })),
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
        description = "List compact installed component identities, exact versions, config versions, and valid controls. Read exact entry details only for components you select; read a config schema only for constraints on a non-default field"
    )]
    fn proofstorm_catalog_list(
        &self,
        Parameters(request): Parameters<CatalogListRequest>,
    ) -> Result<Json<CatalogListResponse>, ErrorData> {
        self.authorize(Capability::CatalogRead)?;
        catalog_page(&request).map(Json)
    }

    #[tool(
        description = "Read exact authoring metadata for one selected component version: compatibility, immutable image, controls, authorable and required config fields, and safe defaults. Read its config schema only for constraints on a non-default field"
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

    #[tool(
        description = "Plan any supported lab topology from catalog implementation IDs and role connections. Read catalog_entry details for selected implementations and declare every intended runtime control in runtime_requirements; Proofstorm rejects unavailable driver controls before storing or materializing. It selects preferred versions, infers component kinds, configuration contracts, and typed dependency bindings. Adding implementations or controls does not add MCP tools. Next call lab_apply with the returned digest"
    )]
    fn proofstorm_lab_plan(
        &self,
        Parameters(request): Parameters<LabPlanRequest>,
    ) -> Result<Json<LabPlanReceipt>, ErrorData> {
        self.authorize(Capability::LabCreate)?;
        self.authorize(Capability::CatalogRead)?;
        let lab = compile_lab_plan(&request)?;
        let validation = lab_validation_result(&lab);
        if !validation.valid {
            return Err(ErrorData::invalid_request(
                format!(
                    "lab plan failed publication preflight; no plan was stored: {}",
                    validation_issue_summary(&validation.issues)
                ),
                Some(serde_json::json!({
                    "code": "lab_plan_invalid",
                    "validation": validation,
                })),
            ));
        }
        let plan_digest = digest_json(&lab);
        let components = resolved_plan_components(&lab);
        let runtime_endpoints = resolved_plan_runtime_endpoints(&lab)?;
        let connections = lab.links.clone();
        self.store
            .create_draft(
                &self.workspace,
                &self.principal,
                &request.plan_id,
                &lab,
                &request.idempotency_key,
            )
            .map_err(store_error)
            .and_then(|draft| {
                bounded_agent_response(LabPlanReceipt {
                    plan_id: draft.id,
                    plan_digest,
                    version: draft.version,
                    components,
                    runtime_endpoints,
                    connections,
                    validation,
                    next_tool: "proofstorm_lab_apply".into(),
                })
            })
            .map(Json)
    }

    #[tool(
        description = "Atomically publish and materialize the exact validated lab plan identified by plan_id and expected_plan_digest. A digest mismatch fails before publication. Replay the same request after interruption; both internal stages are idempotent. Then wait for ready"
    )]
    async fn proofstorm_lab_apply(
        &self,
        Parameters(request): Parameters<LabApplyRequest>,
    ) -> Result<Json<LabApplyReceipt>, ErrorData> {
        let draft = self
            .store
            .read_draft(&self.workspace, &self.principal, &request.plan_id)
            .map_err(store_error)?;
        let plan_digest = digest_json(&draft.lab);
        if plan_digest != request.expected_plan_digest {
            return Err(ErrorData::invalid_request(
                "stored lab plan does not match expected_plan_digest; nothing was applied",
                Some(serde_json::json!({
                    "code": "lab_plan_digest_mismatch",
                    "plan_id": request.plan_id,
                    "expected_plan_digest": request.expected_plan_digest,
                    "actual_plan_digest": plan_digest,
                    "recovery": "read or recreate the plan and apply the returned digest",
                })),
            ));
        }
        let revision = self
            .store
            .publish(
                &self.workspace,
                &self.principal,
                &request.plan_id,
                draft.version,
                &format!("{}:publish", request.idempotency_key),
            )
            .map_err(store_error)?;
        let component_count = u32::try_from(revision.lab.components.len()).unwrap_or(u32::MAX);
        let revision_digest = revision.digest.clone();
        let lock_digest = revision.lock.digest.clone();
        let status = self
            .proofstorm_lab_materialize(Parameters(MaterializeLabRequest {
                instance_id: request.instance_id.clone(),
                revision_digest: revision.digest,
                idempotency_key: format!("{}:materialize", request.idempotency_key),
            }))
            .await?
            .0;
        Ok(Json(LabApplyReceipt {
            plan_id: request.plan_id,
            plan_digest,
            revision_digest,
            lock_digest,
            instance_id: status.instance.id,
            phase: status.phase,
            component_count,
            next_tool: "proofstorm_lab_wait".into(),
        }))
    }

    #[tool(
        description = "Create a publication-ready versioned lab draft and return a compact receipt. Omit policy for safe defaults. Backend links use flat kind-specific fields, never a nested binding: chain_backend network; payment_backend method+unit. Invalid catalog configuration is rejected before any draft is written"
    )]
    fn proofstorm_lab_create(
        &self,
        Parameters(request): Parameters<CreateDraftRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::LabCreate)?;
        let lab = LabSpec::try_from(request.lab)
            .map_err(|message| coded_invalid_request("invalid_link_binding", message))?;
        let validation = lab_validation_result(&lab);
        if !validation.valid {
            return Err(ErrorData::invalid_request(
                "lab failed publication preflight; no draft was created",
                Some(serde_json::json!({
                    "code": "lab_validation_failed",
                    "validation": validation,
                    "next_tool": "proofstorm_lab_validate"
                })),
            ));
        }
        self.store
            .create_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                &lab,
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec!["/".into()])))
            .map_err(store_error)
    }

    #[tool(
        description = "Create a validated lab draft from a server-owned recipe; no catalog lookup, manual topology JSON, or separate validation call is needed. nutshell_lnd_cln_routing_fees creates bitcoin-core, lnd-backend, lnd-router, cln-backend, mint-lnd, mint-cln, payer-lnd, recipient-lnd, payer-cln, and recipient-cln with exact preferred versions and typed backend bindings. Initialize all four wallets, fund only payer-lnd and payer-cln, and create invoices only on the opposite recipient so both directions can run concurrently without cross-crediting baselines. Next call proofstorm_lab_publish"
    )]
    fn proofstorm_lab_recipe_create(
        &self,
        Parameters(request): Parameters<CreateLabRecipeRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::LabCreate)?;
        let name = request.name.unwrap_or_else(|| request.draft_id.clone());
        let lab = lab_from_recipe(request.recipe, name)?;
        let validation = lab_validation_result(&lab);
        if !validation.valid {
            return Err(ErrorData::internal_error(
                "built-in lab recipe failed publication preflight",
                Some(serde_json::json!({
                    "code": "lab_recipe_invalid",
                    "validation": validation,
                })),
            ));
        }
        self.store
            .create_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                &lab,
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
        let lab = LabSpec::try_from(request.lab)
            .map_err(|message| coded_invalid_request("invalid_link_binding", message))?;
        self.store
            .edit_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &lab,
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
        description = "Add a uniquely named typed link. Backend qualifiers are required flat fields selected by kind: chain_backend uses network; payment_backend uses method and unit; database_backend uses role; authentication_backend uses protocol. Never send a nested binding object"
    )]
    fn proofstorm_link_add(
        &self,
        Parameters(request): Parameters<MutateLinkRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let link =
            LinkSpec::try_from(request.link).map_err(|message| invalid_operation(&message))?;
        let changed_path = format!("/links/{}", link.id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::AddLink { link },
                &request.idempotency_key,
            )
            .map(|draft| Json(compact_draft_mutation(draft, vec![changed_path])))
            .map_err(store_error)
    }

    #[tool(description = "Remove one link from a lab draft by its stable link_id")]
    fn proofstorm_link_remove(
        &self,
        Parameters(request): Parameters<RemoveLinkRequest>,
    ) -> Result<Json<DraftMutationResult>, ErrorData> {
        self.authorize(Capability::TopologyMutate)?;
        let draft = self
            .store
            .read_draft(&self.workspace, &self.principal, &request.draft_id)
            .map_err(store_error)?;
        let link = draft
            .lab
            .links
            .into_iter()
            .find(|link| link.id == request.link_id)
            .ok_or_else(|| {
                invalid_operation(&format!("link {:?} does not exist", request.link_id))
            })?;
        let changed_path = format!("/links/{}", link.id);
        self.store
            .mutate_draft(
                &self.workspace,
                &self.principal,
                &request.draft_id,
                request.expected_version,
                &DraftMutation::RemoveLink { link },
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

    #[tool(
        description = "Validate structural, catalog, configuration, and publication contracts for a complete Proofstorm v1alpha1 lab. Omit policy for safe defaults. Backend links use flat kind-specific fields, never a nested binding: chain_backend network; payment_backend method+unit. Resolve every returned warning before creating the draft"
    )]
    fn proofstorm_lab_validate(
        &self,
        Parameters(request): Parameters<ValidateLabRequest>,
    ) -> Result<Json<LabValidationResult>, ErrorData> {
        self.authorize(Capability::LabValidate)?;
        let lab = LabSpec::try_from(request.lab)
            .map_err(|message| coded_invalid_request("invalid_link_binding", message))?;
        Ok(Json(lab_validation_result(&lab)))
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
        let mut last_status = None;
        loop {
            let status = match tokio::time::timeout_at(
                deadline,
                self.full_lab_status(&request.instance_id),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    return last_status.map_or_else(
                        || {
                            Err(coded_invalid_request(
                                "lab_wait_deadline_exceeded",
                                "the runtime status backend did not answer before the requested lab wait deadline",
                            ))
                        },
                        |status| {
                            Ok(Json(compact_lab_wait(
                                status,
                                request.target_phase,
                                false,
                                true,
                            )))
                        },
                    );
                }
            };
            let reached = status.phase == request.target_phase;
            if reached || lab_wait_terminal(status.phase) {
                return Ok(Json(compact_lab_wait(
                    status,
                    request.target_phase,
                    reached,
                    false,
                )));
            }
            last_status = Some(status.clone());
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

    #[tool(
        description = "Begin verified Kubernetes teardown and return a compact closing receipt. Then call lab_wait with target_phase=closed; success includes teardown_receipt.verified_absent=true"
    )]
    async fn proofstorm_lab_close(
        &self,
        Parameters(request): Parameters<InstanceRequest>,
    ) -> Result<Json<LabWaitResult>, ErrorData> {
        self.authorize(Capability::LabClose)?;
        let instance = self
            .store
            .instance_for_close(&self.workspace, &self.principal, &request.instance_id)
            .map_err(store_error)?;
        self.finalize_active_operations(&instance.id).await?;
        let status = self.runtime()?.close(instance).await?;
        let reached = status.phase == InstancePhase::Closed;
        Ok(Json(compact_lab_wait(
            status,
            InstancePhase::Closed,
            reached,
            false,
        )))
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
        description = "Restart any running lab component, whether its workload is a Deployment or StatefulSet, and wait for the exact accepted rollout to become ready. Use this for mints and wallets as well as Bitcoin and Lightning nodes"
    )]
    async fn proofstorm_component_restart(
        &self,
        Parameters(request): Parameters<NodeControlRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ComponentControl)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ComponentControl,
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
            OperationKind::ComponentRestart,
            &request,
            &request.idempotency_key,
            Capability::ComponentControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ComponentRestart(NodeControlAction {
                component: request.component,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Read a bounded tail of one lab component's own container log, journaled as an experiment artifact. This reads the selected running or failed component pod and keeps working while the component is unready, crash-looping, or stopped. The artifact also reports pod phase, container readiness, and restart count"
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

    #[tool(
        description = "Close an unleased experiment after its actions are terminal. Proofstorm first reconciles completed runtime actions into the journal; if any are still active, wait for the returned operation IDs. Finalization order: operation waits, lease_release, experiment_close, artifact_export"
    )]
    async fn proofstorm_experiment_close(
        &self,
        Parameters(request): Parameters<CloseExperimentRequest>,
    ) -> Result<Json<Experiment>, ErrorData> {
        self.authorize(Capability::ExperimentClose)?;
        let active = self
            .reconcile_experiment_operations(&request.experiment_id)
            .await?;
        if !active.is_empty() {
            return Err(coded_invalid_request(
                "experiment_actions_active",
                format!(
                    "wait for these operations before closing the experiment: {}",
                    active.join(", ")
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
        description = "Restart any running lab component, whether its workload is a Deployment or StatefulSet, and wait for the exact accepted rollout to become ready. Use this for mints and wallets as well as Bitcoin and Lightning nodes"
    )]
    async fn proofstorm_component_restart(
        &self,
        Parameters(request): Parameters<NodeControlRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ComponentControl)?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ComponentControl,
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
            OperationKind::ComponentRestart,
            &request,
            &request.idempotency_key,
            Capability::ComponentControl,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ComponentRestart(NodeControlAction {
                component: request.component,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Read a bounded tail of one lab component's own container log, journaled as an experiment artifact. This reads the selected running or failed component pod and keeps working while the component is unready, crash-looping, or stopped. The artifact also reports pod phase, container readiness, and restart count"
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

    #[tool(
        description = "Release an experiment lease owned by the current principal. At finalization, first wait for submitted operations, then call lease_release, experiment_close, and artifact_export in that order"
    )]
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
        description = "Run bounded offline forensics in a disposable pod built from a component's pinned image and declared data mounts. This is not the running component and does not promise its localhost, Unix sockets, process identity, or live CLI connectivity. Use it for source and database inspection; use component_exec_live for a running component's native CLI"
        self.submit_node_control(request, OperationKind::NodeStart)
    async fn proofstorm_component_forensics(
    }

    #[tool(description = "Stop a logical Bitcoin or Lightning node without deleting its state")]
        self.authorize(Capability::ComponentForensics)?;
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
                Capability::ComponentForensics,
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
            OperationKind::ComponentForensics,
        component_image_any(&revision, &request.component, component.kind)?;
        let operation = self.create_operation(
            Capability::ComponentForensics,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ComponentLogs,
            &request,
            &request.idempotency_key,
            Capability::ComponentLogs,
        )?;
            LabAction::ComponentForensics(ComponentForensicsAction {
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
    #[tool(
        description = "Execute a bounded non-interactive shell program inside the selected running component container. This shares the component's real network namespace, user, files, localhost APIs, and Unix sockets. Native CLIs still require their own flags; read the selected catalog entry's runtime endpoint limitations for exact invocation hints. Prefer typed actions for routine mutations; this powerful escape hatch is fully journaled"
    )]
    async fn proofstorm_component_exec_live(
        &self,
        Parameters(request): Parameters<ComponentExecLiveRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ComponentExecLive)?;
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
                Capability::ComponentExecLive,
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
            OperationKind::ComponentExecLive,
            &request,
            &request.idempotency_key,
            Capability::ComponentExecLive,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ComponentExecLive(ComponentExecLiveAction {
                component: request.component,
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
        description = "Run bounded offline forensics in a disposable pod built from a component's pinned image and declared data mounts. This is not the running component and does not promise its localhost, Unix sockets, process identity, or live CLI connectivity. Use it for source and database inspection; use component_exec_live for a running component's native CLI"
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
    async fn proofstorm_component_forensics(
            .map_err(store_error)
    }

        self.authorize(Capability::ComponentForensics)?;
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
                Capability::ComponentForensics,
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
            OperationKind::ComponentForensics,
        let operation = self.create_operation(
            &request.instance_id,
            Capability::ComponentForensics,
            &request.lease_id,
            &request.operation_id,
            OperationKind::AuthenticationReplay,
            &request,
            &request.idempotency_key,
            Capability::AuthenticationTest,
        )?;
        if operation.phase != OperationPhase::Pending {
            LabAction::ComponentForensics(ComponentForensicsAction {
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
    #[tool(
        description = "Execute a bounded non-interactive shell program inside the selected running component container. This shares the component's real network namespace, user, files, localhost APIs, and Unix sockets. Native CLIs still require their own flags; read the selected catalog entry's runtime endpoint limitations for exact invocation hints. Prefer typed actions for routine mutations; this powerful escape hatch is fully journaled"
    )]
    async fn proofstorm_component_exec_live(
        &self,
        Parameters(request): Parameters<ComponentExecLiveRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ComponentExecLive)?;
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
                Capability::ComponentExecLive,
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
            OperationKind::ComponentExecLive,
            &request,
            &request.idempotency_key,
            Capability::ComponentExecLive,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(Json(operation));
        }
        let action = runtime_action_resource(
            &self.runtime()?.control_namespace,
            &instance,
            &operation,
            LabAction::ComponentExecLive(ComponentExecLiveAction {
                component: request.component,
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

        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

    #[tool(
        description = "Last-resort shell execution in a fresh pod using a component's pinned image, volumes, and CLI. Prefer typed actions and component_logs. This is not the running component: localhost is wrong; use supplied PROOFSTORM_TARGET_* or native endpoint variables. The bounded artifact reports output, exit code, and target endpoint readiness"
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

    #[tool(
        description = "First runtime action for a lab made by proofstorm_lab_recipe_create. Proofstorm supplies the recipe's exact Bitcoin/LND component IDs and safe funding/channel amounts. Await success, then call proofstorm_lab_recipe_route_channel_open; do not call generic liquidity or channel tools for this recipe"
    )]
    async fn proofstorm_lab_recipe_bootstrap(
        &self,
        Parameters(request): Parameters<LabRecipeSetupRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.proofstorm_liquidity_bootstrap(Parameters(recipe_bootstrap_request(request)))
            .await
    }

    #[tool(
        description = "Second runtime action for a lab made by proofstorm_lab_recipe_create. After recipe bootstrap succeeds, Proofstorm opens the remaining router-to-CLN channel with server-owned IDs, safe capacity, and balanced directional liquidity. Await success, then call proofstorm_lab_recipe_fee_matrix_run once"
    )]
    async fn proofstorm_lab_recipe_route_channel_open(
        &self,
        Parameters(request): Parameters<LabRecipeSetupRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.proofstorm_channel_open(Parameters(recipe_route_channel_request(request)))
            .await
    }

    #[tool(
        description = "Run the complete auditable payment matrix for a ready nutshell_lnd_cln_routing_fees recipe after both recipe setup operations succeed. Proofstorm initializes the four role wallets, funds only the two payers, applies known below- and above-reserve routing policies, pays in both directions, and runs four exact conservation oracles. All 26 child actions remain individually journaled. The call returns a compact scientific summary; replay the same matrix_id after interruption"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "the recipe matrix keeps one auditable, dependency-ordered experiment together"
    )]
    async fn proofstorm_lab_recipe_fee_matrix_run(
        &self,
        Parameters(request): Parameters<LabRecipeFeeMatrixRequest>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        match request.recipe {
            LabRecipe::NutshellLndClnRoutingFees => {}
        }
        let operation_prefix = recipe_fee_matrix_operation_prefix(&request);
        let operation_id = |suffix: &str| format!("{operation_prefix}-{suffix}");
        let idempotency_key = |suffix: &str| format!("{}:{suffix}", request.idempotency_key);
        let common_wait = |operation_ids: Vec<String>| OperationWaitManyRequest {
            operation_ids,
            timeout_seconds: 120,
        };

        for (suffix, wallet, mint) in [
            ("init-payer-lnd", "payer-lnd", "mint-lnd"),
            ("init-recipient-lnd", "recipient-lnd", "mint-lnd"),
            ("init-payer-cln", "payer-cln", "mint-cln"),
            ("init-recipient-cln", "recipient-cln", "mint-cln"),
        ] {
            self.proofstorm_wallet_initialize(Parameters(WalletInitializeRequest {
                instance_id: request.instance_id.clone(),
                experiment_id: request.experiment_id.clone(),
                lease_id: request.lease_id.clone(),
                operation_id: operation_id(suffix),
                wallet: wallet.into(),
                mint: mint.into(),
                idempotency_key: idempotency_key(suffix),
            }))
            .await?;
        }
        let initialized = self
            .proofstorm_operation_wait_many(Parameters(common_wait(vec![
                operation_id("init-payer-lnd"),
                operation_id("init-recipient-lnd"),
                operation_id("init-payer-cln"),
                operation_id("init-recipient-cln"),
            ])))
            .await?
            .0;
        require_matrix_stage(&initialized, "wallet initialization", 4)?;

        for (suffix, wallet, mint) in [
            ("fund-payer-lnd", "payer-lnd", "mint-lnd"),
            ("fund-payer-cln", "payer-cln", "mint-cln"),
        ] {
            self.proofstorm_wallet_fund(Parameters(WalletFundRequest {
                instance_id: request.instance_id.clone(),
                experiment_id: request.experiment_id.clone(),
                lease_id: request.lease_id.clone(),
                operation_id: operation_id(suffix),
                wallet: wallet.into(),
                mint: mint.into(),
                payer_lightning: "lnd-router".into(),
                amount_sat: ROUTING_FEE_RECIPE_WALLET_FUNDING_SAT,
                idempotency_key: idempotency_key(suffix),
            }))
            .await?;
        }
        let funded = self
            .proofstorm_operation_wait_many(Parameters(common_wait(vec![
                operation_id("fund-payer-lnd"),
                operation_id("fund-payer-cln"),
            ])))
            .await?
            .0;
        require_matrix_stage(&funded, "payer funding", 2)?;

        let mut cases = Vec::with_capacity(4);
        for (treatment, treatment_id, base_fee_sat, fee_rate_ppm) in [
            (
                "below_reserve",
                "below-reserve",
                ROUTING_FEE_RECIPE_LOW_BASE_FEE_SAT,
                ROUTING_FEE_RECIPE_LOW_FEE_RATE_PPM,
            ),
            (
                "above_reserve",
                "above-reserve",
                ROUTING_FEE_RECIPE_HIGH_BASE_FEE_SAT,
                ROUTING_FEE_RECIPE_HIGH_FEE_RATE_PPM,
            ),
        ] {
            for (endpoint, to_lightning) in [("cln", "cln-backend"), ("lnd", "lnd-backend")] {
                let suffix = format!("policy-{treatment_id}-{endpoint}");
                self.proofstorm_channel_policy_set(Parameters(ChannelPolicySetRequest {
                    instance_id: request.instance_id.clone(),
                    experiment_id: request.experiment_id.clone(),
                    lease_id: request.lease_id.clone(),
                    operation_id: operation_id(&suffix),
                    from_lightning: "lnd-router".into(),
                    to_lightning: to_lightning.into(),
                    base_fee_sat,
                    fee_rate_ppm,
                    idempotency_key: idempotency_key(&suffix),
                }))
                .await?;
            }
            let policy = self
                .proofstorm_operation_wait_many(Parameters(common_wait(vec![
                    operation_id(&format!("policy-{treatment_id}-cln")),
                    operation_id(&format!("policy-{treatment_id}-lnd")),
                ])))
                .await?
                .0;
            require_matrix_stage(&policy, &format!("{treatment} routing policy"), 2)?;

            for (suffix, wallet, mint) in [
                (
                    format!("baseline-{treatment_id}-lnd"),
                    "payer-lnd",
                    "mint-lnd",
                ),
                (
                    format!("baseline-{treatment_id}-cln"),
                    "payer-cln",
                    "mint-cln",
                ),
            ] {
                self.proofstorm_wallet_balance(Parameters(WalletBalanceRequest {
                    instance_id: request.instance_id.clone(),
                    experiment_id: request.experiment_id.clone(),
                    lease_id: request.lease_id.clone(),
                    operation_id: operation_id(&suffix),
                    wallet: wallet.into(),
                    mint: mint.into(),
                    idempotency_key: idempotency_key(&suffix),
                }))
                .await?;
            }
            for (suffix, wallet, mint) in [
                (
                    format!("invoice-{treatment_id}-recipient-cln"),
                    "recipient-cln",
                    "mint-cln",
                ),
                (
                    format!("invoice-{treatment_id}-recipient-lnd"),
                    "recipient-lnd",
                    "mint-lnd",
                ),
            ] {
                self.proofstorm_wallet_invoice(Parameters(WalletInvoiceRequest {
                    instance_id: request.instance_id.clone(),
                    experiment_id: request.experiment_id.clone(),
                    lease_id: request.lease_id.clone(),
                    operation_id: operation_id(&suffix),
                    wallet: wallet.into(),
                    mint: mint.into(),
                    amount_sat: ROUTING_FEE_RECIPE_PAYMENT_SAT,
                    timeout_seconds: default_quote_timeout_seconds(),
                    idempotency_key: idempotency_key(&suffix),
                }))
                .await?;
            }
            let observations = self
                .proofstorm_operation_wait_many(Parameters(common_wait(vec![
                    operation_id(&format!("baseline-{treatment_id}-lnd")),
                    operation_id(&format!("baseline-{treatment_id}-cln")),
                    operation_id(&format!("invoice-{treatment_id}-recipient-cln")),
                    operation_id(&format!("invoice-{treatment_id}-recipient-lnd")),
                ])))
                .await?
                .0;
            require_matrix_stage(
                &observations,
                &format!("{treatment} baseline and invoices"),
                4,
            )?;
            let cln_quote = matrix_invoice_quote_id(&observations.operations[2])?;
            let lnd_quote = matrix_invoice_quote_id(&observations.operations[3])?;

            let quote_ids = [cln_quote, lnd_quote];
            for (direction, quote_id) in ROUTING_FEE_RECIPE_PAYMENT_DIRECTIONS
        description = "Set the outgoing routing policy from one Lightning node to its peer. base_fee_sat is in satoshis (like all other agent-facing amounts); fee_rate_ppm is parts per million. Proofstorm converts the base fee to native millisatoshis and resolves the channel/adapter; prefer this typed operation over a native CLI"
                .zip(quote_ids)
            {
                let suffix = format!("pay-{treatment_id}-{}", direction.id);
                self.proofstorm_wallet_pay(Parameters(WalletPayRequest {
                    instance_id: request.instance_id.clone(),
                    experiment_id: request.experiment_id.clone(),
                    lease_id: request.lease_id.clone(),
                    operation_id: operation_id(&suffix),
                    wallet: direction.payer_wallet.into(),
                    mint: direction.payer_mint.into(),
                    recipient_wallet: direction.recipient_wallet.into(),
                    recipient_mint: direction.recipient_mint.into(),
                    mint_quote_id: quote_id,
                    idempotency_key: idempotency_key(&suffix),
                }))
                .await?;
            }
            let payments = self
                .proofstorm_operation_wait_many(Parameters(common_wait(vec![
                    operation_id(&format!("pay-{treatment_id}-lnd-to-cln")),
                    operation_id(&format!("pay-{treatment_id}-cln-to-lnd")),
                ])))
                .await?
                .0;
            require_matrix_stage(&payments, &format!("{treatment} payments"), 2)?;

            let mut oracle_operations = Vec::with_capacity(2);
            for direction in ROUTING_FEE_RECIPE_PAYMENT_DIRECTIONS {
                let suffix = format!("oracle-{treatment_id}-{}", direction.oracle_endpoint);
                let oracle = self
                    .proofstorm_conservation_oracle(Parameters(ConservationOracleRequest {
                        instance_id: request.instance_id.clone(),
                        experiment_id: request.experiment_id.clone(),
                        lease_id: request.lease_id.clone(),
                        operation_id: operation_id(&suffix),
                        wallet: direction.payer_wallet.into(),
                        mint: direction.payer_mint.into(),
                        baseline_operation_id: operation_id(&format!(
                            "baseline-{treatment_id}-{}",
                            direction.oracle_endpoint
                        )),
                        treatment_operation_id: operation_id(&format!(
                            "pay-{treatment_id}-{}",
                            direction.id
                        )),
                        idempotency_key: idempotency_key(&suffix),
                    }))
                    .await?
                    .0;
                oracle_operations.push(oracle);
            }
            for (index, direction) in ROUTING_FEE_RECIPE_PAYMENT_DIRECTIONS
                .into_iter()
                .enumerate()
            {
                cases.push(matrix_case_summary(
                    treatment,
                    direction.label,
                    base_fee_sat,
                    fee_rate_ppm,
                    &payments.operations[index],
                    &oracle_operations[index],
                )?);
            }
        }

        Ok(Json(serde_json::json!({
            "recipe": request.recipe,
            "matrix_id": request.matrix_id,
            "all_terminal": true,
            "journaled_child_actions": 26,
            "wallet_funding_sat_each": ROUTING_FEE_RECIPE_WALLET_FUNDING_SAT,
            "payment_amount_sat": ROUTING_FEE_RECIPE_PAYMENT_SAT,
            "cases": cases,
            "guidance": "Matrix complete. Release the lease, close the experiment, export evidence, then close and await the lab.",
        })))
    }

    #[tool(
        description = "Required first action after a custom regtest Lightning lab is ready: mine 101 blocks, fund two LND nodes, and open their channel. Await success before peer/channel actions. Labs created from a recipe should use proofstorm_lab_recipe_bootstrap instead"
    )]
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

    #[tool(
        description = "Connect Lightning peers. Requires a succeeded liquidity_bootstrap in this experiment; premature calls are rejected without creating an operation"
    )]
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
        self.require_liquidity_bootstrap(&request.experiment_id, &request.instance_id)?;
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
        description = "Set the outgoing routing policy from one Lightning node to its peer. base_fee_sat is in satoshis (like all other agent-facing amounts); fee_rate_ppm is parts per million. Proofstorm converts the base fee to native millisatoshis and resolves the channel/adapter; prefer this typed operation over a native CLI"
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

    #[tool(
        description = "Connect two Lightning endpoints in a custom lab, then open and confirm a channel. Requires a succeeded liquidity_bootstrap. Proofstorm rejects unproven funding sources and channel amounts above the bootstrapped node's safe remaining on-chain budget. Labs created from a recipe should use proofstorm_lab_recipe_route_channel_open instead"
    )]
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
        let bootstrap =
            self.require_liquidity_bootstrap(&request.experiment_id, &request.instance_id)?;
        validate_channel_funding_admission(&request, &bootstrap)?;
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

    #[tool(
        description = "Set the outgoing routing policy from one Lightning node to its peer. base_fee_sat is in satoshis (like all other agent-facing amounts); fee_rate_ppm is parts per million. Proofstorm converts the base fee to native millisatoshis and resolves the channel/adapter; do not use component_exec for fee policy"
    )]
    async fn proofstorm_channel_policy_set(
        &self,
        Parameters(request): Parameters<ChannelPolicySetRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::ChannelOpen)?;
        validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
        if request.base_fee_sat > 100_000 || request.fee_rate_ppm > 1_000_000 {
            return Err(coded_invalid_request(
                "invalid_channel_policy",
                "base_fee_sat must be <= 100000 and fee_rate_ppm must be <= 1000000",
            ));
        }
        let base_fee_msat = request.base_fee_sat.checked_mul(1_000).ok_or_else(|| {
            coded_invalid_request(
                "invalid_channel_policy",
                "base_fee_sat cannot be represented in native millisatoshis",
            )
        })?;
        let (instance, revision) = self
            .store
            .operation_context(
                &self.workspace,
                &self.principal,
                &request.instance_id,
                Capability::ChannelOpen,
            )
            .map_err(store_error)?;
        component_image_any(&revision, &request.from_lightning, ComponentKind::Lightning)?;
        component_image_any(&revision, &request.to_lightning, ComponentKind::Lightning)?;
        self.require_liquidity_bootstrap(&request.experiment_id, &request.instance_id)?;
        let operation = self.create_operation(
            &request.instance_id,
            &request.experiment_id,
            &request.lease_id,
            &request.operation_id,
            OperationKind::ChannelPolicySet,
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
            LabAction::ChannelPolicySet(ChannelPolicySetAction {
                from_lightning: request.from_lightning.clone(),
                to_lightning: request.to_lightning.clone(),
                base_fee_msat,
                fee_rate_ppm: request.fee_rate_ppm,
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
        if !(1..=30).contains(&request.timeout_seconds) {
            OperationKind::NetworkHeal,
                "timeout_seconds must be between 1 and 30; no operation was created",
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
    #[tool(
        description = "Refresh one exact payer-side melt quote through the wallet adapter. This performs the wallet's native mint round-trip and, when the mint reports UNPAID, releases proofs reserved by that melt. The artifact reports before/after quote state, reserved proof count, and available balance"
    )]
    async fn proofstorm_wallet_melt_quote_refresh(
        &self,
        Parameters(request): Parameters<WalletMeltQuoteRefreshRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize(Capability::WalletControl)?;
        validate_quote_id(&request.melt_quote_id)?;
        if !(1..=30).contains(&request.timeout_seconds) {
            return Err(invalid_operation(
                "timeout_seconds must be between 1 and 30; no operation was created",
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
            OperationKind::WalletMeltQuoteRefresh,
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
            LabAction::WalletMeltQuoteRefresh(WalletMeltQuoteRefreshAction {
                wallet: request.wallet,
                mint: request.mint,
                melt_quote_id: request.melt_quote_id,
                timeout_seconds: request.timeout_seconds,
            }),
        );
        self.runtime()?.apply_action(&instance, &action).await?;
        self.store
            .update_operation_phase(&self.workspace, &operation.id, OperationPhase::Running)
            .map(Json)
            .map_err(store_error)
    }

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
        description = "Fund a logical wallet with a bounded quote paid by a named LND node. The payer must be distinct from the mint's own payment backend; CLN and self-payment choices are rejected before an operation is created"
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
        require_component_runtime_control(&revision, &request.mint, "component", "wallet_fund")?;
        component_image(
            &revision,
            &request.payer_lightning,
            ComponentKind::Lightning,
            "lnd",
        )?;
        validate_wallet_fund_payer(&revision.lab, &request.mint, &request.payer_lightning)?;
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

    #[tool(
        description = "Verify one wallet_pay debit exactly from immutable artifacts. Capture wallet_balance immediately before one wallet_pay with no intervening wallet mutation, then pass both operation IDs. Proofstorm derives the expected post-payment balance from authoritative melt amount/state/Lightning fee plus the exact NUT-02 input fee derived from the spent proofs and keysets. There is no caller-controlled tolerance. Negative findings return conserved=false evidence, not execution failures. Round trips are invalid because they mint external value first"
    )]
    async fn proofstorm_conservation_oracle(
        &self,
        Parameters(request): Parameters<ConservationOracleRequest>,
    ) -> Result<Json<LabOperation>, ErrorData> {
        self.authorize_all(&[Capability::OracleRun, Capability::ArtifactRead])?;
        let (_instance, revision) = self
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
        let baseline = self
            .store
            .operation(
                &self.workspace,
                &self.principal,
                &request.baseline_operation_id,
            )
            .map_err(store_error)?;
        let treatment = self
            .store
            .operation(
                &self.workspace,
                &self.principal,
                &request.treatment_operation_id,
            )
            .map_err(store_error)?;
        let evidence = conservation_observation(
            &request,
            &baseline,
            &treatment,
            &self.workspace,
            &self.principal,
        )?;
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
        self.store
            .record_operation_result(
                &self.workspace,
                &operation.id,
                OperationPhase::Succeeded,
                evidence,
            )
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
        let completed = self.record_runtime_terminal_result(&operation, phase, artifact)?;
        Ok(Json(completed))
    }

    /// Validate adapter output before committing it to the canonical journal.
    /// Invalid terminal output is itself a terminal operation failure: it must
    /// never leave a completed runtime job occupying an active-operation slot.
    fn record_runtime_terminal_result(
        &self,
        operation: &LabOperation,
        phase: OperationPhase,
        artifact: serde_json::Value,
    ) -> Result<LabOperation, ErrorData> {
        let Ok(observations) = wallet_quote_observations_from_artifact(&artifact) else {
            return self
                .store
                .record_operation_result(
                    &self.workspace,
                    &operation.id,
                    OperationPhase::Failed,
                    invalid_terminal_artifact(
                        operation,
                        phase,
                        "invalid_wallet_quote_observation",
                        "the runtime produced a wallet quote observation outside the strict contract",
                    ),
                )
                .map_err(store_error);
        };
        if validate_operation_quote_observations(operation, &observations).is_err() {
            return self
                .store
                .record_operation_result(
                    &self.workspace,
                    &operation.id,
                    OperationPhase::Failed,
                    invalid_terminal_artifact(
                        operation,
                        phase,
                        "wallet_quote_observation_identity_mismatch",
                        "the runtime wallet quote observations do not match the admitted typed operation",
                    ),
                )
                .map_err(store_error);
        }
        self.store
            .record_operation_result_with_quote_observations(
                &self.workspace,
                &operation.id,
                phase,
                artifact,
                &observations,
            )
            .map_err(store_error)
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
        let mut last_operation = None;
        loop {
            let operation = match tokio::time::timeout_at(
                deadline,
                self.proofstorm_operation_status(Parameters(OperationRequest {
                    operation_id: request.operation_id.clone(),
                })),
            )
            .await
            {
                Ok(result) => result?.0,
                Err(_) => {
                    return last_operation.map_or_else(
                        || {
                            Err(coded_invalid_request(
                                "operation_wait_deadline_exceeded",
                                "the runtime action backend did not answer before the requested operation wait deadline",
                            ))
                        },
                        |operation| Ok(Json(compact_operation_wait(operation, true))),
                    );
                }
            };
            if operation_terminal(operation.phase) {
                return Ok(Json(compact_operation_wait(operation, false)));
            }
            last_operation = Some(operation.clone());
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Json(compact_operation_wait(operation, true)));
            }
            tokio::time::sleep(backoff.min(deadline - now)).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(2));
        }
    }

    #[tool(
        description = "Preferred after starting up to 8 independent operations (the per-instance active-operation limit): wait for all of them together with bounded parallel polling. Returns compact terminal artifacts in request order; timeout_seconds must be 1..=120"
    )]
    async fn proofstorm_operation_wait_many(
        &self,
        Parameters(request): Parameters<OperationWaitManyRequest>,
    ) -> Result<Json<OperationWaitManyResult>, ErrorData> {
        validate_operation_wait_many_request(&request)?;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(u64::from(request.timeout_seconds));
        let mut backoff = std::time::Duration::from_millis(250);
        let mut last_operations = None;
        loop {
            let statuses = request.operation_ids.iter().map(|operation_id| {
                self.proofstorm_operation_status(Parameters(OperationRequest {
                    operation_id: operation_id.clone(),
                }))
            });
            let operations = match tokio::time::timeout_at(
                deadline,
                futures::future::join_all(statuses),
            )
            .await
            {
                Ok(results) => results
                    .into_iter()
                    .map(|result| result.map(|operation| operation.0))
                    .collect::<Result<Vec<_>, _>>()?,
                Err(_) => {
                    return last_operations.map_or_else(
                        || {
                            Err(coded_invalid_request(
                                "operation_wait_many_deadline_exceeded",
                                "the runtime action backend did not answer before the requested batch wait deadline",
                            ))
                        },
                        |operations| compact_operation_wait_many(operations, true).map(Json),
                    );
                }
            };
            if operations
                .iter()
                .all(|operation| operation_terminal(operation.phase))
            {
                return compact_operation_wait_many(operations, false).map(Json);
            }
            last_operations = Some(operations.clone());
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return compact_operation_wait_many(operations, true).map(Json);
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
            (OperationKind::WalletMeltQuoteRefresh, WalletQuoteObservationRole::PaymentMelt) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
                    && field("melt_quote_id") == Some(&observation.quote_id)
            }
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
        description = "Export complete deterministic evidence after lease_release and experiment_close. The journal always includes every action plus artifact descriptors; leave artifact_operation_ids empty unless up to 16 specific full bodies are needed. A smaller artifact_count does not mean incomplete evidence. Bulk content stays outside model context at resource_uri"
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
            runtime_endpoints: entry.runtime_endpoints.clone(),
            image: entry.image.clone(),
            source_digest: entry.source_digest.clone(),
            allowed_control: entry.allowed_control.clone(),
            recommended_control: recommended_control(entry),
            authorable_config_fields: authorable_config_fields(entry),
            required_config_fields: required_config_fields(entry),
            config_defaults: config_defaults(entry),
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
    let validation = validate_lab(&draft.lab);
    let summary = topology_summary(&draft.lab);
    let mut warnings = summary.warnings;
    warnings.extend(
        validation
            .issues
            .iter()
            .take(8)
            .map(|issue| format!("{}@{}: {}", issue.code, issue.path, issue.message)),
    );
    DraftMutationResult {
        draft_id: draft.id,
        version: draft.version,
        component_count: summary.component_count,
        link_count: summary.link_count,
        structure: format!(
            "components=[{}]; links=[{}]; backend_bindings={}/{}",
            summary.component_ids.join(","),
            summary.link_ids.join(","),
            summary.bound_backend_link_count,
            summary.backend_link_count
        ),
        topology_digest: summary.topology_digest,
        valid: validation.valid,
        warnings,
        changed_paths,
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

fn topology_summary(lab: &LabSpec) -> TopologySummary {
    let mut components = lab.components.clone();
    components.sort_by(|left, right| left.id.cmp(&right.id));
    let mut links = lab.links.clone();
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
        warnings.push("empty_topology: the draft contains no components".into());
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
            direct_backend_peers.join(",")
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
            "distinct_payment_wallets_required: {mint_count} mints but only {wallet_count} wallet component(s); bidirectional cross-mint wallet_pay requires distinct payer and recipient wallet components"
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

fn lab_validation_result(lab: &LabSpec) -> LabValidationResult {
    let mut validation = validate_lab(lab);
    if validation.valid {
        if let Err(message) = proofstorm_core::resolve_lock(lab, default_catalog()) {
            validation.valid = false;
            validation.issues.push(ValidationIssue {
                code: "publication_preflight_failed".into(),
                path: "/".into(),
                message,
            });
        }
    }
    let summary = topology_summary(lab);
    LabValidationResult {
        valid: validation.valid,
        issues: validation.issues,
        component_ids: summary.component_ids,
        link_ids: summary.link_ids,
        warnings: summary.warnings,
    }
}

fn validation_issue_summary(issues: &[ValidationIssue]) -> String {
    let mut summary = issues
        .iter()
        .take(3)
        .map(|issue| format!("{} at {}: {}", issue.code, issue.path, issue.message))
        .collect::<Vec<_>>()
        .join("; ");
    if issues.len() > 3 {
        summary.push_str("; and ");
        summary.push_str(&(issues.len() - 3).to_string());
        summary.push_str(" more issue(s)");
    }
    summary
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
    // Pagination is a resource safeguard, not user intent. Saturate harmless
    // model guesses instead of spending an agent turn on a recoverable error.
    let limit = request.limit.clamp(1, MAX_CATALOG_LIST_LIMIT);
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
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .map(|entry| CatalogEntrySummary {
            id: entry.id.clone(),
            kind: entry.kind,
            version: entry.version.clone(),
            preferred: preferred.contains(&(&entry.id, &entry.version)),
            adapter_version: entry.adapter_version.clone(),
            protocol_action_adapter_version: entry.protocol_action_adapter_version.clone(),
            config_version: entry.config_version.clone(),
            config_schema_digest: entry.config_schema_digest.clone(),
            allowed_control: entry.allowed_control.clone(),
            recommended_control: recommended_control(entry),
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

fn recommended_control(entry: &CatalogEntry) -> ControlClass {
    [
        ControlClass::Target,
        ControlClass::Laboratory,
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

fn compile_lab_plan(request: &LabPlanRequest) -> Result<LabSpec, ErrorData> {
    let catalog = default_catalog();
    let (components, selected_entries) = resolve_plan_components(&request.components, catalog)?;
    validate_plan_runtime_requirements(&request.runtime_requirements, &selected_entries)?;
    let links = request
        .connections
        .iter()
        .map(|connection| compile_plan_connection(connection, &selected_entries))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(LabSpec {
        api_version: API_VERSION.into(),
        name: request.plan_id.clone(),
        components,
        links,
        policy: request.policy.clone(),
    })
}

fn validate_plan_runtime_requirements(
    requirements: &[LabPlanRuntimeRequirement],
    selected_entries: &BTreeMap<String, &'static CatalogEntry>,
) -> Result<(), ErrorData> {
    for requirement in requirements {
        if requirement.controls.is_empty() {
            return Err(ErrorData::invalid_request(
                format!(
                    "runtime requirement for component {:?} endpoint {:?} has no controls; no plan was stored",
                    requirement.component, requirement.endpoint
                ),
                Some(serde_json::json!({
                    "code": "lab_plan_runtime_controls_empty",
                    "component_id": requirement.component,
                    "endpoint": requirement.endpoint,
                    "recovery": "remove the empty requirement or list the runtime controls the experiment will execute",
                })),
            ));
        }
        let entry = plan_endpoint(selected_entries, &requirement.component)?;
        let endpoint = entry
            .runtime_endpoints
            .iter()
            .find(|endpoint| endpoint.id == requirement.endpoint)
            .ok_or_else(|| {
                ErrorData::invalid_request(
                    format!(
                        "component {:?} has no runtime endpoint {:?}; no plan was stored",
                        requirement.component, requirement.endpoint
                    ),
                    Some(serde_json::json!({
                        "code": "lab_plan_runtime_endpoint_not_found",
                        "component_id": requirement.component,
                        "implementation": entry.id,
                        "requested_endpoint": requirement.endpoint,
                        "available_endpoints": entry.runtime_endpoints,
                        "recovery": "read the selected catalog entry and choose one advertised runtime endpoint",
                    })),
                )
            })?;
        let unavailable = requirement
            .controls
            .difference(&endpoint.controls)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !unavailable.is_empty() {
            return Err(ErrorData::invalid_request(
                format!(
                    "component {:?} endpoint {:?} cannot execute required control(s) {}; no plan was stored. {}",
                    requirement.component,
                    requirement.endpoint,
                    unavailable.iter().cloned().collect::<Vec<_>>().join(", "),
                    endpoint.limitations.join("; ")
                ),
                Some(serde_json::json!({
                    "code": "lab_plan_runtime_control_unsupported",
                    "component_id": requirement.component,
                    "implementation": entry.id,
                    "endpoint": endpoint,
                    "unavailable_controls": unavailable,
                    "recovery": "choose an implementation endpoint that advertises every required control, or limit the experiment to supported observations",
                })),
            ));
        }
    }
    Ok(())
}

fn resolve_plan_components(
    inputs: &[LabPlanComponentInput],
    catalog: &'static CatalogResponse,
) -> Result<(Vec<ComponentSpec>, BTreeMap<String, &'static CatalogEntry>), ErrorData> {
    let mut components = Vec::with_capacity(inputs.len());
    let mut selected_entries = BTreeMap::new();
    for input in inputs {
        let version = match input.version.as_deref() {
            Some(version) => version,
            None => catalog
                .implementations
                .iter()
                .find(|support| support.implementation == input.implementation)
                .map(|support| support.preferred_version.as_str())
                .ok_or_else(|| {
                    ErrorData::invalid_request(
                        format!(
                            "catalog implementation {:?} is not installed",
                            input.implementation
                        ),
                        Some(serde_json::json!({
                            "code": "lab_plan_implementation_not_found",
                            "component_id": input.id,
                            "requested_implementation": input.implementation,
                            "available_implementations": catalog.implementations.iter()
                                .map(|support| support.implementation.as_str())
                                .collect::<Vec<_>>(),
                        })),
                    )
                })?,
        };
        let entry = exact_catalog_entry(&catalog.entries, &input.implementation, version)?;
        let control = input
            .control
            .unwrap_or_else(|| default_plan_control(entry.kind, &entry.allowed_control));
        if !entry.allowed_control.contains(&control) {
            return Err(ErrorData::invalid_request(
                format!(
                    "component {:?} cannot use control class {control:?}",
                    input.id
                ),
                Some(serde_json::json!({
                    "code": "lab_plan_control_unsupported",
                    "component_id": input.id,
                    "implementation": entry.id,
                    "allowed_control": entry.allowed_control,
                })),
            ));
        }
        components.push(ComponentSpec {
            id: input.id.clone(),
            kind: entry.kind,
            implementation: entry.id.clone(),
            version: Some(entry.version.clone()),
            config_version: entry.config_version.clone(),
            control,
            config: input.config.clone(),
        });
        selected_entries.insert(input.id.clone(), entry);
    }
    Ok((components, selected_entries))
}

fn plan_endpoint(
    selected_entries: &BTreeMap<String, &'static CatalogEntry>,
    id: &str,
) -> Result<&'static CatalogEntry, ErrorData> {
    selected_entries.get(id).copied().ok_or_else(|| {
        ErrorData::invalid_request(
            format!("connection references unknown component {id:?}"),
            Some(serde_json::json!({
                "code": "lab_plan_connection_endpoint_not_found",
                "requested_component_id": id,
                "available_component_ids": selected_entries.keys().collect::<Vec<_>>(),
            })),
        )
    })
}

fn compile_plan_connection(
    connection: &LabPlanConnectionInput,
    selected_entries: &BTreeMap<String, &'static CatalogEntry>,
) -> Result<LinkSpec, ErrorData> {
    let link = match connection {
        LabPlanConnectionInput::BitcoinPeer { id, node_a, node_b } => LinkSpec {
            id: id.clone(),
            kind: LinkKind::BitcoinPeer,
            from: node_a.clone(),
            to: node_b.clone(),
            binding: None,
        },
        LabPlanConnectionInput::LightningPeer { id, node_a, node_b } => LinkSpec {
            id: id.clone(),
            kind: LinkKind::LightningPeer,
            from: node_a.clone(),
            to: node_b.clone(),
            binding: None,
        },
        LabPlanConnectionInput::ChainBackend {
            id,
            component,
            chain,
            network,
        } => LinkSpec {
            id: id.clone(),
            kind: LinkKind::ChainBackend,
            from: component.clone(),
            to: chain.clone(),
            binding: Some(DependencyBinding::Chain {
                network: network.unwrap_or(BitcoinNetwork::Regtest),
            }),
        },
        LabPlanConnectionInput::PaymentBackend {
            id,
            mint,
            lightning,
            method,
            unit,
        } => {
            let binding = resolve_plan_payment_binding(
                id,
                plan_endpoint(selected_entries, mint)?,
                plan_endpoint(selected_entries, lightning)?,
                *method,
                unit.as_deref(),
            )?;
            LinkSpec {
                id: id.clone(),
                kind: LinkKind::PaymentBackend,
                from: mint.clone(),
                to: lightning.clone(),
                binding: Some(DependencyBinding::Payment {
                    method: binding.method,
                    unit: binding.unit.clone(),
                }),
            }
        }
        LabPlanConnectionInput::DatabaseBackend {
            id,
            component,
            database,
            role,
        } => {
            let target = plan_endpoint(selected_entries, database)?;
            LinkSpec {
                id: id.clone(),
                kind: LinkKind::DatabaseBackend,
                from: component.clone(),
                to: database.clone(),
                binding: Some(DependencyBinding::Database {
                    role: role.unwrap_or(
                        if target.features.contains(&CatalogFeature::RedisCache) {
                            DatabaseRole::Cache
                        } else {
                            DatabaseRole::Primary
                        },
                    ),
                }),
            }
        }
        LabPlanConnectionInput::AuthenticationBackend {
            id,
            component,
            identity_provider,
            protocol,
        } => LinkSpec {
            id: id.clone(),
            kind: LinkKind::AuthenticationBackend,
            from: component.clone(),
            to: identity_provider.clone(),
            binding: Some(DependencyBinding::Authentication {
                protocol: protocol.unwrap_or(AuthenticationProtocol::Oidc),
            }),
        },
        LabPlanConnectionInput::NetworkPath { id, source, target } => LinkSpec {
            id: id.clone(),
            kind: LinkKind::NetworkPath,
            from: source.clone(),
            to: target.clone(),
            binding: None,
        },
    };
    Ok(link)
}

fn resolve_plan_payment_binding<'a>(
    link_id: &str,
    source: &'a CatalogEntry,
    target: &CatalogEntry,
    method: Option<PaymentMethod>,
    unit: Option<&str>,
) -> Result<&'a proofstorm_core::CatalogPaymentBindingSupport, ErrorData> {
    let candidates = source
        .support_matrix
        .payment_bindings
        .iter()
        .filter(|binding| {
            binding.backend.implementation == target.id
                && binding.backend.versions.contains(&target.version)
                && method.is_none_or(|method| binding.method == method)
                && unit.is_none_or(|unit| binding.unit == unit)
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        return Ok(candidates[0]);
    }
    Err(ErrorData::invalid_request(
        format!(
            "payment backend connection {link_id:?} has {} compatible binding choices; exactly one is required",
            candidates.len()
        ),
        Some(serde_json::json!({
            "code": if candidates.is_empty() {
                "lab_plan_payment_binding_unsupported"
            } else {
                "lab_plan_payment_binding_ambiguous"
            },
            "link_id": link_id,
            "from_implementation": source.id,
            "to_implementation": target.id,
            "requested_method": method,
            "requested_unit": unit,
            "compatible_bindings": source.support_matrix.payment_bindings,
            "recovery": "choose a compatible backend or specify method and unit when multiple choices remain",
        })),
    ))
}

fn default_plan_control(kind: ComponentKind, allowed: &[ControlClass]) -> ControlClass {
    let preferred = match kind {
        ComponentKind::Mint => ControlClass::Target,
        ComponentKind::Attacker => ControlClass::Attacker,
        ComponentKind::Oracle => ControlClass::Oracle,
        ComponentKind::Bitcoin
        | ComponentKind::Lightning
        | ComponentKind::Database
        | ComponentKind::IdentityProvider
        | ComponentKind::Wallet
        | ComponentKind::Proxy => ControlClass::Laboratory,
    };
    if allowed.contains(&preferred) {
        preferred
    } else {
        allowed.first().copied().unwrap_or(preferred)
    }
}

fn resolved_plan_components(lab: &LabSpec) -> Vec<LabPlanResolvedComponent> {
    lab.components
        .iter()
        .map(|component| LabPlanResolvedComponent {
            id: component.id.clone(),
            kind: component.kind,
            implementation: component.implementation.clone(),
            version: component.version.clone().unwrap_or_default(),
            config_version: component.config_version.clone(),
            control: component.control,
        })
        .collect()
}

fn resolved_plan_runtime_endpoints(
    lab: &LabSpec,
) -> Result<Vec<LabPlanResolvedRuntimeEndpoint>, ErrorData> {
    let catalog = default_catalog();
    lab.components
        .iter()
        .flat_map(|component| {
            let entry = exact_catalog_entry(
                &catalog.entries,
                &component.implementation,
                component.version.as_deref().unwrap_or_default(),
            );
            match entry {
                Ok(entry) => entry
                    .runtime_endpoints
                    .iter()
                    .map(|endpoint| {
                        Ok(LabPlanResolvedRuntimeEndpoint {
                            component: component.id.clone(),
                            endpoint: endpoint.id.clone(),
                            kind: endpoint.kind.clone(),
                            controls: endpoint.controls.clone(),
                            limitations: endpoint.limitations.clone(),
                        })
                    })
                    .collect::<Vec<_>>(),
                Err(error) => vec![Err(error)],
            }
        })
        .collect()
}

fn preferred_recipe_component(
    component_id: &str,
    implementation: &str,
    control: ControlClass,
    config: BTreeMap<String, serde_json::Value>,
) -> Result<ComponentSpec, ErrorData> {
    let catalog = default_catalog();
    let version = catalog
        .implementations
        .iter()
        .find(|support| support.implementation == implementation)
        .map(|support| support.preferred_version.as_str())
        .ok_or_else(|| {
            ErrorData::internal_error(
                format!("built-in recipe implementation {implementation:?} is not installed"),
                Some(serde_json::json!({"code": "lab_recipe_catalog_entry_missing"})),
            )
        })?;
    let entry = exact_catalog_entry(&catalog.entries, implementation, version)?;
    if !entry.allowed_control.contains(&control) {
        return Err(ErrorData::internal_error(
            format!(
                "built-in recipe control {control:?} is invalid for {implementation} {version}"
            ),
            Some(serde_json::json!({"code": "lab_recipe_control_invalid"})),
        ));
    }
    Ok(ComponentSpec {
        id: component_id.into(),
        kind: entry.kind,
        implementation: entry.id.clone(),
        version: Some(entry.version.clone()),
        config_version: entry.config_version.clone(),
        control,
        config,
        (
            "proofstorm_component_restart",
            &[Capability::ComponentControl],
        ),
        (
            "proofstorm_component_exec_live",
            &[Capability::ComponentExecLive],
        ),
        (
            "proofstorm_component_forensics",
            &[Capability::ComponentForensics],
        ),
}

#[allow(
    clippy::too_many_lines,
    reason = "the recipe keeps one auditable topology declaration together"
)]
fn lab_from_recipe(recipe: LabRecipe, name: String) -> Result<LabSpec, ErrorData> {
    match recipe {
        LabRecipe::NutshellLndClnRoutingFees => Ok(LabSpec {
            api_version: API_VERSION.into(),
            name,
            components: vec![
                preferred_recipe_component(
                    "bitcoin-core",
                    "bitcoin-core",
                    ControlClass::Laboratory,
                    BTreeMap::new(),
                )?,
                preferred_recipe_component(
                    "lnd-backend",
                    "lnd",
                    ControlClass::Laboratory,
                    BTreeMap::from([("alias".into(), serde_json::json!("lnd-backend"))]),
                )?,
                preferred_recipe_component(
                    "lnd-router",
                    "lnd",
                    ControlClass::Laboratory,
                    BTreeMap::from([("alias".into(), serde_json::json!("lnd-router"))]),
                )?,
                preferred_recipe_component(
                    "cln-backend",
                    "cln",
                    ControlClass::Laboratory,
                    BTreeMap::from([("alias".into(), serde_json::json!("cln-backend"))]),
                )?,
                preferred_recipe_component(
                    "mint-lnd",
                    "nutshell",
                    ControlClass::Target,
                    BTreeMap::from([(
                        "name".into(),
                        serde_json::json!("Nutshell mint backed by LND"),
                    )]),
                )?,
                preferred_recipe_component(
                    "mint-cln",
                    "nutshell",
                    ControlClass::Target,
                    BTreeMap::from([(
                        "name".into(),
                        serde_json::json!("Nutshell mint backed by CLN"),
                    )]),
                )?,
                preferred_recipe_component(
                    "payer-lnd",
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                    BTreeMap::new(),
                )?,
                preferred_recipe_component(
                    "recipient-lnd",
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                    BTreeMap::new(),
                )?,
                preferred_recipe_component(
                    "payer-cln",
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                    BTreeMap::new(),
                )?,
                preferred_recipe_component(
                    "recipient-cln",
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                    BTreeMap::new(),
                )?,
            ],
            links: vec![
                LinkSpec {
                    id: "lnd-chain".into(),
                    kind: LinkKind::ChainBackend,
                    from: "lnd-backend".into(),
                    to: "bitcoin-core".into(),
                    binding: Some(DependencyBinding::Chain {
                        network: BitcoinNetwork::Regtest,
                    }),
                },
                LinkSpec {
                    id: "router-chain".into(),
                    kind: LinkKind::ChainBackend,
                    from: "lnd-router".into(),
                    to: "bitcoin-core".into(),
                    binding: Some(DependencyBinding::Chain {
                        network: BitcoinNetwork::Regtest,
                    }),
                },
                LinkSpec {
        (
            "proofstorm_wallet_melt_quote_refresh",
            &[Capability::WalletControl],
        ),
                    id: "cln-chain".into(),
                    kind: LinkKind::ChainBackend,
                    from: "cln-backend".into(),
                    to: "bitcoin-core".into(),
                    binding: Some(DependencyBinding::Chain {
                        network: BitcoinNetwork::Regtest,
                    }),
                },
                LinkSpec {
                    id: "lnd-router-peer".into(),
                    kind: LinkKind::LightningPeer,
                    from: "lnd-backend".into(),
                    to: "lnd-router".into(),
                    binding: None,
                },
                LinkSpec {
                    id: "router-cln-peer".into(),
                    kind: LinkKind::LightningPeer,
                    from: "lnd-router".into(),
                    to: "cln-backend".into(),
                    binding: None,
                },
                LinkSpec {
                    id: "mint-lnd-payment".into(),
                    kind: LinkKind::PaymentBackend,
                    from: "mint-lnd".into(),
                    to: "lnd-backend".into(),
                    binding: Some(DependencyBinding::Payment {
                        method: PaymentMethod::Bolt11,
                        unit: "sat".into(),
                    }),
                },
                LinkSpec {
                    id: "mint-cln-payment".into(),
                    kind: LinkKind::PaymentBackend,
                    from: "mint-cln".into(),
                    to: "cln-backend".into(),
                    binding: Some(DependencyBinding::Payment {
                        method: PaymentMethod::Bolt11,
                        unit: "sat".into(),
                    }),
                },
            ],
            policy: LabPolicy::default(),
        }),
    }
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
            "Use catalog_list to discover implementation IDs, then lab_plan to describe roles and connections for any supported topology. Proofstorm resolves preferred versions, kinds, controls, config contracts, and unambiguous dependency bindings. Verify the returned normalized plan and call lab_apply with its digest; do not substitute an unrelated recipe for a requested topology. After readiness, create one experiment and lease, use typed runtime operations, release the lease, close the experiment, export evidence, and close and await the lab. Read full evidence only through its manifest resource_uri; use proofstorm_evidence_section_read for bounded inspection.",
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
        (
            "proofstorm_component_restart",
            &[Capability::ComponentControl],
        ),
        (
            "proofstorm_component_exec_live",
            &[Capability::ComponentExecLive],
        ),
        (
            "proofstorm_component_forensics",
            &[Capability::ComponentForensics],
        ),
        ("proofstorm_catalog_entry_read", &[Capability::CatalogRead]),
        (
            "proofstorm_catalog_config_schema_read",
            &[Capability::CatalogRead],
        ),
        (
            "proofstorm_network_capabilities",
            &[Capability::CatalogRead],
        ),
        (
            "proofstorm_lab_plan",
            &[Capability::CatalogRead, Capability::LabCreate],
        ),
        (
            "proofstorm_lab_apply",
            &[
                Capability::LabRead,
                Capability::LabPublish,
                Capability::LabMaterialize,
            ],
        ),
        ("proofstorm_lab_create", &[Capability::LabCreate]),
        ("proofstorm_lab_recipe_create", &[Capability::LabCreate]),
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

#[allow(
    clippy::too_many_lines,
    reason = "the runtime discovery contract deliberately lists every tool and required grant"
)]
fn runtime_tool_capabilities() -> Vec<(&'static str, &'static [Capability])> {
    vec![
        ("proofstorm_node_start", &[Capability::NodeControl]),
        ("proofstorm_node_stop", &[Capability::NodeControl]),
        ("proofstorm_node_restart", &[Capability::NodeControl]),
        ("proofstorm_component_exec", &[Capability::ComponentExec]),
        (
            "proofstorm_lab_recipe_bootstrap",
            &[
                Capability::ChainMine,
                Capability::WalletFund,
                Capability::PeerConnect,
                Capability::ChannelOpen,
            ],
        ),
        (
            "proofstorm_lab_recipe_route_channel_open",
            &[
                Capability::ChannelOpen,
                Capability::ChainMine,
                Capability::ExperimentRead,
            ],
        ),
        (
            "proofstorm_lab_recipe_fee_matrix_run",
            &[
                Capability::WalletCreate,
                Capability::WalletFund,
                Capability::WalletControl,
                Capability::ChannelOpen,
                Capability::ExperimentRead,
                Capability::ArtifactRead,
                Capability::OracleRun,
            ],
        ),
        (
            "proofstorm_liquidity_bootstrap",
            &[
                Capability::ChainMine,
                Capability::WalletFund,
                Capability::PeerConnect,
                Capability::ChannelOpen,
            ],
        ),
        (
            "proofstorm_peer_connect",
            &[Capability::PeerConnect, Capability::ExperimentRead],
        ),
        ("proofstorm_peer_disconnect", &[Capability::PeerDisconnect]),
        (
            "proofstorm_channel_open",
            &[
                Capability::ChannelOpen,
                Capability::ChainMine,
                Capability::ExperimentRead,
            ],
        ),
        (
            "proofstorm_channel_policy_set",
            &[Capability::ChannelOpen, Capability::ExperimentRead],
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
        (
            "proofstorm_conservation_oracle",
            &[Capability::OracleRun, Capability::ArtifactRead],
        ),
        ("proofstorm_reachability_oracle", &[Capability::OracleRun]),
        ("proofstorm_action_cancel", &[Capability::ActionCancel]),
        ("proofstorm_operation_status", &[Capability::ArtifactRead]),
        ("proofstorm_operation_wait", &[Capability::ArtifactRead]),
        (
            "proofstorm_operation_wait_many",
            &[Capability::ArtifactRead],
        ),
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

    /// Refuse peer/channel actions until the experiment's chain and initial
    /// LND liquidity have been initialized. This check happens before the
    /// requested action is journaled, so a premature call cannot consume an
    /// action sequence or become an opaque Kubernetes failure.
    fn require_liquidity_bootstrap(
        &self,
        experiment_id: &str,
        instance_id: &str,
    ) -> Result<LabOperation, ErrorData> {
        let actions = self
            .store
            .actions(&self.workspace, &self.principal, experiment_id, 0, 100)
            .map_err(store_error)?;
        let bootstrap = actions.into_iter().rev().find(|action| {
            action.instance_id == instance_id && action.kind == OperationKind::BootstrapLiquidity
        });
        match bootstrap {
            Some(action) if action.phase == OperationPhase::Succeeded => Ok(action),
            Some(action)
                if matches!(
                    action.phase,
                    OperationPhase::Pending | OperationPhase::Running
                ) =>
            {
                let operation_id = action.id;
                Err(ErrorData::invalid_request(
                    format!(
                        "liquidity bootstrap operation {operation_id:?} is not terminal; wait for it to succeed before connecting peers or opening channels"
                    ),
                    Some(serde_json::json!({
                        "code": "runtime_initialization_in_progress",
                        "operation_id": operation_id,
                        "next_tool": "proofstorm_operation_wait"
                    })),
                ))
            }
            Some(action) => {
                let operation_id = action.id;
                Err(ErrorData::invalid_request(
                    format!(
                        "liquidity bootstrap operation {operation_id:?} did not succeed; inspect its artifact, then submit a new liquidity_bootstrap operation"
                    ),
                    Some(serde_json::json!({
                        "code": "runtime_initialization_failed",
                        "operation_id": operation_id,
                        "next_tool": "proofstorm_operation_status",
                        "recovery_tool": "proofstorm_liquidity_bootstrap"
                    })),
                ))
            }
            None => Err(ErrorData::invalid_request(
                "regtest infrastructure is running but Lightning is not initialized; call proofstorm_liquidity_bootstrap with the Bitcoin component and two LND components, wait for it to succeed, then connect peers and open any additional LND/CLN channels",
                Some(serde_json::json!({
                    "code": "runtime_initialization_required",
                    "next_tool": "proofstorm_liquidity_bootstrap",
                    "required_sequence": [
                        "proofstorm_liquidity_bootstrap",
                        "proofstorm_operation_wait",
                        "proofstorm_peer_connect",
                        "proofstorm_operation_wait",
                        "proofstorm_channel_open",
                        "proofstorm_operation_wait"
                    ],
                    "mixed_backend_hint": "bootstrap the LND-LND edge first; then open the LND-to-CLN edge from the funded LND node"
                })),
            )),
        }
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
    let valid_component_ids = || valid_component_ids(revision, kind, Some(implementation));
    let Some(component) = revision
        .lab
        .components
        .iter()
        .find(|component| component.id == id)
    else {
        let valid_component_ids = valid_component_ids();
        return Err(ErrorData::invalid_request(
            format!(
                "component {id:?} is not in this revision; expected {} using {implementation:?}; valid component IDs: {valid_component_ids:?}",
                component_kind_name(kind)
            ),
            Some(serde_json::json!({
                "code": "component_id_unknown",
                "requested_id": id,
                "expected_kind": kind,
                "expected_implementation": implementation,
                "valid_component_ids": valid_component_ids,
            })),
        ));
    };
    if component.kind != kind {
        let valid_component_ids = valid_component_ids();
        return Err(ErrorData::invalid_request(
            format!(
                "component {id:?} is {}; expected {} using {implementation:?}; valid component IDs: {valid_component_ids:?}",
                component_kind_name(component.kind),
                component_kind_name(kind)
            ),
            Some(serde_json::json!({
                "code": "component_kind_mismatch",
                "requested_id": id,
                "actual_kind": component.kind,
                "expected_kind": kind,
                "expected_implementation": implementation,
                "valid_component_ids": valid_component_ids,
            })),
        ));
    }
    if component.implementation != implementation {
        let valid_component_ids = valid_component_ids();
        return Err(ErrorData::invalid_request(
            format!(
                "component {id:?} uses {:?}; expected {implementation:?}; valid component IDs: {valid_component_ids:?}",
                component.implementation
            ),
            Some(serde_json::json!({
                "code": "component_implementation_mismatch",
                "requested_id": id,
                "actual_implementation": component.implementation,
                "expected_kind": kind,
                "expected_implementation": implementation,
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

fn component_image_any(
    revision: &PublishedRevision,
    id: &str,
    kind: ComponentKind,
) -> Result<String, ErrorData> {
    let valid_component_ids = || valid_component_ids(revision, kind, None);
    let Some(component) = revision
        .lab
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
) -> Result<(), ErrorData> {
    let component = revision
        .lab
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
        &default_catalog().entries,
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
        .lab
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
        ComponentKind::Attacker => "attacker",
        ComponentKind::Proxy => "proxy",
        ComponentKind::Oracle => "oracle",
    }
}
fn validate_quote_id(quote_id: &str) -> Result<(), ErrorData> {
    if quote_id.is_empty()
        || quote_id.len() > 256
        || quote_id
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(invalid_operation(
            "melt_quote_id must be a non-empty opaque identifier of at most 256 bytes without whitespace",
        ));
    }
    Ok(())
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

fn validate_wallet_fund_payer(
    lab: &LabSpec,
    mint: &str,
    payer_lightning: &str,
) -> Result<(), ErrorData> {
    let is_own_backend = lab.links.iter().any(|link| {
        link.kind == LinkKind::PaymentBackend && link.from == mint && link.to == payer_lightning
    });
    if !is_own_backend {
        return Ok(());
    }

    let eligible = lab
        .components
        .iter()
        .filter(|component| {
            component.kind == ComponentKind::Lightning
                && component.implementation == "lnd"
                && component.id != payer_lightning
        })
        .map(|component| component.id.as_str())
        .collect::<Vec<_>>();
    let suggestion = if eligible.is_empty() {
        "add a distinct LND payer node".to_owned()
    } else {
        format!(
            "use one of these distinct LND payer nodes: {}",
            eligible.join(", ")
        )
    };
    Err(coded_invalid_request(
        "self_payment_unsupported",
        format!(
            "payer_lightning {payer_lightning:?} is the payment backend for mint {mint:?} and cannot pay its own invoice; {suggestion}"
        ),
    ))
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

fn validate_channel_funding_admission(
    request: &ChannelOpenRequest,
    bootstrap_operation: &LabOperation,
) -> Result<(), ErrorData> {
    let bootstrap =
        serde_json::from_value::<StoredBootstrapFunding>(bootstrap_operation.request.clone())
            .map_err(|_| {
                ErrorData::internal_error(
                    "stored liquidity bootstrap request does not match its typed contract",
                    Some(serde_json::json!({
                        "code": "bootstrap_request_contract_violation",
                        "operation_id": bootstrap_operation.id,
                    })),
                )
            })?;
    let prior_channel_sat = if request.from_lightning == bootstrap.payer_lightning {
        bootstrap.channel_sat
    } else if request.from_lightning == bootstrap.mint_lightning {
        0
    } else {
        return Err(ErrorData::invalid_request(
            format!(
                "channel funding for {:?} is not established by the succeeded liquidity bootstrap; open the channel from a funded bootstrap LND component",
                request.from_lightning
            ),
            Some(serde_json::json!({
                "code": "channel_funding_source_unproven",
                "bootstrap_operation_id": bootstrap_operation.id,
                "requested_from": request.from_lightning,
                "funded_components": [bootstrap.payer_lightning, bootstrap.mint_lightning],
                "recommended_from": bootstrap.payer_lightning,
                "next_tool": "proofstorm_channel_open",
            })),
        ));
    };
    let safe_channel_sat = bootstrap
        .funding_sat
        .saturating_sub(prior_channel_sat)
        .saturating_sub(BOOTSTRAP_FUNDING_MARGIN_SAT);
    if request.channel_sat <= safe_channel_sat {
        return Ok(());
    }
    Err(ErrorData::invalid_request(
        format!(
            "channel_sat {} exceeds the safe remaining on-chain budget of {safe_channel_sat} sat for {:?}; retry with channel_sat <= {safe_channel_sat}",
            request.channel_sat, request.from_lightning
        ),
        Some(serde_json::json!({
            "code": "insufficient_channel_funding_margin",
            "bootstrap_operation_id": bootstrap_operation.id,
            "from_lightning": request.from_lightning,
            "funding_sat": bootstrap.funding_sat,
            "prior_channel_sat": prior_channel_sat,
            "reserved_fee_margin_sat": BOOTSTRAP_FUNDING_MARGIN_SAT,
            "requested_channel_sat": request.channel_sat,
            "safe_max_channel_sat": safe_channel_sat,
            "recommended_channel_sat": safe_channel_sat,
            "next_tool": "proofstorm_channel_open",
        })),
    ))
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

fn conservation_observation(
    request: &ConservationOracleRequest,
    baseline: &LabOperation,
    treatment: &LabOperation,
    workspace: &str,
    principal: &str,
) -> Result<serde_json::Value, ErrorData> {
    let same_scope = |operation: &LabOperation| {
        operation.workspace_id == workspace
            && operation.principal_id == principal
            && operation.instance_id == request.instance_id
            && operation.experiment_id == request.experiment_id
    };
    if baseline.id != request.baseline_operation_id
        || baseline.kind != OperationKind::WalletBalance
        || baseline.phase != OperationPhase::Succeeded
        || !same_scope(baseline)
        || baseline
            .request
            .get("wallet")
            .and_then(serde_json::Value::as_str)
            != Some(request.wallet.as_str())
        || baseline
            .request
            .get("mint")
            .and_then(serde_json::Value::as_str)
            != Some(request.mint.as_str())
    {
        return Err(coded_invalid_request(
            "conservation_baseline_invalid",
            "baseline_operation_id must name an earlier successful wallet_balance for the same principal, instance, experiment, wallet, and mint",
        ));
    }
    if treatment.id != request.treatment_operation_id
        || treatment.kind != OperationKind::WalletPay
        || treatment.phase != OperationPhase::Succeeded
        || !same_scope(treatment)
        || treatment.sequence <= baseline.sequence
        || treatment
            .request
            .get("wallet")
            .and_then(serde_json::Value::as_str)
            != Some(request.wallet.as_str())
        || treatment
            .request
            .get("mint")
            .and_then(serde_json::Value::as_str)
            != Some(request.mint.as_str())
    {
        return Err(coded_invalid_request(
            "conservation_treatment_invalid",
            "treatment_operation_id must name a later successful wallet_pay for the same principal, instance, experiment, wallet, and mint; wallet_round_trip is not balance-invariant because it mints external value first",
        ));
    }
    let baseline_sat = baseline
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.content.get("balance_sat"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_baseline_artifact_invalid",
                "the baseline wallet_balance artifact has no unsigned balance_sat",
            )
        })?;
    if baseline_sat > 100_000_000 {
        return Err(coded_invalid_request(
            "conservation_baseline_out_of_bounds",
            "the baseline balance_sat cannot exceed 100,000,000",
        ));
    }
    let treatment = conservation_treatment_evidence(request, treatment, baseline_sat)?;
    let delta_sat = treatment.expected_sat.abs_diff(treatment.actual_sat);
    Ok(serde_json::json!({
        "baseline_operation_id": request.baseline_operation_id,
        "treatment_operation_id": request.treatment_operation_id,
        "baseline_sat": baseline_sat,
        "melt_state": treatment.melt_state,
        "amount_sat": treatment.amount_sat,
        "fee_paid_sat": treatment.fee_paid_sat,
        "input_fee_sat": treatment.input_fee_sat,
        "input_proof_count": treatment.input_proof_count,
        "expected_sat": treatment.expected_sat,
        "actual_sat": treatment.actual_sat,
        "delta_sat": delta_sat,
        "conserved": delta_sat == 0,
    }))
}

            "Ready means infrastructure/protocol availability only, not mature regtest blocks or Lightning liquidity. For a lab created from a server-owned recipe, create an experiment and lease, then run and await lab_recipe_bootstrap followed by lab_recipe_route_channel_open; Proofstorm owns the exact component IDs and safe liquidity values. For custom labs, use liquidity_bootstrap followed by channel_open. Prefer typed channel_policy_set for routing policies; reserve live native CLI execution for behavior without a typed control.",
    actual_sat: u64,
    melt_state: String,
    amount_sat: u64,
    fee_paid_sat: u64,
    input_fee_sat: u64,
    input_proof_count: u64,
    expected_sat: u64,
}

fn conservation_input_evidence(
    treatment_content: &serde_json::Value,
    melt_state: &str,
) -> Result<(u64, u64), ErrorData> {
    let invalid =
        |message| coded_invalid_request("conservation_treatment_artifact_invalid", message);
    let input_fee_sat = treatment_content
        .get("input_fee_sat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            invalid("the wallet_pay treatment artifact has no exact unsigned input_fee_sat")
        })?;
    let input_proof_count = treatment_content
        .get("input_proof_count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            invalid("the wallet_pay treatment artifact has no exact unsigned input_proof_count")
        })?;
    if input_fee_sat > 100_000 || input_proof_count > 10_000 {
        return Err(invalid(
            "the observed input fee or proof count exceeds its evidence bound",
        ));
    }
    match melt_state {
        "PAID" if input_proof_count == 0 => Err(invalid(
            "a PAID melt must identify at least one spent input proof",
        )),
        "UNPAID" if input_fee_sat != 0 || input_proof_count != 0 => Err(invalid(
            "an UNPAID melt cannot report spent input proofs or an input fee",
        )),
        _ => Ok((input_fee_sat, input_proof_count)),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the fail-closed evidence parser validates all correlated wallet-pay fields in one auditable path"
)]
fn conservation_treatment_evidence(
    request: &ConservationOracleRequest,
    treatment: &LabOperation,
    baseline_sat: u64,
) -> Result<ConservationTreatmentEvidence, ErrorData> {
    let treatment_content = treatment
        .artifact
        .as_ref()
        .map(|artifact| &artifact.content)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the wallet_pay treatment has no terminal artifact",
            )
        })?;
    let actual_sat = treatment_content
        .get("payer_balance_sat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the wallet_pay treatment artifact has no unsigned payer_balance_sat",
            )
        })?;
    let melt = treatment_content
        .get("quote_observations")
        .and_then(serde_json::Value::as_array)
        .and_then(|observations| {
            observations.iter().find(|observation| {
                observation.get("role").and_then(serde_json::Value::as_str) == Some("payment_melt")
                    && observation
                        .get("wallet_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(request.wallet.as_str())
                    && observation
                        .get("mint_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(request.mint.as_str())
            })
        })
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the wallet_pay treatment artifact has no matching payment_melt observation",
            )
        })?;
    let melt_state = melt
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the payment_melt observation has no state",
            )
        })?;
    let amount_sat = melt
        .get("amount_sat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the payment_melt observation has no unsigned amount_sat",
            )
        })?;
    let melt_state = melt_state.to_ascii_uppercase();
    let (input_fee_sat, input_proof_count) =
        conservation_input_evidence(treatment_content, &melt_state)?;
    let (expected_sat, fee_paid_sat) = match melt_state.as_str() {
        "PAID" => {
            let fee = melt
                .get("fee_paid_sat")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    coded_invalid_request(
                        "conservation_treatment_artifact_invalid",
                        "a PAID payment_melt observation has no unsigned fee_paid_sat",
                    )
                })?;
            let debit = amount_sat
                .checked_add(fee)
                .and_then(|debit| debit.checked_add(input_fee_sat))
                .ok_or_else(|| {
                    coded_invalid_request(
                        "conservation_expected_balance_invalid",
                        "the observed payment debit overflows",
                    )
                })?;
            let expected = baseline_sat.checked_sub(debit).ok_or_else(|| {
                coded_invalid_request(
                    "conservation_expected_balance_invalid",
                    "the observed payment debit exceeds the baseline balance",
                )
            })?;
            (expected, fee)
        }
        "UNPAID" => (baseline_sat, 0),
        _ => {
            return Err(coded_invalid_request(
                "conservation_treatment_not_settled",
                "the wallet_pay melt must be PAID or UNPAID before conservation can be evaluated",
            ));
        }
    };
    Ok(ConservationTreatmentEvidence {
        actual_sat,
        melt_state,
        amount_sat,
        fee_paid_sat,
        input_fee_sat,
        input_proof_count,
        expected_sat,
    })
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

fn validate_operation_wait_many_request(
    request: &OperationWaitManyRequest,
) -> Result<(), ErrorData> {
    validate_wait_timeout(request.timeout_seconds)?;
    if !(1..=8).contains(&request.operation_ids.len()) {
        return Err(ErrorData::invalid_request(
            "operation_ids must contain between 1 and 8 IDs".to_owned(),
            Some(serde_json::json!({
                "code": "operation_wait_many_count_invalid",
                "minimum": 1,
                "maximum": 8,
                "requested": request.operation_ids.len(),
            })),
        ));
    }
    let unique = request.operation_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != request.operation_ids.len() {
        return Err(ErrorData::invalid_request(
            "operation_ids must be unique".to_owned(),
            Some(serde_json::json!({
                "code": "operation_wait_many_duplicate_id",
            })),
        ));
    }
    Ok(())
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
            "Ready means infrastructure/protocol availability only, not mature regtest blocks or Lightning liquidity. For a lab created from a server-owned recipe, create an experiment and lease, then run and await lab_recipe_bootstrap followed by lab_recipe_route_channel_open; Proofstorm owns the exact component IDs and safe liquidity values. For custom labs, use liquidity_bootstrap followed by channel_open. Prefer typed channel_policy_set for routing policies; reserve live native CLI execution for behavior without a typed control.",
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
        runtime_guidance: runtime_guidance(status.phase).map(str::to_owned),
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
        runtime_guidance: runtime_guidance(status.phase).map(str::to_owned),
        teardown_receipt: status.teardown_receipt,
        message: status.message,
    }
}

const fn runtime_guidance(phase: InstancePhase) -> Option<&'static str> {
    match phase {
        InstancePhase::Ready => Some(
            "Ready means infrastructure/protocol availability only, not mature regtest blocks or Lightning liquidity. For a lab created from a server-owned recipe, create an experiment and lease, then run and await lab_recipe_bootstrap followed by lab_recipe_route_channel_open; Proofstorm owns the exact component IDs and safe liquidity values. For custom labs, use liquidity_bootstrap followed by channel_open. Set routing policies with channel_policy_set, not component_exec.",
        ),
        _ => None,
    }
}

fn require_matrix_stage(
    result: &OperationWaitManyResult,
    stage: &str,
    expected_operations: usize,
) -> Result<(), ErrorData> {
    if result.all_terminal
        && !result.timed_out
        && result.operations.len() == expected_operations
        && result
            .operations
            .iter()
            .all(|operation| operation.phase == OperationPhase::Succeeded)
    {
        return Ok(());
    }
    Err(ErrorData::invalid_request(
        format!("recipe fee matrix stopped during {stage}"),
        Some(serde_json::json!({
            "code": "recipe_fee_matrix_stage_failed",
            "stage": stage,
            "operations": result.operations.iter().map(|operation| serde_json::json!({
                "operation_id": operation.operation_id,
                "phase": operation.phase,
                "timed_out": operation.timed_out,
            })).collect::<Vec<_>>(),
            "recovery": "inspect the listed operation artifacts, correct the runtime issue, then replay the same matrix_id",
        })),
    ))
}

fn matrix_invoice_quote_id(operation: &OperationWaitResult) -> Result<String, ErrorData> {
    operation
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.content.get("mint_quote_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            coded_invalid_request(
                "recipe_fee_matrix_invoice_artifact_invalid",
                format!(
                    "invoice operation {:?} has no private mint quote reference",
                    operation.operation_id
                ),
            )
        })
}

fn matrix_case_summary(
    treatment: &str,
    direction: &str,
    base_fee_sat: u64,
    fee_rate_ppm: u32,
    payment: &OperationWaitResult,
    oracle: &LabOperation,
) -> Result<serde_json::Value, ErrorData> {
    let payment_content = payment
        .artifact
        .as_ref()
        .map(|artifact| &artifact.content)
        .ok_or_else(|| {
            coded_invalid_request(
                "recipe_fee_matrix_payment_artifact_missing",
                format!(
                    "payment operation {:?} has no terminal artifact",
                    payment.operation_id
                ),
            )
        })?;
    let observations = wallet_quote_observations_from_artifact(payment_content).map_err(|_| {
        coded_invalid_request(
            "recipe_fee_matrix_payment_artifact_invalid",
            format!(
                "payment operation {:?} has invalid quote observations",
                payment.operation_id
            ),
        )
    })?;
    let melt = observations
        .iter()
        .find(|observation| observation.role == WalletQuoteObservationRole::PaymentMelt)
        .ok_or_else(|| {
            coded_invalid_request(
                "recipe_fee_matrix_melt_observation_missing",
                format!(
                    "payment operation {:?} has no melt observation",
                    payment.operation_id
                ),
            )
        })?;
    let receive = observations
        .iter()
        .find(|observation| observation.role == WalletQuoteObservationRole::PaymentReceive)
        .ok_or_else(|| {
            coded_invalid_request(
                "recipe_fee_matrix_receive_observation_missing",
                format!(
                    "payment operation {:?} has no receive observation",
                    payment.operation_id
                ),
            )
        })?;
    let oracle_content = oracle
        .artifact
        .as_ref()
        .map(|artifact| &artifact.content)
        .ok_or_else(|| {
            coded_invalid_request(
                "recipe_fee_matrix_oracle_artifact_missing",
                format!("oracle operation {:?} has no result", oracle.id),
            )
        })?;
    Ok(serde_json::json!({
        "treatment": treatment,
        "direction": direction,
        "routing_policy": {
            "base_fee_sat": base_fee_sat,
            "fee_rate_ppm": fee_rate_ppm,
        },
        "payer_wallet": melt.wallet_id,
        "payer_mint": melt.mint_id,
        "recipient_wallet": receive.wallet_id,
        "recipient_mint": receive.mint_id,
        "amount_sat": melt.amount_sat,
        "melt_state": melt.state,
        "receive_state": receive.state,
        "fee_reserve_sat": melt.fee_reserve_sat,
        "fee_paid_sat": melt.fee_paid_sat,
        "input_fee_sat": payment_content.get("input_fee_sat"),
        "payer_balance_after_sat": payment_content.get("payer_balance_sat"),
        "recipient_balance_after_sat": payment_content.get("recipient_balance_sat"),
        "balance_before_sat": oracle_content.get("baseline_sat"),
        "expected_balance_after_sat": oracle_content.get("expected_sat"),
        "actual_balance_after_sat": oracle_content.get("actual_sat"),
        "conserved": oracle_content.get("conserved"),
        "conservation_delta_sat": oracle_content.get("delta_sat"),
        "payment_operation_id": payment.operation_id,
        "oracle_operation_id": oracle.id,
    }))
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

fn compact_operation_wait_many(
    operations: Vec<LabOperation>,
    timed_out: bool,
) -> Result<OperationWaitManyResult, ErrorData> {
    let all_terminal = operations
        .iter()
        .all(|operation| operation_terminal(operation.phase));
    let mut result = OperationWaitManyResult {
        operations: operations
            .into_iter()
            .map(|operation| {
                let operation_timed_out = timed_out && !operation_terminal(operation.phase);
                compact_operation_wait(operation, operation_timed_out)
            })
            .collect(),
        all_terminal,
        timed_out,
        artifact_bodies_omitted: false,
    };
    if serialized_size(&result)? > MAX_AGENT_RESPONSE_BYTES {
        for operation in &mut result.operations {
            operation.artifact = None;
        }
        result.artifact_bodies_omitted = true;
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
    #[test]
    fn persisted_lab_output_round_trips_back_into_validation() {
        let request = serde_json::from_value::<ValidateLabRequest>(serde_json::json!({
            "lab": {
                "api_version": API_VERSION,
                "name": "canonical-round-trip",
                "components": [],
                "links": [{
                    "id": "backend",
                    "kind": "chain_backend",
                    "from": "lightning",
                    "to": "chain",
                    "binding": {"type": "chain", "network": "regtest"}
                }],
                "policy": {}
            }
        }))
        .expect("persisted canonical lab output must remain valid MCP input");
        assert!(matches!(
            request.lab.links.as_slice(),
            [AddLinkInput::ChainBackend {
                network: BitcoinNetwork::Regtest,
                ..
            }]
        ));
    }

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

fn invalid_terminal_artifact(
    operation: &LabOperation,
    reported_phase: OperationPhase,
    code: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "message": message,
        "operation_id": operation.id,
        "reported_phase": reported_phase,
        "recoverable": false,
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

    fn authored_lab(name: &str) -> AuthoredLabSpec {
        AuthoredLabSpec {
            api_version: API_VERSION.into(),
            name: name.into(),
            components: vec![],
            links: vec![],
            policy: LabPolicy::default(),
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
        let authored = LabSpec {
            api_version: API_VERSION.into(),
            name: "component-references".into(),
            components: vec![
                component("wallet-b", "nutshell-wallet"),
                component("chain", "bitcoin-core"),
                component("wallet-a", "nutshell-wallet"),
            ],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lab = proofstorm_core::resolve_effective_lab(&authored, catalog)
            .expect("effective component fixture");
    #[test]
    fn persisted_lab_output_round_trips_back_into_validation() {
        let request = serde_json::from_value::<ValidateLabRequest>(serde_json::json!({
            "lab": {
                "api_version": API_VERSION,
                "name": "canonical-round-trip",
                "components": [],
                "links": [{
                    "id": "backend",
                    "kind": "chain_backend",
                    "from": "lightning",
                    "to": "chain",
                    "binding": {"type": "chain", "network": "regtest"}
                }],
                "policy": {}
            }
        }))
        .expect("persisted canonical lab output must remain valid MCP input");
        assert!(matches!(
            request.lab.links.as_slice(),
            [AddLinkInput::ChainBackend {
                network: BitcoinNetwork::Regtest,
                ..
            }]
        ));
    }

        let lock = proofstorm_core::resolve_lock(&lab, catalog).expect("locked component fixture");
        PublishedRevision {
            workspace_id: "alpha".into(),
            digest: digest_json(&(&lab, &lock)),
            lab,
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
    fn authored_lab_policy_is_optional_and_defaults_safely() {
        let authored = serde_json::from_value::<AuthoredLabSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "default-policy",
            "components": [],
            "links": []
        }))
        .expect("policy may be omitted");
        assert_eq!(authored.policy, LabPolicy::default());
    }

    #[test]
    fn lab_request_accepts_object_or_once_stringified_object() {
        let lab = serde_json::json!({
            "api_version": API_VERSION,
            "name": "wire-compatible",
            "components": [],
            "links": []
        });
        let object = serde_json::from_value::<ValidateLabRequest>(serde_json::json!({
            "lab": lab.clone()
        }))
        .expect("canonical object");
        let stringified = serde_json::from_value::<ValidateLabRequest>(serde_json::json!({
            "lab": serde_json::to_string(&lab).expect("encode lab")
        }))
        .expect("once-stringified object");

        assert_eq!(
            serde_json::to_value(object.lab).expect("object value"),
            serde_json::to_value(stringified.lab).expect("stringified value")
        );
    }

    #[test]
    fn persisted_lab_output_round_trips_back_into_validation() {
        let request = serde_json::from_value::<ValidateLabRequest>(serde_json::json!({
            "lab": {
                "api_version": API_VERSION,
                "name": "canonical-round-trip",
                "components": [],
                "links": [{
                    "id": "backend",
                    "kind": "chain_backend",
                    "from": "lightning",
                    "to": "chain",
                    "binding": {"type": "chain", "network": "regtest"}
                }],
                "policy": {}
            }
        }))
        .expect("persisted canonical lab output must remain valid MCP input");
        assert!(matches!(
            request.lab.links.as_slice(),
            [AddLinkInput::ChainBackend {
                network: BitcoinNetwork::Regtest,
                ..
            }]
        ));
    }

    #[test]
    fn stringified_lab_still_enforces_the_strict_contract() {
        for invalid in [
            "not json".to_owned(),
            serde_json::json!({
                "api_version": API_VERSION,
                "name": "missing-structure"
            })
            .to_string(),
            serde_json::json!({
                "api_version": API_VERSION,
                "name": "unknown-field",
                "components": [],
                "links": [],
                "surprise": true
            })
            .to_string(),
        ] {
            assert!(
                serde_json::from_value::<ValidateLabRequest>(serde_json::json!({"lab": invalid}))
                    .is_err(),
                "invalid stringified lab must fail closed"
            );
        }
    }

    #[test]
    fn create_rejects_catalog_invalid_config_without_writing_a_draft() {
        let service = ProofstormMcp::new(seeded_store(), "alpha", "designer").expect("service");
        let authored = serde_json::from_value::<AuthoredLabSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "invalid-alias",
            "components": [
                {
                    "id": "chain",
                    "kind": "bitcoin",
                    "implementation": "bitcoin-core",
                    "version": "30.0",
                    "config_version": "bitcoin-core/30/v1",
                    "control": "laboratory",
                    "config": {}
                },
                {
                    "id": "node",
                    "kind": "lightning",
                    "implementation": "lnd",
                    "version": "0.20.0-beta",
                    "config_version": "lnd/0.20/v1",
                    "control": "laboratory",
                    "config": {"alias": "this-alias-is-deliberately-far-too-long-for-lnd"}
                }
            ],
            "links": [{
                "id": "node-chain",
                "kind": "chain_backend",
                "from": "node",
                "to": "chain",
                "network": "regtest"
            }]
        }))
        .expect("wire-valid authored lab");

        let validation = service
            .proofstorm_lab_validate(Parameters(ValidateLabRequest {
                lab: authored.clone(),
            }))
            .expect("validation result")
            .0;
        assert!(!validation.valid);
        assert!(validation.issues.iter().any(|issue| {
            issue.code == "publication_preflight_failed" && issue.message.contains("alias")
        }));

        let error = match service.proofstorm_lab_create(Parameters(CreateDraftRequest {
            draft_id: "invalid-alias".into(),
            lab: authored,
            idempotency_key: "invalid-alias-once".into(),
        })) {
            Ok(_) => panic!("invalid effective config must not create a draft"),
            Err(error) => error,
        };
        assert_eq!(
            error.data.expect("structured error")["code"],
            "lab_validation_failed"
        );
        assert!(
            service
                .proofstorm_lab_read(Parameters(ReadDraftRequest {
                    draft_id: "invalid-alias".into(),
                }))
                .is_err(),
            "rejected lab left no draft behind"
        );
    }

    #[test]
    fn routing_fee_recipe_creates_a_valid_versioned_topology_in_one_call() {
        let service = ProofstormMcp::new(seeded_store(), "alpha", "designer").expect("service");
        let receipt = service
            .proofstorm_lab_recipe_create(Parameters(CreateLabRecipeRequest {
                draft_id: "routing-fees".into(),
                idempotency_key: "routing-fees-once".into(),
                recipe: LabRecipe::NutshellLndClnRoutingFees,
                name: None,
            }))
            .expect("recipe draft")
            .0;
        assert!(receipt.valid);
        assert_eq!(receipt.component_count, 10);
        assert_eq!(receipt.link_count, 7);
        assert!(receipt.structure.contains("backend_bindings=5/5"));

        let draft = service
            .proofstorm_lab_read(Parameters(ReadDraftRequest {
                draft_id: "routing-fees".into(),
            }))
            .expect("created recipe draft")
            .0;
        assert_eq!(draft.lab.name, "routing-fees");
        for id in [
            "bitcoin-core",
            "lnd-backend",
            "lnd-router",
            "cln-backend",
            "mint-lnd",
            "mint-cln",
            "payer-lnd",
            "recipient-lnd",
            "payer-cln",
            "recipient-cln",
        ] {
            let component = draft
                .lab
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap_or_else(|| panic!("recipe component {id}"));
            assert!(component.version.is_some(), "{id} has an exact version");
        }
        assert!(validate_lab(&draft.lab).issues.is_empty());
    }

    #[test]
    fn routing_fee_recipe_setup_owns_component_ids_and_liquidity_values() {
        let request = LabRecipeSetupRequest {
            instance_id: "instance".into(),
            experiment_id: "experiment".into(),
            lease_id: "lease".into(),
            operation_id: "operation".into(),
            recipe: LabRecipe::NutshellLndClnRoutingFees,
            idempotency_key: "once".into(),
        };
        let bootstrap = recipe_bootstrap_request(request.clone());
        assert_eq!(bootstrap.chain, "bitcoin-core");
        assert_eq!(bootstrap.mint_lightning, "lnd-backend");
        assert_eq!(bootstrap.payer_lightning, "lnd-router");
        assert_eq!(bootstrap.funding_sat, 10_000_000);
        assert_eq!(bootstrap.channel_sat, 2_000_000);
        assert_eq!(bootstrap.push_sat, 0);

        let channel = recipe_route_channel_request(request);
        assert_eq!(channel.chain, "bitcoin-core");
        assert_eq!(channel.from_lightning, "lnd-router");
        assert_eq!(channel.to_lightning, "cln-backend");
        assert_eq!(channel.channel_sat, 2_000_000);
        assert_eq!(channel.push_sat, 1_000_000);
    }

    #[test]
    fn recipe_setup_wire_contract_rejects_low_level_overrides() {
        let mut input = serde_json::json!({
            "instance_id": "instance",
            "experiment_id": "experiment",
            "lease_id": "lease",
            "operation_id": "operation",
            "recipe": "nutshell_lnd_cln_routing_fees",
            "idempotency_key": "once"
        });
        assert!(serde_json::from_value::<LabRecipeSetupRequest>(input.clone()).is_ok());
        input["chain"] = serde_json::json!("regtest");
        assert!(
            serde_json::from_value::<LabRecipeSetupRequest>(input).is_err(),
            "recipe setup must reject caller-controlled component IDs"
        );
    }

    #[test]
    fn recipe_fee_matrix_wire_contract_rejects_scientific_role_overrides() {
        let mut input = serde_json::json!({
            "instance_id": "instance",
            "experiment_id": "experiment",
            "lease_id": "lease",
            "matrix_id": "matrix",
            "recipe": "nutshell_lnd_cln_routing_fees",
            "idempotency_key": "once"
        });
        assert!(serde_json::from_value::<LabRecipeFeeMatrixRequest>(input.clone()).is_ok());
        input["payer_wallet"] = serde_json::json!("recipient-lnd");
        assert!(
            serde_json::from_value::<LabRecipeFeeMatrixRequest>(input).is_err(),
            "recipe matrix must reject caller-controlled wallet roles"
        );
    }

    #[test]
    fn generic_lab_plan_resolves_catalog_versions_kinds_and_bindings() {
        let request = LabPlanRequest {
            plan_id: "generic-plan".into(),
            components: vec![
                LabPlanComponentInput {
                    id: "chain".into(),
                    implementation: "bitcoin-core".into(),
                    version: None,
                    control: None,
                    config: BTreeMap::new(),
                },
                LabPlanComponentInput {
                    id: "lnd-a".into(),
                    implementation: "lnd".into(),
                    version: None,
                    control: None,
                    config: BTreeMap::new(),
                },
                LabPlanComponentInput {
                    id: "lnd-b".into(),
                    implementation: "lnd".into(),
                    version: None,
                    control: None,
                    config: BTreeMap::new(),
                },
            ],
            connections: vec![
                LabPlanConnectionInput::ChainBackend {
                    id: "a-chain".into(),
                    component: "lnd-a".into(),
                    chain: "chain".into(),
                    network: None,
                },
                LabPlanConnectionInput::ChainBackend {
                    id: "b-chain".into(),
                    component: "lnd-b".into(),
                    chain: "chain".into(),
                    network: None,
                },
                LabPlanConnectionInput::LightningPeer {
                    id: "direct".into(),
                    node_a: "lnd-a".into(),
                    node_b: "lnd-b".into(),
                },
            ],
            runtime_requirements: vec![],
            policy: LabPolicy::default(),
            idempotency_key: "generic-plan-once".into(),
        };
        let lab = compile_lab_plan(&request).expect("generic plan compiles");
        let validation = lab_validation_result(&lab);
        assert!(validation.valid, "{:#?}", validation.issues);
        assert_eq!(lab.components.len(), 3);
        assert!(
            lab.components
                .iter()
                .all(|component| component.version.is_some())
        );
        assert!(lab.components.iter().all(|component| {
            component.kind == ComponentKind::Bitcoin || component.kind == ComponentKind::Lightning
        }));
        assert!(lab.links[..2].iter().all(|link| {
            link.binding
                == Some(DependencyBinding::Chain {
                    network: BitcoinNetwork::Regtest,
                })
        }));
        assert!(lab.links[2].binding.is_none());
    }

    #[test]
    fn generic_lab_plan_infers_the_exact_catalog_payment_binding() {
        let request = LabPlanRequest {
            plan_id: "payment-plan".into(),
            components: vec![
                LabPlanComponentInput {
                    id: "mint".into(),
                    implementation: "nutshell".into(),
                    version: None,
                    control: None,
                    config: BTreeMap::new(),
                },
                LabPlanComponentInput {
        assert!(
            error.to_string().contains("network"),
            "unexpected diagnostic: {error}"
        );
                    implementation: "lnd".into(),
                    version: None,
                    control: None,
                    config: BTreeMap::new(),
                },
            ],
            connections: vec![LabPlanConnectionInput::PaymentBackend {
                id: "payment".into(),
                mint: "mint".into(),
                lightning: "backend".into(),
                method: None,
                unit: None,
            }],
            runtime_requirements: vec![],
            policy: LabPolicy::default(),
            idempotency_key: "payment-plan-once".into(),
        };
        let lab = compile_lab_plan(&request).expect("payment binding is unambiguous");
        assert_eq!(
            lab.links[0].binding,
            Some(DependencyBinding::Payment {
                method: PaymentMethod::Bolt11,
                unit: "sat".into(),
            })
        );
    }

    #[test]
    fn generic_lab_plan_keeps_implementation_ids_open_and_reports_catalog_alternatives() {
        let schema = serde_json::to_value(schemars::schema_for!(LabPlanComponentInput))
            .expect("plan component schema");
        assert!(
            schema["properties"]["implementation"].get("enum").is_none(),
            "catalog growth must not require an MCP schema change"
        );
        let request = LabPlanRequest {
            plan_id: "unknown-plan".into(),
            components: vec![LabPlanComponentInput {
                id: "future-mint".into(),
                implementation: "future-mint".into(),
                version: None,
                control: None,
                config: BTreeMap::new(),
            }],
            connections: vec![],
            runtime_requirements: vec![],
            policy: LabPolicy::default(),
            idempotency_key: "unknown-plan-once".into(),
        };
        let error = compile_lab_plan(&request).expect_err("unknown catalog entry must fail closed");
        let data = error.data.expect("structured plan error");
        assert_eq!(data["code"], "lab_plan_implementation_not_found");
        assert!(
            data["available_implementations"]
                .as_array()
                .is_some_and(|implementations| !implementations.is_empty())
        );
    }

    #[test]
    fn generic_lab_plan_rejects_unavailable_runtime_controls_before_storage() {
        let plan_schema =
            serde_json::to_value(schemars::schema_for!(LabPlanRequest)).expect("lab plan schema");
        assert!(
            plan_schema["required"].as_array().is_some_and(
                |required| required.contains(&serde_json::json!("runtime_requirements"))
            ),
            "runtime intent must not be silently omitted"
        );
        assert!(
            serde_json::from_value::<LabPlanRequest>(serde_json::json!({
                "plan_id": "omitted-runtime-intent",
                "components": [],
                "connections": [],
                "idempotency_key": "once"
            }))
            .is_err(),
            "the wire contract must reject omitted runtime requirements"
        );
        let requirement_schema =
            serde_json::to_value(schemars::schema_for!(LabPlanRuntimeRequirement))
                .expect("runtime requirement schema");
        assert!(
            requirement_schema["properties"]["endpoint"]
                .get("enum")
                .is_none(),
            "runtime endpoint growth must not require an MCP schema change"
        );
        assert!(
            requirement_schema["properties"]["controls"]["items"]
                .get("enum")
                .is_none(),
            "runtime control growth must not require an MCP schema change"
        );

        let request = LabPlanRequest {
            plan_id: "embedded-ldk-control-plan".into(),
            components: vec![LabPlanComponentInput {
                id: "mint".into(),
                implementation: "cdk-ldk".into(),
                version: None,
                control: None,
                config: BTreeMap::new(),
            }],
            connections: vec![],
            runtime_requirements: vec![LabPlanRuntimeRequirement {
                component: "mint".into(),
                endpoint: "ldk-node".into(),
                controls: BTreeSet::from(["channel_open".into(), "peer_connect".into()]),
            }],
            policy: LabPolicy::default(),
            idempotency_key: "embedded-ldk-control-plan-once".into(),
        };
        let error = compile_lab_plan(&request)
            .expect_err("an embedded endpoint without a control driver must fail closed");
        let message = error.message.to_string();
        assert!(message.contains("channel_open"));
        assert!(message.contains("peer_connect"));
        assert!(message.contains("no plan was stored"));
        let data = error.data.expect("structured runtime feasibility error");
        assert_eq!(data["code"], "lab_plan_runtime_control_unsupported");
        assert_eq!(data["endpoint"]["kind"], "lightning");
        assert!(
            data["endpoint"]["limitations"]
                .as_array()
                .is_some_and(|limitations| !limitations.is_empty())
        );
    }

    #[test]
    fn runtime_admission_rejects_a_catalog_unsupported_control_before_operation() {
        let catalog = default_catalog();
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "cdk-ldk")
            .expect("CDK-LDK catalog entry");
        let authored = LabSpec {
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
            policy: LabPolicy::default(),
        };
        let lab = proofstorm_core::resolve_effective_lab(&authored, catalog)
            .expect("effective runtime-admission fixture");
        let lock = proofstorm_core::resolve_lock(&lab, catalog).expect("fixture lock");
        let revision = PublishedRevision {
            workspace_id: "alpha".into(),
            digest: digest_json(&(&lab, &lock)),
            lab,
            lock,
        };

        require_component_runtime_control(&revision, "mint", "component", "wallet_initialize")
            .expect("declared runtime control");
        let error =
            require_component_runtime_control(&revision, "mint", "component", "wallet_fund")
                .expect_err("unsupported runtime control must fail before operation creation");
        assert!(error.message.contains("no operation was created"));
        assert_eq!(
            error.data.expect("structured runtime admission error")["code"],
            "runtime_control_unsupported"
        );
    }

    #[test]
    fn generic_plan_connections_name_endpoint_roles_in_the_wire_schema() {
        let encoded = serde_json::to_string(&schemars::schema_for!(LabPlanConnectionInput))
            .expect("plan connection schema");
        for role_name in [
            "node_a",
            "node_b",
            "component",
            "chain",
            "mint",
            "lightning",
            "database",
            "identity_provider",
            "source",
            "target",
        ] {
            assert!(
                encoded.contains(&format!("\"{role_name}\"")),
                "connection schema is missing semantic endpoint {role_name}"
            );
        }
        assert!(
            !encoded.contains("\"from\"") && !encoded.contains("\"to\""),
            "ambiguous dependency direction must not return to the planner contract"
        );
    }

    #[tokio::test]
    async fn generic_lab_plan_is_durable_and_apply_rejects_a_digest_mismatch_before_mutation() {
        let store = seeded_store();
        for capability in [
            Capability::CatalogRead,
            Capability::LabCreate,
            Capability::LabRead,
            Capability::LabPublish,
            Capability::LabMaterialize,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("generic planner grant");
        }
        let service = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("service");
        let request = LabPlanRequest {
            plan_id: "durable-plan".into(),
            components: vec![LabPlanComponentInput {
                id: "chain".into(),
                implementation: "bitcoin-core".into(),
                version: None,
                control: None,
                config: BTreeMap::new(),
            }],
            connections: vec![],
            runtime_requirements: vec![],
            policy: LabPolicy::default(),
            idempotency_key: "durable-plan-once".into(),
        };
        let first = service
            .proofstorm_lab_plan(Parameters(request.clone()))
            .expect("plan stored")
            .0;
        let replay = service
            .proofstorm_lab_plan(Parameters(request))
            .expect("plan replay")
            .0;
        assert_eq!(first, replay);
        let stored = store
            .read_draft("alpha", "designer", "durable-plan")
            .expect("durable plan can be read");
        assert_eq!(digest_json(&stored.lab), first.plan_digest);

        let result = service
            .proofstorm_lab_apply(Parameters(LabApplyRequest {
                plan_id: "durable-plan".into(),
                expected_plan_digest: "not-the-plan".into(),
                instance_id: "must-not-exist".into(),
                idempotency_key: "apply-mismatch".into(),
            }))
            .await;
        let Err(error) = result else {
            panic!("digest mismatch must fail before publication");
        };
        assert_eq!(
            error.data.expect("digest mismatch data")["code"],
            "lab_plan_digest_mismatch"
        );
        assert!(
            store
                .revision("alpha", "designer", &first.plan_digest)
                .is_err(),
            "digest mismatch must not publish the stored plan"
        );
    }

    #[test]
        assert!(
            error.to_string().contains("network"),
            "unexpected diagnostic: {error}"
        );
        let request = LabRecipeFeeMatrixRequest {
            instance_id: "instance-with-a-long-but-valid-identifier-01234567890123456789".into(),
            experiment_id: "experiment-with-a-long-but-valid-identifier-012345678901234567".into(),
            lease_id: "lease-with-a-long-but-valid-identifier-01234567890123456789012".into(),
            matrix_id: "matrix-with-a-long-but-valid-identifier-0123456789012345678901".into(),
            recipe: LabRecipe::NutshellLndClnRoutingFees,
            idempotency_key: "once".into(),
        };
        let prefix = recipe_fee_matrix_operation_prefix(&request);
        assert_eq!(prefix.len(), 23);
        assert!(prefix.starts_with("matrix-"));

        for suffix in [
            "init-recipient-lnd",
            "fund-payer-cln",
            "policy-above-reserve-cln",
            "baseline-above-reserve-lnd",
            "invoice-above-reserve-recipient-cln",
            "pay-above-reserve-cln-to-lnd",
            "oracle-above-reserve-cln",
        ] {
            let operation_id = format!("{prefix}-{suffix}");
            assert!(operation_id.len() <= 63, "{operation_id}");
            assert!(
                operation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            );
        }

        assert_eq!(prefix, recipe_fee_matrix_operation_prefix(&request));

        for direction in ROUTING_FEE_RECIPE_PAYMENT_DIRECTIONS {
            assert!(!direction.id.contains('_'));
            assert!(direction.label.contains('_'));
            let payment_id = format!("{prefix}-pay-above-reserve-{}", direction.id);
            let oracle_treatment_reference = format!("{prefix}-pay-above-reserve-{}", direction.id);
            assert_eq!(payment_id, oracle_treatment_reference);
            assert!(payment_id.len() <= 63);
        }
    }

    #[test]
    fn link_mutation_wire_contract_flattens_bindings_and_is_fail_closed() {
        let common = serde_json::json!({
            "draft_id": "draft",
            "expected_version": 1,
            "idempotency_key": "link-once"
        });

        let mut missing_network = common.clone();
        missing_network["link"] = serde_json::json!({
            "id": "chain-lnd",
            "kind": "chain_backend",
            "from": "lnd",
            "to": "chain"
        });
        let error = serde_json::from_value::<MutateLinkRequest>(missing_network)
            .expect_err("a chain backend without its flat network must fail at the wire boundary");
        assert!(error.to_string().contains("missing field `network`"));

        let mut peer_with_binding_field = common.clone();
        peer_with_binding_field["link"] = serde_json::json!({
            "id": "peer",
            "kind": "lightning_peer",
            "from": "left",
            "to": "right",
            "network": "regtest"
        });
        let error = serde_json::from_value::<MutateLinkRequest>(peer_with_binding_field)
            .expect_err("a peer link must not admit backend binding fields");
        assert!(error.to_string().contains("unknown field `network`"));

        let mut complete_payment = common.clone();
        complete_payment["link"] = serde_json::json!({
            "id": "mint-lnd",
            "kind": "payment_backend",
            "from": "mint",
            "to": "lnd",
            "method": "bolt11",
            "unit": "sat"
        });
        let request = serde_json::from_value::<MutateLinkRequest>(complete_payment)
            .expect("flat payment binding fields deserialize");
        let link = LinkSpec::try_from(request.link).expect("canonical payment binding");
        assert_eq!(
            link.binding,
            Some(DependencyBinding::Payment {
                method: PaymentMethod::Bolt11,
                unit: "sat".into()
            })
        );

        let bulk = serde_json::json!({
            "lab": {
                "api_version": API_VERSION,
                "name": "strict-bulk",
                "components": [],
                "links": [{
                    "id": "chain-lnd",
                    "kind": "chain_backend",
                    "from": "lnd",
                    "to": "chain"
                }],
                "policy": {"allow": [], "limits": {}}
            }
        });
        let error = serde_json::from_value::<ValidateLabRequest>(bulk)
            .expect_err("bulk lab backend links must require flat binding fields");
        assert!(
            error.to_string().contains("network"),
            "unexpected diagnostic: {error}"
        );

        let mut nested_binding = common.clone();
        nested_binding["link"] = serde_json::json!({
            "id": "chain-lnd",
            "kind": "chain_backend",
            "from": "lnd",
            "to": "chain",
            "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}
        });
        let error = serde_json::from_value::<MutateLinkRequest>(nested_binding)
            .expect_err("nested binding objects are excluded from the MCP wire contract");
        assert!(error.to_string().contains("unknown field `binding`"));

        let mut valid = common;
        valid["link"] = serde_json::json!({
            "id": "chain-lnd",
            "kind": "chain_backend",
            "from": "lnd",
            "to": "chain",
            "network": "regtest"
        });
        let request = serde_json::from_value::<MutateLinkRequest>(valid)
            .expect("a complete flat backend link is valid input");
        let link = LinkSpec::try_from(request.link).expect("canonical binding constructed");
        assert_eq!(link.kind, LinkKind::ChainBackend);
        assert!(matches!(
            link.binding,
            Some(DependencyBinding::Chain { .. })
        ));
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
        assert_eq!(designer.tool_names().len(), 20);
        assert!(
            !designer
                .tool_names()
                .contains(&"proofstorm_lab_edit".to_owned())
        );
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
        assert_eq!(oversized_limit.items.len(), 12);

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
        assert_eq!(service.tool_names().len(), 76);
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
        for required in [
            "proofstorm_component_restart",
            "proofstorm_component_exec_live",
            "proofstorm_component_forensics",
            "proofstorm_wallet_melt_quote_refresh",
        ] {
            assert!(
                service.tool_names().contains(&required.to_owned()),
                "{required} must be discoverable with its explicit capability"
            );
        }
        let authorized =
            ProofstormMcp::new(store.clone(), "alpha", "designer").expect("authorized session");
        assert!(
            authorized
                .tool_names()
                .contains(&"proofstorm_authentication_conformance".to_owned())
        );
            encoded.len() < 240 * 1024,
            authorized
                .tool_names()
                .contains(&"proofstorm_authentication_protected_spend".to_owned())
        );
            (ProofstormToolset::Experiment, 145 * 1024),
            !authorized
            (ProofstormToolset::Runtime, 200 * 1024),
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
                .contains(&"proofstorm_component_exec_live".to_owned())
                draft_id: "compact-draft".into(),
                lab: authored_lab("compact-draft"),
                idempotency_key: "create-compact-draft".into(),
            }))
            .expect("create draft")
            .0;
        assert_eq!(receipt.draft_id, "compact-draft");
        assert_eq!(receipt.version, 1);
        assert_eq!(receipt.component_count, 0);
        assert_eq!(receipt.link_count, 0);
        assert_eq!(
            receipt.structure,
            "components=[]; links=[]; backend_bindings=0/0"
        );
        assert!(receipt.topology_digest.starts_with("sha256:"));
        assert!(receipt.valid);
        assert!(receipt.warnings[0].starts_with("empty_topology:"));
        assert_eq!(receipt.changed_paths, ["/"]);
            "proofstorm_component_restart",
            "proofstorm_component_exec_live",
        let encoded = serde_json::to_string(&receipt).expect("serialize receipt");
            "proofstorm_wallet_melt_quote_refresh",
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
    fn topology_receipts_expose_stable_identities_and_binding_coverage() {
        let mut authored = serde_json::from_value::<LabSpec>(serde_json::json!({
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
                    "version": "30.0",
                    "config_version": "bitcoin-core/30/v1",
                    "control": "laboratory",
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
        .expect("typed lab");
        let first = lab_validation_result(&authored);
        assert_eq!(first.component_ids, ["chain", "lnd"]);
        assert_eq!(first.link_ids, ["lnd-chain"]);
        assert!(first.warnings.is_empty());

        let first_digest = topology_summary(&authored).topology_digest;
        authored.components.reverse();
        authored.links.reverse();
        let reordered = lab_validation_result(&authored);
        assert_eq!(first.component_ids, reordered.component_ids);
        assert_eq!(first_digest, topology_summary(&authored).topology_digest);
    }

    #[test]
    fn wallet_funding_rejects_the_mints_own_payment_backend_with_alternatives() {
        let authored = serde_json::from_value::<LabSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "funding-admission",
            "components": [
                {"id":"mint","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"backend-lnd","kind":"lightning","implementation":"lnd","version":"0.20","config_version":"lnd/0.20/v1","control":"laboratory","config":{}},
                {"id":"router-lnd","kind":"lightning","implementation":"lnd","version":"0.20","config_version":"lnd/0.20/v1","control":"laboratory","config":{}}
            ],
            "links": [{"id":"pay","kind":"payment_backend","from":"mint","to":"backend-lnd","binding":{"type":"payment","method":"bolt11","unit":"sat"}}],
            "policy": {}
    fn component_execution_modes_are_hidden_without_their_distinct_capabilities() {
        .expect("typed lab");

        let error = validate_wallet_fund_payer(&authored, "mint", "backend-lnd")
            .expect_err("a backend cannot pay its own invoice");
        let data = error.data.expect("structured admission error");
        assert_eq!(data["code"], "self_payment_unsupported");
                .contains(&"proofstorm_component_exec_live".to_owned())
        );
        assert!(
            !restricted
                .tool_names()
                .contains(&"proofstorm_component_forensics".to_owned())
        validate_wallet_fund_payer(&authored, "mint", "router-lnd")
            .expect("a distinct LND payer is accepted");
    }
            .grant("alpha", "designer", Capability::ComponentExecLive)
            .expect("live exec grant");
        let live = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("live session");
        clippy::too_many_lines,
            live.tool_names()
                .contains(&"proofstorm_component_exec_live".to_owned())
        );
        assert!(
            !live
    )]
                .contains(&"proofstorm_component_forensics".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::ComponentForensics)
            .expect("forensics grant");
        let both = ProofstormMcp::new(store, "alpha", "designer").expect("execution session");
        assert!(
            both.tool_names()
                .contains(&"proofstorm_component_forensics".to_owned())
        let operation = |id: &str,
                         sequence: u64,
                         kind: OperationKind,
                         request: serde_json::Value,
                         artifact: serde_json::Value| LabOperation {
            id: id.into(),
            workspace_id: "alpha".into(),
            instance_id: "instance".into(),
            experiment_id: "experiment".into(),
            lease_id: "lease".into(),
            principal_id: "designer".into(),
            sequence,
            kind,
            capability: Capability::WalletControl,
            resource_name: format!("resource-{id}"),
            request_digest: format!("sha256:{id}"),
            request,
            phase: OperationPhase::Succeeded,
            accepted_at_unix: 1,
            started_at_unix: Some(2),
            completed_at_unix: Some(3),
            artifact: Some(OperationArtifact {
                media_type: "application/json".into(),
                digest: format!("sha256:artifact-{id}"),
                byte_length: 1,
                content: artifact,
            }),
        };
        let baseline = operation(
            "balance-before",
            10,
            OperationKind::WalletBalance,
            serde_json::json!({"wallet":"wallet", "mint":"mint"}),
            serde_json::json!({"balance_sat": 19_998}),
        );
        let treatment = operation(
            "high-fee-pay",
            11,
            OperationKind::WalletPay,
            serde_json::json!({"wallet":"wallet", "mint":"mint"}),
            serde_json::json!({
                "payer_balance_sat": 19_998,
                "input_fee_sat": 0,
                "input_proof_count": 0,
                "quote_observations": [{
                    "role": "payment_melt",
                    "wallet_id": "wallet",
                    "mint_id": "mint",
                    "state": "UNPAID",
                    "amount_sat": 1_000,
                    "fee_paid_sat": 0
                }]
            }),
        );
        let request = ConservationOracleRequest {
            instance_id: "instance".into(),
            experiment_id: "experiment".into(),
            lease_id: "lease".into(),
            operation_id: "conservation".into(),
            wallet: "wallet".into(),
            mint: "mint".into(),
            baseline_operation_id: "balance-before".into(),
            treatment_operation_id: "high-fee-pay".into(),
            idempotency_key: "conservation".into(),
        };

        let evidence =
            conservation_observation(&request, &baseline, &treatment, "alpha", "designer")
                .expect("valid anchored conservation request");
        assert_eq!(evidence["baseline_sat"], 19_998);
        assert_eq!(evidence["expected_sat"], 19_998);
        assert_eq!(evidence["actual_sat"], 19_998);
        assert_eq!(evidence["conserved"], true);

        let mut treatment_before_baseline = treatment.clone();
        treatment_before_baseline.sequence = 9;
        let error = conservation_observation(
            &request,
            &baseline,
            &treatment_before_baseline,
            "alpha",
            "designer",
        )
        .expect_err("treatment must follow the balance baseline");
        assert_eq!(
            error.data.expect("coded error")["code"],
            "conservation_treatment_invalid"
        );

        let mut round_trip = treatment_before_baseline;
        round_trip.sequence = 11;
        round_trip.kind = OperationKind::WalletRoundTrip;
        let error = conservation_observation(&request, &baseline, &round_trip, "alpha", "designer")
            .expect_err("a value-minting round trip is not a balance-invariance treatment");
        assert_eq!(
            error.data.expect("coded error")["code"],
            "conservation_treatment_invalid"
        );

        let mut paid = treatment;
        paid.artifact.as_mut().expect("paid artifact").content = serde_json::json!({
            "payer_balance_sat": 18_996,
            "input_fee_sat": 1,
            "input_proof_count": 1,
            "quote_observations": [{
                "role": "payment_melt",
                "wallet_id": "wallet",
                "mint_id": "mint",
                "state": "PAID",
                "amount_sat": 1_000,
                "fee_paid_sat": 1
            }]
        });
        let evidence = conservation_observation(&request, &baseline, &paid, "alpha", "designer")
            .expect("paid debit evidence");
        assert_eq!(evidence["input_fee_sat"], 1);
        assert_eq!(evidence["input_proof_count"], 1);
        assert_eq!(evidence["expected_sat"], 18_996);
        assert_eq!(evidence["actual_sat"], 18_996);
        assert_eq!(evidence["conserved"], true);
            encoded.len() < 240 * 1024,
        let mut incomplete = paid;
        incomplete
            .artifact
            .as_mut()
            (ProofstormToolset::Experiment, 145 * 1024),
            .content
            (ProofstormToolset::Runtime, 200 * 1024),
            .expect("artifact object")
            .remove("input_fee_sat");
        let error = conservation_observation(&request, &baseline, &incomplete, "alpha", "designer")
            .expect_err("missing exact input-fee evidence must fail closed");
        assert_eq!(
            error.data.expect("coded error")["code"],
            "conservation_treatment_artifact_invalid"
        );
    }

    #[test]
    fn topology_receipt_warns_when_a_direct_backend_link_bypasses_a_router() {
        let authored = serde_json::from_value::<LabSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "routing-hazard",
            "components": [
                {"id":"mint-a","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"mint-b","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                .contains(&"proofstorm_component_exec_live".to_owned())
                {"id":"backend-b","kind":"lightning","implementation":"cln","version":"26.06","config_version":"cln/26.06/v1","control":"laboratory","config":{}},
                {"id":"router","kind":"lightning","implementation":"lnd","version":"0.20","config_version":"lnd/0.20/v1","control":"laboratory","config":{}}
            ],
            "links": [
                {"id":"pay-a","kind":"payment_backend","from":"mint-a","to":"backend-a","binding":{"type":"payment","method":"bolt11","unit":"sat"}},
                {"id":"pay-b","kind":"payment_backend","from":"mint-b","to":"backend-b","binding":{"type":"payment","method":"bolt11","unit":"sat"}},
                {"id":"direct","kind":"lightning_peer","from":"backend-a","to":"backend-b"}
            ],
            "policy": {}
        }))
        .expect("typed lab");
        let summary = topology_summary(&authored);
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.starts_with("direct_mint_backend_peer:"))
        );
            "proofstorm_component_restart",
            "proofstorm_component_exec_live",
    }

    #[test]
    fn topology_receipt_warns_when_cross_mint_work_has_only_one_wallet() {
        let authored = serde_json::from_value::<LabSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "wallet-hazard",
            "components": [
                {"id":"mint-a","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"mint-b","kind":"mint","implementation":"nutshell","version":"0.20.3","config_version":"nutshell-mint/0.20/v1","control":"target","config":{}},
                {"id":"wallet","kind":"wallet","implementation":"nutshell-wallet","version":"0.20.3","config_version":"nutshell-wallet/0.20/v1","control":"laboratory","config":{}}
            ],
            "links": [],
            "policy": {}
        }))
        .expect("typed lab");
        let summary = topology_summary(&authored);
        assert!(summary.warnings.iter().any(|warning| {
            warning.starts_with("distinct_payment_wallets_required:")
                && warning.contains("bidirectional cross-mint wallet_pay")
        }));
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
        assert_eq!(service.tool_names().len(), 73);
        assert!(
            !service
                .tool_names()
                .contains(&"proofstorm_lab_edit".to_owned()),
            "whole-document replacement is not an agent tool"
        );
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
            service
                .tool_names()
                .contains(&"proofstorm_channel_policy_set".to_owned()),
            "routing policy is a first-class typed runtime operation"
        );
        assert!(
            encoded.len() < 218 * 1024,
            "fully authorized tool discovery is {} bytes",
            encoded.len()
        );
        for (toolset, maximum) in [
            (ProofstormToolset::Experiment, 125 * 1024),
            (ProofstormToolset::Design, 100 * 1024),
            (ProofstormToolset::Runtime, 180 * 1024),
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
    fn component_execution_modes_are_hidden_without_their_distinct_capabilities() {
        let design = service.clone().with_toolset(ProofstormToolset::Design);
        assert!(
            !design
                .tool_names()
                .contains(&"proofstorm_component_exec".to_owned())
        );
                .contains(&"proofstorm_component_exec_live".to_owned())
        );
        assert!(
            !restricted
                .tool_names()
                .contains(&"proofstorm_component_forensics".to_owned())
        assert!(
            !evidence
                .tool_names()
            .grant("alpha", "designer", Capability::ComponentExecLive)
            .expect("live exec grant");
        let live = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("live session");

            live.tool_names()
                .contains(&"proofstorm_component_exec_live".to_owned())
        );
        assert!(
            !live
    fn experiment_toolset_is_generic_and_one_session_capable() {
                .contains(&"proofstorm_component_forensics".to_owned())
        );

        store
            .grant("alpha", "designer", Capability::ComponentForensics)
            .expect("forensics grant");
        let both = ProofstormMcp::new(store, "alpha", "designer").expect("execution session");
        assert!(
            both.tool_names()
                .contains(&"proofstorm_component_forensics".to_owned())
            "proofstorm_catalog_list",
            "proofstorm_lab_plan",
            "proofstorm_lab_apply",
            "proofstorm_liquidity_bootstrap",
            "proofstorm_channel_open",
            "proofstorm_channel_policy_set",
            "proofstorm_wallet_pay",
            "proofstorm_network_partition",
            "proofstorm_authentication_replay",
            "proofstorm_operation_wait_many",
            "proofstorm_artifact_export",
            "proofstorm_lab_close",
        ] {
            assert!(
                experiment_tool(required),
                "experiment workflow is missing {required}"
            );
        }
        for recipe_specific_or_unbounded in [
            "proofstorm_lab_create",
            "proofstorm_lab_validate",
            "proofstorm_component_exec",
            "proofstorm_lab_recipe_create",
            "proofstorm_lab_recipe_bootstrap",
            "proofstorm_lab_recipe_route_channel_open",
            "proofstorm_lab_recipe_fee_matrix_run",
            "proofstorm_wallet_round_trip",
            "proofstorm_lab_clone",
        ] {
            assert!(
                !experiment_tool(recipe_specific_or_unbounded),
                "generic experiment workflow should not expose {recipe_specific_or_unbounded}"
            );
        }
    }

    #[test]
    fn conservation_contract_has_no_caller_controlled_tolerance() {
        let schema = serde_json::to_value(schemars::schema_for!(ConservationOracleRequest))
            .expect("conservation schema");
        assert!(
            !schema.to_string().contains("tolerance_sat"),
            "exact conservation must not expose caller-controlled slack"
        );
    }

    #[test]
    fn channel_policy_agent_contract_uses_satoshis() {
        let schema = serde_json::to_value(schemars::schema_for!(ChannelPolicySetRequest))
            .expect("channel policy schema");
        let rendered = schema.to_string();
        assert!(rendered.contains("base_fee_sat"));
        assert!(!rendered.contains("base_fee_msat"));
        assert!(rendered.contains("100000"));
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
        let valid_batch = OperationWaitManyRequest {
            operation_ids: vec!["operation-a".into(), "operation-b".into()],
            timeout_seconds: 120,
        };
        assert!(validate_operation_wait_many_request(&valid_batch).is_ok());
        for operation_ids in [
            Vec::new(),
            vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into(),
                "f".into(),
                "g".into(),
                "h".into(),
                "i".into(),
            ],
        ] {
            let error = validate_operation_wait_many_request(&OperationWaitManyRequest {
                operation_ids,
                timeout_seconds: 30,
            })
            .expect_err("batch size must refuse");
            assert_eq!(
                error.data.expect("structured batch error")["code"],
                "operation_wait_many_count_invalid"
            );
        }
        let duplicate_error = validate_operation_wait_many_request(&OperationWaitManyRequest {
            operation_ids: vec!["same".into(), "same".into()],
            timeout_seconds: 30,
        })
        .expect_err("duplicate IDs must refuse");
        assert_eq!(
            duplicate_error.data.expect("structured duplicate error")["code"],
            "operation_wait_many_duplicate_id"
        );
        let schema = serde_json::to_string(&schemars::schema_for!(OperationWaitManyRequest))
            .expect("batch wait schema");
        assert!(schema.contains("\"minItems\":1"));
        assert!(schema.contains("\"maxItems\":8"));
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
        assert!(
            !restricted
                .tool_names()
                .contains(&"proofstorm_operation_wait_many".to_owned())
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
        assert!(
            authorized
                .tool_names()
                .contains(&"proofstorm_operation_wait_many".to_owned())
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
        let receipt = compact_lab_status(status.clone());
        assert_eq!(receipt.total_components, 0);
        assert_eq!(receipt.inventory_count, 1);
        assert!(receipt.inventory_digest.starts_with("sha256:"));
        assert!(
            receipt
                .runtime_guidance
                .as_deref()
                .is_some_and(|guidance| guidance.contains("liquidity_bootstrap"))
        );
        let encoded = serde_json::to_string(&receipt).expect("status receipt");
        assert!(!encoded.contains("\"components\":["));
        assert!(!encoded.contains("inventory\":"));
        assert!(!encoded.contains("secret-routing-key"));
        assert!(serialized_size(&receipt).expect("status size") < 1024);

        let close_receipt = compact_lab_wait(status, InstancePhase::Closed, false, false);
        assert_eq!(close_receipt.phase, InstancePhase::Ready);
        assert_eq!(close_receipt.target_phase, InstancePhase::Closed);
        assert!(!close_receipt.reached);
        let encoded = serde_json::to_string(&close_receipt).expect("close receipt");
        assert!(!encoded.contains("\"components\":["));
        assert!(!encoded.contains("inventory\":"));
        assert!(!encoded.contains("secret-routing-key"));
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

    #[test]
    fn artifact_export_agent_schema_cannot_request_bulk_content() {
        let schema = schemars::schema_for!(ArtifactExportRequest);
        let encoded = serde_json::to_string(&schema).expect("artifact export schema");
        assert!(!encoded.contains("include_content"));
        assert!(encoded.contains("artifact_operation_ids"));
        assert!(encoded.contains("\"maxItems\":16"));
        assert!(encoded.contains("Do not enumerate"));
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
            lab: authored_lab("refused"),
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
            Capability::ExperimentRead,
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
    #[allow(
        clippy::too_many_lines,
        reason = "the admission fixture covers missing, pending, failed-safe, funded, and unfunded workflow states"
    )]
    fn peer_and_channel_admission_require_a_succeeded_bootstrap() {
        let store = seeded_store();
        for capability in [
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::LeaseAcquire,
            Capability::WalletFund,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("runtime prerequisite grant");
        }
        store
            .create_draft(
                "alpha",
                "designer",
                "runtime-lab",
                &lab("runtime-lab"),
                "create-runtime-lab",
            )
            .expect("draft");
        let revision = store
            .publish("alpha", "designer", "runtime-lab", 1, "publish-runtime-lab")
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "runtime-instance",
                &revision.digest,
                "materialize-runtime-lab",
            )
            .expect("instance");
        store
            .create_experiment(
                "alpha",
                "designer",
                "runtime-experiment",
                "runtime-instance",
                "create-runtime-experiment",
            )
            .expect("experiment");
        store
            .acquire_lease(
                "alpha",
                "designer",
                "runtime-experiment",
                "runtime-lease",
                300,
                2,
                "acquire-runtime-lease",
            )
            .expect("lease");
        let service = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
        let error = service
            .require_liquidity_bootstrap("runtime-experiment", "runtime-instance")
            .expect_err("missing bootstrap must fail closed");
        assert_eq!(
            error.data.expect("missing bootstrap data")["code"],
            "runtime_initialization_required"
        );

        let bootstrap = store
            .create_operation(
                "alpha",
                "designer",
                "runtime-instance",
                "runtime-experiment",
                "runtime-lease",
                "runtime-bootstrap",
                OperationKind::BootstrapLiquidity,
                &serde_json::json!({
                    "instance_id": "runtime-instance",
                    "experiment_id": "runtime-experiment",
                    "lease_id": "runtime-lease",
                    "operation_id": "runtime-bootstrap",
                    "chain": "chain",
                    "mint_lightning": "mint-lnd",
                    "payer_lightning": "router-lnd",
                    "funding_sat": 1_000_000,
                    "channel_sat": 500_000,
                    "push_sat": 250_000
                }),
                "create-runtime-bootstrap",
                Capability::WalletFund,
            )
            .expect("bootstrap operation");
        let error = service
            .require_liquidity_bootstrap("runtime-experiment", "runtime-instance")
            .expect_err("pending bootstrap must fail closed");
        assert_eq!(
            error.data.expect("pending bootstrap data")["code"],
            "runtime_initialization_in_progress"
        );
        store
            .record_operation_result(
                "alpha",
                &bootstrap.id,
                OperationPhase::Succeeded,
                serde_json::json!({"ready": true}),
            )
            .expect("bootstrap result");
        let succeeded = service
            .require_liquidity_bootstrap("runtime-experiment", "runtime-instance")
            .expect("succeeded bootstrap unlocks peer/channel actions");
        let channel_request = |from: &str, channel_sat| ChannelOpenRequest {
            instance_id: "runtime-instance".into(),
            experiment_id: "runtime-experiment".into(),
            lease_id: "runtime-lease".into(),
            operation_id: "runtime-channel".into(),
            chain: "chain".into(),
            from_lightning: from.into(),
            to_lightning: "cln-mint".into(),
            channel_sat,
            push_sat: channel_sat / 2,
            idempotency_key: "runtime-channel-key".into(),
        };
        let error =
            validate_channel_funding_admission(&channel_request("router-lnd", 500_000), &succeeded)
                .expect_err("a second channel cannot consume the bootstrap fee margin");
        let data = error.data.expect("funding admission data");
        assert_eq!(data["code"], "insufficient_channel_funding_margin");
        assert_eq!(data["safe_max_channel_sat"], 490_000);
        validate_channel_funding_admission(&channel_request("router-lnd", 400_000), &succeeded)
            .expect("a channel below the safe remaining budget is admitted");
        let error =
            validate_channel_funding_admission(&channel_request("cln-mint", 20_000), &succeeded)
                .expect_err("an unfunded channel source must be rejected before runtime");
        assert_eq!(
            error.data.expect("source admission data")["code"],
            "channel_funding_source_unproven"
        );
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
            Capability::LeaseAcquire,
            Capability::LeaseRelease,
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
                "finalization-lab",
                &lab("finalization-lab"),
                "create-finalization-lab",
            )
            .expect("draft");
        let revision = store
            .publish(
                "alpha",
                "designer",
                "finalization-lab",
                1,
                "publish-finalization-lab",
            )
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "finalization-instance",
                &revision.digest,
                "materialize-finalization-lab",
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
            .proofstorm_experiment_close(Parameters(CloseExperimentRequest {
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
            .acquire_lease(
                "alpha",
                "designer",
                "active-finalization",
                "active-finalization-lease",
                300,
                1,
                "acquire-active-finalization-lease",
            )
            .expect("lease");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "finalization-instance",
                "active-finalization",
                "active-finalization-lease",
                "active-finalization-balance",
                OperationKind::WalletBalance,
                &serde_json::json!({"wallet": "wallet", "mint": "mint"}),
                "create-active-finalization-balance",
                Capability::WalletControl,
            )
            .expect("active operation");
        store
            .release_lease(
                "alpha",
                "designer",
                "active-finalization-lease",
                "release-active-finalization-lease",
            )
            .expect("release lease");
        let Err(error) = service
            .proofstorm_experiment_close(Parameters(CloseExperimentRequest {
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
            .proofstorm_experiment_close(Parameters(CloseExperimentRequest {
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
            Capability::LeaseAcquire,
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
                "artifact-contract-lab",
                &lab("artifact-contract-lab"),
                "create-artifact-contract-lab",
            )
            .expect("draft");
        let revision = store
            .publish(
                "alpha",
                "designer",
                "artifact-contract-lab",
                1,
                "publish-artifact-contract-lab",
            )
            .expect("revision");
        store
            .materialize(
                "alpha",
                "designer",
                "artifact-contract-instance",
                &revision.digest,
                "materialize-artifact-contract-lab",
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
        store
            .acquire_lease(
                "alpha",
                "designer",
                "artifact-contract-experiment",
                "artifact-contract-lease",
                300,
                1,
                "acquire-artifact-contract-lease",
            )
            .expect("lease");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "artifact-contract-instance",
                "artifact-contract-experiment",
                "artifact-contract-lease",
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
