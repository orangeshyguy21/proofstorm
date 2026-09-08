use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ComponentKind, ComponentSpec, LinkSpec, LockEntry};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ComponentConditionType {
    WorkloadReady,
    StorageReady,
    CredentialsReady,
    ServiceReady,
    ProtocolReady,
    DependenciesReady,
    ComponentReady,
    ExperimentControllable,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ComponentConditionState {
    True,
    False,
    Unknown,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ComponentConditionReason {
    Observed,
    NotObserved,
    StaleRevision,
    WorkloadAvailable,
    WorkloadUnavailable,
    ImagePullFailed,
    ImagePullBackoff,
    InvalidImageName,
    ContainerConfigError,
    ContainerCrashLoop,
    ContainerStartError,
    ContainerExited,
    PodUnschedulable,
    StorageBound,
    StoragePending,
    CredentialsProjected,
    CredentialsMissing,
    EndpointsReady,
    EndpointsMissing,
    ProtocolResponding,
    ProtocolProbePending,
    ProtocolProbeFailed,
    DependenciesSatisfied,
    DependenciesUnsatisfied,
    ComponentOperational,
    ComponentNotOperational,
    IntentionallyStopped,
    ControlAvailable,
    ControlUnavailable,
}

impl ComponentConditionReason {
    /// Startup is failing; repeated waiting alone is not a recovery strategy.
    #[must_use]
    pub const fn blocks_startup(self) -> bool {
        matches!(
            self,
            Self::ImagePullFailed
                | Self::ImagePullBackoff
                | Self::InvalidImageName
                | Self::ContainerConfigError
                | Self::ContainerCrashLoop
                | Self::ContainerStartError
                | Self::ContainerExited
                | Self::PodUnschedulable
        )
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationClass {
    Inspect,
    Start,
    Stop,
    Restart,
    NativeExec,
    NetworkHeal,
    PeerChannelMutation,
    WalletPayment,
    Authentication,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessPrerequisite {
    AcceptedIdentity,
    Storage,
    WorkloadIdentity,
    ExecutionContext,
    TargetDescriptor,
    FaultIdentity,
    Dependencies,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationAdmissionContract {
    pub operation: OperationClass,
    pub prerequisites: BTreeSet<ReadinessPrerequisite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionAggregationContract {
    pub condition: ComponentConditionType,
    pub all_of: BTreeSet<ComponentConditionType>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadControllerKind {
    Deployment,
    StatefulSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum StorageRequirementTemplate {
    StatefulClaimTemplate { template_name: String },
    ComponentClaim { suffix: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkloadObservationContract {
    pub kind: WorkloadControllerKind,
    pub name: String,
    pub desired_replicas: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageObservationContract {
    pub claim_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkedStateObservationContract {
    pub component_id: String,
    pub state_contract: String,
    pub storage: Vec<StorageObservationContract>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialObservationContract {
    pub identity: String,
    pub source_component_id: String,
    pub source_state_contract: String,
    pub claim_name: String,
    pub mount_name: String,
    pub mount_path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolProbeContract {
    Tcp { port_name: String },
    HttpGet { port_name: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolProbePlan {
    Tcp { port: u16 },
    HttpGet { port: u16, path: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum ConfigDefault {
    Literal(Value),
    ComponentId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValueKind {
    Boolean,
    Number,
    Integer,
    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSettingClass {
    AgentAuthorable,
    TopologyDerived,
    GeneratedInstanceSecret,
    ImportedSecretReference,
    RuntimePolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigFieldContract {
    pub description: String,
    pub value_kind: ConfigValueKind,
    pub classification: ConfigSettingClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ConfigDefault>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigRule {
    MutuallyExclusive {
        fields: BTreeSet<String>,
    },
    RequiredWhen {
        field: String,
        equals: Value,
        required_field: String,
    },
    LessThanOrEqual {
        minimum_field: String,
        maximum_field: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentBackendContract {
    pub id: String,
    pub kind: ComponentKind,
    pub config_version: String,
    pub config_fields: BTreeMap<String, ConfigFieldContract>,
    pub config_rules: Vec<ConfigRule>,
    pub service_ports: BTreeMap<String, u16>,
    pub execution_state_contract: String,
    pub execution_mounts: Vec<ExecutionMountTemplateContract>,
    pub execution_environment: BTreeMap<String, String>,
    pub workload_kind: WorkloadControllerKind,
    pub storage_requirements: Vec<StorageRequirementTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_probe: Option<ProtocolProbeContract>,
    pub applicable_conditions: BTreeSet<ComponentConditionType>,
    pub condition_reasons: BTreeMap<ComponentConditionType, BTreeSet<ComponentConditionReason>>,
    pub condition_aggregation: Vec<ConditionAggregationContract>,
    pub operation_admission: Vec<OperationAdmissionContract>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContextContract {
    pub component_id: String,
    pub image: String,
    pub state_contract: String,
    pub mounts: Vec<ExecutionMountContract>,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ExecutionStorageTemplateSource {
    StatefulData,
    ComponentPersistentData,
    ComponentConfig,
    LinkedStatefulData {
        link_kind: crate::LinkKind,
        binding: crate::DependencyBinding,
        target_implementation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ExecutionStorageSource {
    StatefulData,
    ComponentPersistentData,
    ComponentConfig,
    LinkedStatefulData { link_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionMountTemplateContract {
    pub name: String,
    pub mount_path: String,
    pub read_only: bool,
    #[serde(default)]
    pub requirement: ExecutionMountRequirement,
    pub source: ExecutionStorageTemplateSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionMountRequirement {
    #[default]
    Required,
    AtLeastOne {
        group: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionMountContract {
    pub name: String,
    pub mount_path: String,
    pub read_only: bool,
    pub source: ExecutionStorageSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetDescriptorContract {
    pub component_id: String,
    pub kind: ComponentKind,
    pub backend_id: String,
    pub version: String,
    pub ports: BTreeMap<String, u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentPlanInput {
    pub instance_key: String,
    pub revision_digest: String,
    pub component: ComponentSpec,
    pub lock: LockEntry,
    pub relevant_links: Vec<LinkSpec>,
    /// Target descriptors keyed by stable `LinkSpec::id`, not component ID.
    pub linked_targets: BTreeMap<String, TargetDescriptorContract>,
    /// Linked state contracts keyed by stable `LinkSpec::id`.
    pub linked_state: BTreeMap<String, LinkedStateObservationContract>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BitcoinCoreConfig {
    pub txindex: bool,
    pub fallback_fee: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LndConfig {
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClnConfig {
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CdkMintConfig {
    pub name: String,
    pub description: String,
    pub description_long: String,
    pub motd: String,
    pub icon_url: String,
    pub contact_email: String,
    pub contact_nostr_public_key: String,
    pub tos_url: String,
    pub enable_info_page: bool,
    pub input_fee_ppk: u64,
    pub use_keyset_v2: bool,
    pub mint_quote_ttl_seconds: u64,
    pub melt_quote_ttl_seconds: u64,
    pub http_cache_ttl_seconds: u64,
    pub http_cache_tti_seconds: u64,
    pub max_inputs: u64,
    pub max_outputs: u64,
    pub min_mint_sat: u64,
    pub max_mint_sat: u64,
    pub min_melt_sat: u64,
    pub max_melt_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these booleans are independent upstream Nutshell policy settings, not one state machine"
)]
pub struct NutshellMintConfig {
    pub name: String,
    pub description: String,
    pub description_long: String,
    pub motd: String,
    pub icon_url: String,
    pub contact_email: String,
    pub contact_nostr_public_key: String,
    pub tos_url: String,
    pub input_fee_ppk: u64,
    pub mint_quote_ttl_seconds: u64,
    pub melt_quote_ttl_seconds: u64,
    pub max_secret_length: u64,
    pub max_witness_length: u64,
    pub max_request_length: u64,
    pub max_mint_sat: u64,
    pub max_melt_sat: u64,
    pub max_balance_sat: u64,
    pub disable_mint: bool,
    pub disable_melt: bool,
    pub rate_limit: bool,
    pub rate_limit_proxy_trust: bool,
    pub global_rate_limit_per_minute: u64,
    pub transaction_rate_limit_per_minute: u64,
    pub quote_backend_check_rate_limit_seconds: u64,
    pub oidc_discovery_url: String,
    pub oidc_client_id: String,
    pub auth_rate_limit_per_minute: u64,
    pub auth_max_blind_tokens: u64,
    pub lightning_fee_percent: f64,
    pub lightning_reserve_fee_min_sat: u64,
    pub clnrest_enable_mpp: bool,
    pub lnd_enable_mpp: bool,
    pub redis_cache_ttl_seconds: u64,
    pub watchdog_enabled: bool,
    pub watchdog_balance_check_interval_seconds: u64,
    pub database_lock_timeout_ms: u64,
    pub regular_tasks_interval_seconds: u64,
    pub websocket_read_timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresConfig {
    pub database_name: String,
    pub storage_size: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedisConfig {
    pub maxmemory_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeycloakConfig {
    pub access_token_lifespan_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "implementation", content = "config")]
pub enum EffectiveComponentConfig {
    #[serde(rename = "bitcoin-core")]
    BitcoinCore(BitcoinCoreConfig),
    #[serde(rename = "lnd")]
    Lnd(LndConfig),
    #[serde(rename = "cln")]
    Cln(ClnConfig),
    #[serde(rename = "cdk")]
    Cdk(CdkMintConfig),
    #[serde(rename = "cdk-ldk")]
    CdkLdk(CdkMintConfig),
    #[serde(rename = "cdk-bdk")]
    CdkBdk(CdkMintConfig),
    #[serde(rename = "nutshell")]
    Nutshell(NutshellMintConfig),
    #[serde(rename = "postgresql")]
    Postgres(PostgresConfig),
    #[serde(rename = "redis")]
    Redis(RedisConfig),
    #[serde(rename = "keycloak")]
    Keycloak(KeycloakConfig),
    #[serde(rename = "nutshell-wallet")]
    NutshellWallet,
    #[serde(rename = "cdk-cli-wallet")]
    CdkCliWallet,
    #[serde(rename = "cocod-wallet")]
    CocodWallet,
    #[serde(rename = "attacker-workspace")]
    AttackerWorkspace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentPlanContract {
    pub instance_key: String,
    pub revision_digest: String,
    pub backend_id: String,
    pub component_id: String,
    pub kind: ComponentKind,
    pub rollout_digest: String,
    pub effective_config: EffectiveComponentConfig,
    pub relevant_links: Vec<LinkSpec>,
    /// Target descriptors keyed by stable `LinkSpec::id`, not component ID.
    pub linked_targets: BTreeMap<String, TargetDescriptorContract>,
    pub execution_context: ExecutionContextContract,
    pub target_descriptor: TargetDescriptorContract,
    pub workload: WorkloadObservationContract,
    pub storage: Vec<StorageObservationContract>,
    pub credentials: Vec<CredentialObservationContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_probe: Option<ProtocolProbePlan>,
    pub applicable_conditions: BTreeSet<ComponentConditionType>,
    pub condition_reasons: BTreeMap<ComponentConditionType, BTreeSet<ComponentConditionReason>>,
    pub condition_aggregation: Vec<ConditionAggregationContract>,
    pub operation_admission: Vec<OperationAdmissionContract>,
}

#[derive(Debug, Clone)]
pub struct BackendContractRegistry {
    entries: BTreeMap<String, ComponentBackendContract>,
}

impl BackendContractRegistry {
    /// Create a registry whose backend identities are unique.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when the same backend ID is registered more
    /// than once.
    pub fn try_new(
        contracts: impl IntoIterator<Item = ComponentBackendContract>,
    ) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for contract in contracts {
            validate_backend_config_contract(&contract)?;
            let id = contract.id.clone();
            if entries.insert(id.clone(), contract).is_some() {
                return Err(format!(
                    "backend_registry_duplicate: backend {id:?} is registered more than once"
                ));
            }
        }
        Ok(Self { entries })
    }

    /// Return one installed backend contract.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when the backend is not registered.
    pub fn require(&self, id: &str) -> Result<&ComponentBackendContract, String> {
        self.entries
            .get(id)
            .ok_or_else(|| format!("backend_registry_missing: backend {id:?} is not registered"))
    }

    /// Iterate installed backend identities in canonical order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Return the machine-readable authoring schema owned by one backend.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when the backend is not registered.
    pub fn config_schema(&self, id: &str) -> Result<Value, String> {
        Ok(self.require(id)?.config_schema())
    }

    /// Validate only the agent-authored configuration fields of a component.
    ///
    /// # Errors
    ///
    /// Returns a stable, field-addressed diagnostic for unknown fields, wrong
    /// types, violated bounds, invalid enumerations, or cross-field rules.
    pub fn validate_component_config(&self, component: &ComponentSpec) -> Result<(), String> {
        self.require(&component.implementation)?
            .validate_config(component, false)
    }

    /// Resolve omitted component configuration through the installed backend
    /// contract without mutating explicitly requested values.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic for an unknown backend or kind mismatch.
    pub fn resolve_effective_component(
        &self,
        component: &ComponentSpec,
    ) -> Result<ComponentSpec, String> {
        let contract = self.require(&component.implementation)?;
        if contract.kind != component.kind {
            return Err(format!(
                "backend_kind_mismatch: component {:?} kind {:?} does not match backend {:?} kind {:?}",
                component.id, component.kind, contract.id, contract.kind
            ));
        }
        contract.validate_config(component, false)?;
        let mut effective = component.clone();
        for (name, field) in &contract.config_fields {
            if field.classification.is_agent_authorable() {
                if let Some(default) = &field.default {
                    effective
                        .config
                        .entry(name.clone())
                        .or_insert_with(|| resolve_config_default(default, &component.id));
                }
            }
        }
        contract.validate_config(&effective, true)?;
        Ok(effective)
    }

    /// Compile the cluster-free identity and admission portion of a component
    /// plan. Kubernetes resources are added by the B2 renderer.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic for an unknown backend or kind mismatch.
    pub fn compile_contract(
        &self,
        input: &ComponentPlanInput,
    ) -> Result<ComponentPlanContract, String> {
        let backend = self.require(&input.lock.catalog_id)?;
        if backend.kind != input.component.kind {
            return Err(format!(
                "backend_kind_mismatch: component {:?} kind {:?} does not match backend {:?} kind {:?}",
                input.component.id, input.component.kind, backend.id, backend.kind
            ));
        }
        if input.component.id != input.lock.component_id {
            return Err(format!(
                "backend_lock_mismatch: component {:?} does not match lock component {:?}",
                input.component.id, input.lock.component_id
            ));
        }
        if input.lock.config_version != backend.config_version {
            return Err(format!(
                "backend_lock_config_version_mismatch: component {:?} lock config version {:?} does not match installed backend {:?} config version {:?}",
                input.component.id, input.lock.config_version, backend.id, backend.config_version
            ));
        }
        let effective = self.resolve_effective_component(&input.component)?;
        let effective_config = EffectiveComponentConfig::try_from_component(&effective)?;
        let mut relevant_links = input.relevant_links.clone();
        relevant_links.sort();
        for link in &relevant_links {
            let Some(target) = input.linked_targets.get(&link.id) else {
                return Err(format!(
                    "backend_link_target_missing: component {:?} binding {:?} target {:?} has no descriptor",
                    input.component.id, link.id, link.to
                ));
            };
            if target.component_id != link.to {
                return Err(format!(
                    "backend_link_target_mismatch: link target {:?} has descriptor for {:?}",
                    link.to, target.component_id
                ));
            }
        }
        let storage = backend
            .storage_requirements
            .iter()
            .map(|requirement| requirement.resolve(&input.component.id))
            .collect();
        let execution_mounts = resolve_execution_mounts(
            backend,
            &input.component.id,
            &relevant_links,
            &input.linked_targets,
        )?;
        let credentials = compile_credential_observations(&execution_mounts, input)?;
        let protocol_probe = backend
            .protocol_probe
            .as_ref()
            .map(|probe| compile_protocol_probe(probe, &backend.service_ports))
            .transpose()?;
        Ok(ComponentPlanContract {
            instance_key: input.instance_key.clone(),
            revision_digest: input.revision_digest.clone(),
            backend_id: backend.id.clone(),
            component_id: input.component.id.clone(),
            kind: input.component.kind,
            rollout_digest: input.lock.rollout_digest.clone(),
            effective_config,
            relevant_links,
            linked_targets: input.linked_targets.clone(),
            execution_context: ExecutionContextContract {
                component_id: input.component.id.clone(),
                image: input.lock.image.clone(),
                state_contract: backend.execution_state_contract.clone(),
                mounts: execution_mounts,
                environment: backend.execution_environment.clone(),
            },
            target_descriptor: TargetDescriptorContract {
                component_id: input.component.id.clone(),
                kind: input.component.kind,
                backend_id: input.lock.catalog_id.clone(),
                version: input.lock.version.clone(),
                ports: backend.service_ports.clone(),
            },
            workload: WorkloadObservationContract {
                kind: backend.workload_kind,
                name: input.component.id.clone(),
                desired_replicas: 1,
            },
            storage,
            credentials,
            protocol_probe,
            applicable_conditions: backend.applicable_conditions.clone(),
            condition_reasons: backend.condition_reasons.clone(),
            condition_aggregation: backend.condition_aggregation.clone(),
            operation_admission: backend.operation_admission.clone(),
        })
    }
}

impl EffectiveComponentConfig {
    #[allow(
        clippy::too_many_lines,
        reason = "exact-version typed configuration decoding keeps all installed backends in one exhaustive match"
    )]
    fn try_from_component(component: &ComponentSpec) -> Result<Self, String> {
        let string = |name| {
            required_config_value(component, name)?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| typed_config_error(component, name))
        };
        let cdk = || -> Result<CdkMintConfig, String> {
            let integer = |name| {
                required_config_value(component, name)?
                    .as_u64()
                    .ok_or_else(|| typed_config_error(component, name))
            };
            Ok(CdkMintConfig {
                name: string("name")?,
                description: string("description")?,
                description_long: string("description_long")?,
                motd: string("motd")?,
                icon_url: string("icon_url")?,
                contact_email: string("contact_email")?,
                contact_nostr_public_key: string("contact_nostr_public_key")?,
                tos_url: string("tos_url")?,
                enable_info_page: required_config_value(component, "enable_info_page")?
                    .as_bool()
                    .ok_or_else(|| typed_config_error(component, "enable_info_page"))?,
                input_fee_ppk: integer("input_fee_ppk")?,
                use_keyset_v2: required_config_value(component, "use_keyset_v2")?
                    .as_bool()
                    .ok_or_else(|| typed_config_error(component, "use_keyset_v2"))?,
                mint_quote_ttl_seconds: integer("mint_quote_ttl_seconds")?,
                melt_quote_ttl_seconds: integer("melt_quote_ttl_seconds")?,
                http_cache_ttl_seconds: integer("http_cache_ttl_seconds")?,
                http_cache_tti_seconds: integer("http_cache_tti_seconds")?,
                max_inputs: integer("max_inputs")?,
                max_outputs: integer("max_outputs")?,
                min_mint_sat: integer("min_mint_sat")?,
                max_mint_sat: integer("max_mint_sat")?,
                min_melt_sat: integer("min_melt_sat")?,
                max_melt_sat: integer("max_melt_sat")?,
            })
        };
        let nutshell = || -> Result<NutshellMintConfig, String> {
            let integer = |name| {
                required_config_value(component, name)?
                    .as_u64()
                    .ok_or_else(|| typed_config_error(component, name))
            };
            let boolean = |name| {
                required_config_value(component, name)?
                    .as_bool()
                    .ok_or_else(|| typed_config_error(component, name))
            };
            let number = |name| {
                required_config_value(component, name)?
                    .as_f64()
                    .ok_or_else(|| typed_config_error(component, name))
            };
            Ok(NutshellMintConfig {
                name: string("name")?,
                description: string("description")?,
                description_long: string("description_long")?,
                motd: string("motd")?,
                icon_url: string("icon_url")?,
                contact_email: string("contact_email")?,
                contact_nostr_public_key: string("contact_nostr_public_key")?,
                tos_url: string("tos_url")?,
                input_fee_ppk: integer("input_fee_ppk")?,
                mint_quote_ttl_seconds: integer("mint_quote_ttl_seconds")?,
                melt_quote_ttl_seconds: integer("melt_quote_ttl_seconds")?,
                max_secret_length: integer("max_secret_length")?,
                max_witness_length: integer("max_witness_length")?,
                max_request_length: integer("max_request_length")?,
                max_mint_sat: integer("max_mint_sat")?,
                max_melt_sat: integer("max_melt_sat")?,
                max_balance_sat: integer("max_balance_sat")?,
                disable_mint: boolean("disable_mint")?,
                disable_melt: boolean("disable_melt")?,
                rate_limit: boolean("rate_limit")?,
                rate_limit_proxy_trust: boolean("rate_limit_proxy_trust")?,
                global_rate_limit_per_minute: integer("global_rate_limit_per_minute")?,
                transaction_rate_limit_per_minute: integer("transaction_rate_limit_per_minute")?,
                quote_backend_check_rate_limit_seconds: integer(
                    "quote_backend_check_rate_limit_seconds",
                )?,
                oidc_discovery_url: string("oidc_discovery_url")?,
                oidc_client_id: string("oidc_client_id")?,
                auth_rate_limit_per_minute: integer("auth_rate_limit_per_minute")?,
                auth_max_blind_tokens: integer("auth_max_blind_tokens")?,
                lightning_fee_percent: number("lightning_fee_percent")?,
                lightning_reserve_fee_min_sat: integer("lightning_reserve_fee_min_sat")?,
                clnrest_enable_mpp: boolean("clnrest_enable_mpp")?,
                lnd_enable_mpp: boolean("lnd_enable_mpp")?,
                redis_cache_ttl_seconds: integer("redis_cache_ttl_seconds")?,
                watchdog_enabled: boolean("watchdog_enabled")?,
                watchdog_balance_check_interval_seconds: integer(
                    "watchdog_balance_check_interval_seconds",
                )?,
                database_lock_timeout_ms: integer("database_lock_timeout_ms")?,
                regular_tasks_interval_seconds: integer("regular_tasks_interval_seconds")?,
                websocket_read_timeout_seconds: integer("websocket_read_timeout_seconds")?,
            })
        };
        match component.implementation.as_str() {
            "bitcoin-core" => Ok(Self::BitcoinCore(BitcoinCoreConfig {
                txindex: required_config_value(component, "txindex")?
                    .as_bool()
                    .ok_or_else(|| typed_config_error(component, "txindex"))?,
                fallback_fee: required_config_value(component, "fallback_fee")?
                    .as_f64()
                    .ok_or_else(|| typed_config_error(component, "fallback_fee"))?,
            })),
            "lnd" => Ok(Self::Lnd(LndConfig {
                alias: string("alias")?,
            })),
            "cln" => Ok(Self::Cln(ClnConfig {
                alias: string("alias")?,
            })),
            "cdk" => Ok(Self::Cdk(cdk()?)),
            "cdk-ldk" => Ok(Self::CdkLdk(cdk()?)),
            "cdk-bdk" => Ok(Self::CdkBdk(cdk()?)),
            "nutshell" => Ok(Self::Nutshell(nutshell()?)),
            "postgresql" => {
                let database_name = string("database_name")?;
                if !is_postgres_identifier(&database_name) {
                    return Err(config_diagnostic(
                        "identifier_violation",
                        component,
                        "database_name",
                        "must begin with a lowercase ASCII letter and contain only lowercase ASCII letters, digits, or '_'",
                    ));
                }
                Ok(Self::Postgres(PostgresConfig {
                    database_name,
                    storage_size: string("storage_size")?,
                }))
            }
            "redis" => Ok(Self::Redis(RedisConfig {
                maxmemory_mb: required_config_value(component, "maxmemory_mb")?
                    .as_u64()
                    .ok_or_else(|| typed_config_error(component, "maxmemory_mb"))?,
            })),
            "keycloak" => Ok(Self::Keycloak(KeycloakConfig {
                access_token_lifespan_seconds: required_config_value(
                    component,
                    "access_token_lifespan_seconds",
                )?
                .as_u64()
                .ok_or_else(|| typed_config_error(component, "access_token_lifespan_seconds"))?,
            })),
            "nutshell-wallet" => Ok(Self::NutshellWallet),
            "cdk-cli-wallet" => Ok(Self::CdkCliWallet),
            "cocod-wallet" => Ok(Self::CocodWallet),
            "attacker-workspace" => Ok(Self::AttackerWorkspace),
            implementation => Err(format!(
                "backend_typed_config_missing: implementation {implementation:?} has no typed effective configuration"
            )),
        }
    }
}

fn required_config_value<'a>(
    component: &'a ComponentSpec,
    name: &str,
) -> Result<&'a Value, String> {
    component
        .config
        .get(name)
        .ok_or_else(|| typed_config_error(component, name))
}

fn typed_config_error(component: &ComponentSpec, name: &str) -> String {
    format!(
        "backend_typed_config_invalid: component {:?} field /config/{name} was not normalized to its declared native type",
        component.id
    )
}

fn is_postgres_identifier(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl ComponentBackendContract {
    fn config_schema(&self) -> Value {
        let properties = self
            .config_fields
            .iter()
            .filter(|(_, field)| field.classification.is_agent_authorable())
            .map(|(name, field)| (name.clone(), config_field_schema(field, true)))
            .collect::<serde_json::Map<_, _>>();
        let managed_settings = self
            .config_fields
            .iter()
            .filter(|(_, field)| !field.classification.is_agent_authorable())
            .map(|(name, field)| (name.clone(), config_field_schema(field, false)))
            .collect::<serde_json::Map<_, _>>();
        let required = self
            .config_fields
            .iter()
            .filter(|(_, field)| {
                field.classification.is_agent_authorable()
                    && field.required
                    && field.default.is_none()
            })
            .map(|(name, _)| Value::String(name.clone()))
            .collect::<Vec<_>>();
        let all_of = self
            .config_rules
            .iter()
            .flat_map(config_rule_schema)
            .collect::<Vec<_>>();
        let mut schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": format!("https://proofstorm.dev/schemas/config/{}", self.config_version),
            "title": format!("{} configuration", self.id),
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": required,
            "x-proofstorm-config-version": self.config_version,
            "x-proofstorm-rules": self.config_rules,
            "x-proofstorm-managed-settings": managed_settings,
        });
        if !all_of.is_empty() {
            schema["allOf"] = Value::Array(all_of);
        }
        schema
    }

    fn validate_config(&self, component: &ComponentSpec, effective: bool) -> Result<(), String> {
        for (name, value) in &component.config {
            let Some(field) = self.config_fields.get(name) else {
                return Err(config_diagnostic(
                    "unknown_field",
                    component,
                    name,
                    "field is not declared by the backend configuration contract",
                ));
            };
            if !field.classification.is_agent_authorable() {
                return Err(config_diagnostic(
                    "managed_field",
                    component,
                    name,
                    "field is derived or owned by Proofstorm and cannot be supplied by an agent",
                ));
            }
            validate_config_value(component, name, value, field)?;
        }
        if self.id == "postgresql"
            && let Some(database_name) = component
                .config
                .get("database_name")
                .and_then(Value::as_str)
            && !is_postgres_identifier(database_name)
        {
            return Err(config_diagnostic(
                "identifier_violation",
                component,
                "database_name",
                "must begin with a lowercase ASCII letter and contain only lowercase ASCII letters, digits, or '_'",
            ));
        }
        if self.id == "nutshell"
            && let Some(discovery_url) = component
                .config
                .get("oidc_discovery_url")
                .and_then(Value::as_str)
            && !discovery_url.is_empty()
            && (!(discovery_url.starts_with("http://") || discovery_url.starts_with("https://"))
                || !discovery_url.ends_with("/.well-known/openid-configuration"))
        {
            return Err(config_diagnostic(
                "oidc_discovery_url_invalid",
                component,
                "oidc_discovery_url",
                "must be an HTTP(S) OpenID Connect discovery URL ending in /.well-known/openid-configuration",
            ));
        }
        if effective {
            for (name, field) in &self.config_fields {
                if field.classification.is_agent_authorable()
                    && field.required
                    && !component.config.contains_key(name)
                {
                    return Err(config_diagnostic(
                        "required_field_missing",
                        component,
                        name,
                        "required field is absent after default resolution",
                    ));
                }
            }
        }
        validate_config_rules(component, &self.config_rules)
    }
}

impl ConfigSettingClass {
    const fn is_agent_authorable(self) -> bool {
        matches!(self, Self::AgentAuthorable | Self::ImportedSecretReference)
    }
}

fn config_field_schema(field: &ConfigFieldContract, include_default: bool) -> Value {
    let mut schema = serde_json::Map::from_iter([
        (
            "description".into(),
            Value::String(field.description.clone()),
        ),
        (
            "type".into(),
            Value::String(config_json_type(field.value_kind).into()),
        ),
        (
            "x-proofstorm-classification".into(),
            serde_json::to_value(field.classification)
                .expect("configuration classification serializes"),
        ),
    ]);
    if include_default {
        if let Some(ConfigDefault::Literal(value)) = &field.default {
            schema.insert("default".into(), value.clone());
        } else if matches!(field.default, Some(ConfigDefault::ComponentId)) {
            schema.insert(
                "x-proofstorm-default".into(),
                json!({"source": "component_id"}),
            );
        }
    } else {
        schema.insert("readOnly".into(), Value::Bool(true));
    }
    if !field.enum_values.is_empty() {
        schema.insert("enum".into(), Value::Array(field.enum_values.clone()));
    }
    insert_optional_number(&mut schema, "minimum", field.minimum);
    insert_optional_number(&mut schema, "maximum", field.maximum);
    insert_optional_usize(&mut schema, "minLength", field.min_length);
    insert_optional_usize(&mut schema, "maxLength", field.max_length);
    Value::Object(schema)
}

fn config_json_type(kind: ConfigValueKind) -> &'static str {
    match kind {
        ConfigValueKind::Boolean => "boolean",
        ConfigValueKind::Number => "number",
        ConfigValueKind::Integer => "integer",
        ConfigValueKind::String => "string",
    }
}

fn resolve_config_default(default: &ConfigDefault, component_id: &str) -> Value {
    match default {
        ConfigDefault::Literal(value) => value.clone(),
        ConfigDefault::ComponentId => Value::String(component_id.into()),
    }
}

fn config_rule_schema(rule: &ConfigRule) -> Vec<Value> {
    match rule {
        ConfigRule::MutuallyExclusive { fields } => {
            let fields = fields.iter().collect::<Vec<_>>();
            let mut pairs = Vec::new();
            for (index, left) in fields.iter().enumerate() {
                for right in fields.iter().skip(index + 1) {
                    pairs.push(json!({"not": {"required": [left, right]}}));
                }
            }
            pairs
        }
        ConfigRule::RequiredWhen {
            field,
            equals,
            required_field,
        } => vec![json!({
            "if": {"properties": {field: {"const": equals}}, "required": [field]},
            "then": {"required": [required_field]}
        })],
        ConfigRule::LessThanOrEqual { .. } => vec![],
    }
}

fn insert_optional_number(
    object: &mut serde_json::Map<String, Value>,
    name: &str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        object.insert(name.into(), json!(value));
    }
}

fn insert_optional_usize(
    object: &mut serde_json::Map<String, Value>,
    name: &str,
    value: Option<usize>,
) {
    if let Some(value) = value {
        object.insert(name.into(), json!(value));
    }
}

fn validate_config_value(
    component: &ComponentSpec,
    name: &str,
    value: &Value,
    field: &ConfigFieldContract,
) -> Result<(), String> {
    let type_matches = match field.value_kind {
        ConfigValueKind::Boolean => value.is_boolean(),
        ConfigValueKind::Number => value.is_number(),
        ConfigValueKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        ConfigValueKind::String => value.is_string(),
    };
    if !type_matches {
        return Err(config_diagnostic(
            "wrong_type",
            component,
            name,
            &format!("expected {}", config_json_type(field.value_kind)),
        ));
    }
    if !field.enum_values.is_empty() && !field.enum_values.contains(value) {
        return Err(config_diagnostic(
            "enum_violation",
            component,
            name,
            "value is not one of the declared enumeration values",
        ));
    }
    if let Some(number) = value.as_f64() {
        if field.minimum.is_some_and(|minimum| number < minimum)
            || field.maximum.is_some_and(|maximum| number > maximum)
        {
            return Err(config_diagnostic(
                "numeric_bound_violation",
                component,
                name,
                "numeric value is outside the declared inclusive bounds",
            ));
        }
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count();
        if field.min_length.is_some_and(|minimum| length < minimum)
            || field.max_length.is_some_and(|maximum| length > maximum)
        {
            return Err(config_diagnostic(
                "string_length_violation",
                component,
                name,
                "string length is outside the declared inclusive bounds",
            ));
        }
    }
    Ok(())
}

fn validate_config_rules(component: &ComponentSpec, rules: &[ConfigRule]) -> Result<(), String> {
    for rule in rules {
        match rule {
            ConfigRule::MutuallyExclusive { fields } => {
                let present = fields
                    .iter()
                    .filter(|field| component.config.contains_key(*field))
                    .collect::<Vec<_>>();
                if present.len() > 1 {
                    return Err(format!(
                        "config_mutually_exclusive: component {:?} fields {:?} cannot be set together",
                        component.id, present
                    ));
                }
            }
            ConfigRule::RequiredWhen {
                field,
                equals,
                required_field,
            } if component.config.get(field) == Some(equals)
                && !component.config.contains_key(required_field) =>
            {
                return Err(config_diagnostic(
                    "conditional_required_field_missing",
                    component,
                    required_field,
                    &format!("field is required when /config/{field} equals {equals}"),
                ));
            }
            ConfigRule::RequiredWhen { .. } => {}
            ConfigRule::LessThanOrEqual {
                minimum_field,
                maximum_field,
            } => {
                let (Some(minimum), Some(maximum)) = (
                    component.config.get(minimum_field).and_then(Value::as_u64),
                    component.config.get(maximum_field).and_then(Value::as_u64),
                ) else {
                    continue;
                };
                if minimum > maximum {
                    return Err(format!(
                        "config_order_violation: component {:?} field /config/{minimum_field} must be less than or equal to /config/{maximum_field}",
                        component.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn config_diagnostic(code: &str, component: &ComponentSpec, field: &str, message: &str) -> String {
    format!(
        "config_{code}: component {:?} field /config/{field}: {message}",
        component.id
    )
}

fn resolve_execution_mounts(
    backend: &ComponentBackendContract,
    component_id: &str,
    relevant_links: &[LinkSpec],
    linked_targets: &BTreeMap<String, TargetDescriptorContract>,
) -> Result<Vec<ExecutionMountContract>, String> {
    let mut resolved = Vec::new();
    let mut alternative_groups = BTreeMap::<&str, usize>::new();
    for mount in &backend.execution_mounts {
        if let ExecutionMountRequirement::AtLeastOne { group } = &mount.requirement {
            alternative_groups.entry(group).or_default();
        }
        let source = match &mount.source {
            ExecutionStorageTemplateSource::StatefulData => ExecutionStorageSource::StatefulData,
            ExecutionStorageTemplateSource::ComponentPersistentData => {
                ExecutionStorageSource::ComponentPersistentData
            }
            ExecutionStorageTemplateSource::ComponentConfig => {
                ExecutionStorageSource::ComponentConfig
            }
            ExecutionStorageTemplateSource::LinkedStatefulData {
                link_kind,
                binding,
                target_implementation,
            } => {
                let links = relevant_links
                    .iter()
                    .filter(|link| {
                        link.kind == *link_kind
                            && link.binding.as_ref() == Some(binding)
                            && linked_targets
                                .get(&link.id)
                                .is_some_and(|target| target.backend_id == *target_implementation)
                    })
                    .collect::<Vec<_>>();
                let link = match links.as_slice() {
                    [] if matches!(
                        mount.requirement,
                        ExecutionMountRequirement::AtLeastOne { .. }
                    ) =>
                    {
                        continue;
                    }
                    [link] => *link,
                    [] => {
                        return Err(format!(
                            "backend_execution_binding_missing: component {component_id:?} mount {:?} requires one {link_kind:?} binding matching {binding:?} through implementation {target_implementation:?}",
                            mount.name,
                        ));
                    }
                    _ => {
                        return Err(format!(
                            "backend_execution_binding_ambiguous: component {component_id:?} mount {:?} requires one {link_kind:?} binding matching {binding:?} through implementation {target_implementation:?}, found {:?}",
                            mount.name,
                            links
                                .iter()
                                .map(|link| link.id.as_str())
                                .collect::<Vec<_>>()
                        ));
                    }
                };
                ExecutionStorageSource::LinkedStatefulData {
                    link_id: link.id.clone(),
                }
            }
        };
        if let ExecutionMountRequirement::AtLeastOne { group } = &mount.requirement {
            *alternative_groups.entry(group).or_default() += 1;
        }
        resolved.push(ExecutionMountContract {
            name: mount.name.clone(),
            mount_path: mount.mount_path.clone(),
            read_only: mount.read_only,
            source,
        });
    }
    if let Some((group, _)) = alternative_groups.iter().find(|(_, count)| **count == 0) {
        return Err(format!(
            "backend_execution_binding_group_missing: component {component_id:?} requires at least one resolved execution mount in group {group:?}"
        ));
    }
    Ok(resolved)
}

fn compile_credential_observations(
    execution_mounts: &[ExecutionMountContract],
    input: &ComponentPlanInput,
) -> Result<Vec<CredentialObservationContract>, String> {
    let mut credentials = Vec::new();
    for mount in execution_mounts {
        let ExecutionStorageSource::LinkedStatefulData { ref link_id } = mount.source else {
            continue;
        };
        let linked = input.linked_state.get(link_id).ok_or_else(|| {
            format!(
                "backend_link_state_missing: component {:?} binding {link_id:?} has no state observation contract",
                input.component.id
            )
        })?;
        let [linked_storage] = linked.storage.as_slice() else {
            return Err(format!(
                "backend_link_storage_cardinality: component {:?} binding {link_id:?} mount {:?} requires exactly one linked storage claim, found {}",
                input.component.id,
                mount.name,
                linked.storage.len()
            ));
        };
        credentials.push(CredentialObservationContract {
            identity: format!(
                "{}:{link_id}:{}:{}",
                mount.name, linked.component_id, linked_storage.claim_name
            ),
            source_component_id: linked.component_id.clone(),
            source_state_contract: linked.state_contract.clone(),
            claim_name: linked_storage.claim_name.clone(),
            mount_name: mount.name.clone(),
            mount_path: mount.mount_path.clone(),
            read_only: mount.read_only,
        });
    }
    credentials.sort_by(|left, right| left.identity.cmp(&right.identity));
    Ok(credentials)
}

impl StorageRequirementTemplate {
    #[must_use]
    pub fn resolve(&self, component_id: &str) -> StorageObservationContract {
        StorageObservationContract {
            claim_name: match self {
                Self::StatefulClaimTemplate { template_name } => {
                    format!("{template_name}-{component_id}-0")
                }
                Self::ComponentClaim { suffix } => format!("{component_id}{suffix}"),
            },
        }
    }
}

#[must_use]
/// Return the built-in, internally validated backend registry.
///
/// The built-in backend contracts, built once per process.
///
/// Every reconcile, action admission and catalog request needs this registry.
/// Rebuilding it meant reconstructing ~215 lines of contract structs and
/// revalidating each one on paths that run every few seconds.
///
/// # Panics
///
/// Panics when a built-in backend violates a configuration-contract invariant.
/// This is a programmer error covered by the backend contract tests.
pub fn default_backend_registry() -> &'static BackendContractRegistry {
    static REGISTRY: std::sync::LazyLock<BackendContractRegistry> =
        std::sync::LazyLock::new(|| {
            BackendContractRegistry::try_new(default_backend_contracts())
                .expect("built-in backend configuration contracts are valid")
        });
    &REGISTRY
}

fn validate_backend_config_contract(contract: &ComponentBackendContract) -> Result<(), String> {
    for (name, field) in &contract.config_fields {
        if !field.classification.is_agent_authorable() && field.default.is_some() {
            return Err(format!(
                "backend_managed_config_default_forbidden: backend {:?} field {name:?} cannot publish or resolve a managed value as an authoring default",
                contract.id
            ));
        }
    }
    for rule in &contract.config_rules {
        let referenced = match rule {
            ConfigRule::MutuallyExclusive { fields } => fields.iter().collect::<Vec<_>>(),
            ConfigRule::RequiredWhen {
                field,
                required_field,
                ..
            } => vec![field, required_field],
            ConfigRule::LessThanOrEqual {
                minimum_field,
                maximum_field,
            } => vec![minimum_field, maximum_field],
        };
        for name in referenced {
            let Some(field) = contract.config_fields.get(name) else {
                return Err(format!(
                    "backend_config_rule_field_missing: backend {:?} rule references undeclared field {name:?}",
                    contract.id
                ));
            };
            if !field.classification.is_agent_authorable() {
                return Err(format!(
                    "backend_config_rule_managed_field: backend {:?} authoring rule references managed field {name:?}",
                    contract.id
                ));
            }
        }
    }
    let mut mount_names = BTreeSet::new();
    let mut mount_paths = BTreeSet::new();
    for mount in &contract.execution_mounts {
        if !mount_names.insert(mount.name.as_str())
            || !mount_paths.insert(mount.mount_path.as_str())
        {
            return Err(format!(
                "backend_execution_mount_collision: backend {:?} repeats mount name {:?} or path {:?}",
                contract.id, mount.name, mount.mount_path
            ));
        }
        if let ExecutionMountRequirement::AtLeastOne { group } = &mount.requirement {
            if group.is_empty() {
                return Err(format!(
                    "backend_execution_mount_group_missing: backend {:?} mount {:?} has an empty alternative group",
                    contract.id, mount.name
                ));
            }
            if !matches!(
                &mount.source,
                ExecutionStorageTemplateSource::LinkedStatefulData { .. }
            ) {
                return Err(format!(
                    "backend_execution_mount_group_source_invalid: backend {:?} mount {:?} uses an alternative requirement without linked state",
                    contract.id, mount.name
                ));
            }
        }
        let ExecutionStorageTemplateSource::LinkedStatefulData {
            link_kind,
            binding,
            target_implementation,
        } = &mount.source
        else {
            continue;
        };
        if target_implementation.is_empty() {
            return Err(format!(
                "backend_execution_selector_target_missing: backend {:?} mount {:?} has no target implementation",
                contract.id, mount.name
            ));
        }
        let compatible = matches!(
            (link_kind, binding),
            (
                crate::LinkKind::ChainBackend,
                crate::DependencyBinding::Chain { .. }
            ) | (
                crate::LinkKind::PaymentBackend,
                crate::DependencyBinding::Payment { .. }
            )
        );
        if !compatible {
            return Err(format!(
                "backend_execution_selector_incompatible: backend {:?} mount {:?} link kind {link_kind:?} does not match binding {binding:?}",
                contract.id, mount.name
            ));
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "backend support contracts deliberately declare native configuration fields inline"
)]
fn default_backend_contracts() -> Vec<ComponentBackendContract> {
    vec![
        contract(
            "bitcoin-core",
            ComponentKind::Bitcoin,
            "bitcoin-core/30/v1",
            BTreeMap::from([
                (
                    "fallback_fee".into(),
                    config_field(
                        "Fallback transaction fee in BTC per kvB",
                        ConfigValueKind::Number,
                        ConfigDefault::Literal(json!(0.0002)),
                    )
                    .with_numeric_bounds(0.0, 1.0),
                ),
                (
                    "txindex".into(),
                    config_field(
                        "Enable the complete transaction index",
                        ConfigValueKind::Boolean,
                        ConfigDefault::Literal(json!(true)),
                    ),
                ),
            ]),
            BTreeMap::from([
                ("p2p".into(), 18_444),
                ("rpc".into(), 18_443),
                ("zmq_block".into(), 28_334),
                ("zmq_tx".into(), 28_335),
            ]),
            "proofstorm/bitcoin-core-state/v1",
            service_conditions(false, false),
        ),
        contract(
            "lnd",
            ComponentKind::Lightning,
            "lnd/0.20/v1",
            BTreeMap::from([(
                "alias".into(),
                config_field(
                    "Native LND node alias",
                    ConfigValueKind::String,
                    ConfigDefault::ComponentId,
                )
                .with_string_bounds(1, 32),
            )]),
            BTreeMap::from([
                ("p2p".into(), 9_735),
                ("rest".into(), 8_080),
                ("rpc".into(), 10_009),
            ]),
            "proofstorm/lnd-state/v1",
            service_conditions(true, false),
        ),
        contract(
            "cln",
            ComponentKind::Lightning,
            "cln/26.06/v1",
            BTreeMap::from([(
                "alias".into(),
                config_field(
                    "Native Core Lightning node alias",
                    ConfigValueKind::String,
                    ConfigDefault::ComponentId,
                )
                .with_string_bounds(1, 32),
            )]),
            BTreeMap::from([("p2p".into(), 9_735), ("rest".into(), 3_010)]),
            "proofstorm/cln-state/v1",
            service_conditions(true, false),
        ),
        contract(
            "cdk",
            ComponentKind::Mint,
            "cdk-mintd/0.18/v1",
            cdk_config_fields(
                "Proofstorm CDK mint",
                "Proofstorm regtest CDK mint",
                1,
                500_000,
            ),
            BTreeMap::from([("http".into(), 3_338)]),
            "proofstorm/cdk-mint-state/v1",
            service_conditions(true, true),
        ),
        contract(
            "cdk-ldk",
            ComponentKind::Mint,
            "cdk-mintd-ldk/0.18/v1",
            cdk_config_fields(
                "Proofstorm CDK LDK mint",
                "Proofstorm regtest CDK LDK mint",
                1,
                500_000,
            ),
            BTreeMap::from([("http".into(), 3_338), ("p2p".into(), 9_735)]),
            "proofstorm/cdk-mint-ldk-state/v1",
            service_conditions(true, false),
        ),
        contract(
            "cdk-bdk",
            ComponentKind::Mint,
            "cdk-mintd-bdk/0.18/v1",
            cdk_config_fields(
                "Proofstorm CDK BDK mint",
                "Proofstorm regtest CDK BDK mint",
                1_000,
                1_000_000,
            ),
            BTreeMap::from([("http".into(), 3_338)]),
            "proofstorm/cdk-mint-bdk-state/v1",
            service_conditions(true, false),
        ),
        contract(
            "nutshell",
            ComponentKind::Mint,
            "nutshell-mint/0.20/v1",
            nutshell_config_fields(),
            BTreeMap::from([("http".into(), 3_338)]),
            "proofstorm/nutshell-mint-state/v1",
            service_conditions(true, true),
        ),
        contract(
            "redis",
            ComponentKind::Database,
            "redis/8.10/v1",
            BTreeMap::from([(
                "maxmemory_mb".into(),
                config_field(
                    "Maximum in-memory cache size",
                    ConfigValueKind::Integer,
                    ConfigDefault::Literal(json!(64)),
                )
                .with_numeric_bounds(16.0, 1_024.0),
            )]),
            BTreeMap::from([("redis".into(), 6_379)]),
            "proofstorm/redis-cache-state/v1",
            ephemeral_service_conditions(),
        ),
        contract(
            "keycloak",
            ComponentKind::IdentityProvider,
            "keycloak/25/v1",
            BTreeMap::from([(
                "access_token_lifespan_seconds".into(),
                config_field(
                    "OIDC access-token lifetime",
                    ConfigValueKind::Integer,
                    ConfigDefault::Literal(json!(300)),
                )
                .with_numeric_bounds(60.0, 3_600.0),
            )]),
            BTreeMap::from([("http".into(), 8_080)]),
            "proofstorm/keycloak-oidc-state/v1",
            ephemeral_dependency_service_conditions(),
        ),
        contract(
            "postgresql",
            ComponentKind::Database,
            "postgresql/17/v1",
            BTreeMap::from([
                (
                    "database_name".into(),
                    config_field(
                        "Database created for the linked primary client",
                        ConfigValueKind::String,
                        ConfigDefault::Literal(json!("cdk_mint")),
                    )
                    .with_string_bounds(1, 63),
                ),
                (
                    "storage_size".into(),
                    config_field(
                        "Persistent database volume size",
                        ConfigValueKind::String,
                        ConfigDefault::Literal(json!("1Gi")),
                    )
                    .with_enum_values(&["1Gi", "2Gi", "5Gi", "10Gi"]),
                ),
            ]),
            BTreeMap::from([("postgres".into(), 5_432)]),
            "proofstorm/postgresql-state/v1",
            service_conditions(false, false),
        ),
        contract(
            "nutshell-wallet",
            ComponentKind::Wallet,
            "nutshell-wallet/0.20/v1",
            BTreeMap::new(),
            BTreeMap::new(),
            "proofstorm/nutshell-wallet-state/v1",
            BTreeSet::from([
                ComponentConditionType::WorkloadReady,
                ComponentConditionType::StorageReady,
                ComponentConditionType::ComponentReady,
                ComponentConditionType::ExperimentControllable,
            ]),
        ),
        contract(
            "cdk-cli-wallet",
            ComponentKind::Wallet,
            "cdk-cli-wallet/0.18/v1",
            BTreeMap::new(),
            BTreeMap::new(),
            "proofstorm/cdk-cli-wallet-state/v1",
            BTreeSet::from([
                ComponentConditionType::WorkloadReady,
                ComponentConditionType::StorageReady,
                ComponentConditionType::ComponentReady,
                ComponentConditionType::ExperimentControllable,
            ]),
        ),
        contract(
            "cocod-wallet",
            ComponentKind::Wallet,
            "cocod-wallet/0.0.17/v1",
            BTreeMap::new(),
            BTreeMap::new(),
            "proofstorm/cocod-wallet-state/v1",
            BTreeSet::from([
                ComponentConditionType::WorkloadReady,
                ComponentConditionType::StorageReady,
                ComponentConditionType::ComponentReady,
                ComponentConditionType::ExperimentControllable,
            ]),
        ),
        contract(
            "attacker-workspace",
            ComponentKind::Attacker,
            "attacker-workspace/0.1/v1",
            BTreeMap::new(),
            BTreeMap::new(),
            "proofstorm/attacker-workspace-state/v1",
            BTreeSet::from([
                ComponentConditionType::WorkloadReady,
                ComponentConditionType::ComponentReady,
                ComponentConditionType::ExperimentControllable,
            ]),
        ),
    ]
}

fn config_field(
    description: &str,
    value_kind: ConfigValueKind,
    default: ConfigDefault,
) -> ConfigFieldContract {
    ConfigFieldContract {
        description: description.into(),
        value_kind,
        classification: ConfigSettingClass::AgentAuthorable,
        default: Some(default),
        enum_values: vec![],
        minimum: None,
        maximum: None,
        min_length: None,
        max_length: None,
        required: true,
    }
}

fn cdk_config_fields(
    default_name: &str,
    default_description: &str,
    default_minimum: u32,
    default_maximum: u32,
) -> BTreeMap<String, ConfigFieldContract> {
    let mut fields = cdk_mint_info_config_fields(default_name, default_description);
    let integer = |description: &str, default: u32, minimum: u32, maximum: u32| {
        config_field(
            description,
            ConfigValueKind::Integer,
            ConfigDefault::Literal(json!(default)),
        )
        .with_numeric_bounds(f64::from(minimum), f64::from(maximum))
    };
    fields.extend(BTreeMap::from([
        (
            "http_cache_tti_seconds".into(),
            integer("In-memory HTTP response-cache idle lifetime", 60, 1, 86_400),
        ),
        (
            "http_cache_ttl_seconds".into(),
            integer(
                "In-memory HTTP response-cache absolute lifetime",
                60,
                1,
                86_400,
            ),
        ),
        (
            "input_fee_ppk".into(),
            integer("Input fee in parts per thousand", 100, 0, 1_000_000),
        ),
        (
            "max_melt_sat".into(),
            integer("Maximum melt quote amount", default_maximum, 1, 10_000_000),
        ),
        (
            "max_mint_sat".into(),
            integer("Maximum mint quote amount", default_maximum, 1, 10_000_000),
        ),
        (
            "max_inputs".into(),
            integer(
                "Maximum inputs accepted by a swap or melt request",
                1_000,
                1,
                10_000,
            ),
        ),
        (
            "max_outputs".into(),
            integer(
                "Maximum outputs accepted by a mint, swap, or melt request",
                1_000,
                1,
                10_000,
            ),
        ),
        (
            "melt_quote_ttl_seconds".into(),
            integer("Melt quote lifetime", 120, 1, 86_400),
        ),
        (
            "min_melt_sat".into(),
            integer("Minimum melt quote amount", default_minimum, 1, 10_000_000),
        ),
        (
            "min_mint_sat".into(),
            integer("Minimum mint quote amount", default_minimum, 1, 10_000_000),
        ),
        (
            "mint_quote_ttl_seconds".into(),
            integer("Mint quote lifetime", 600, 1, 86_400),
        ),
        (
            "use_keyset_v2".into(),
            config_field(
                "Create new keysets using CDK keyset version 2",
                ConfigValueKind::Boolean,
                ConfigDefault::Literal(json!(true)),
            ),
        ),
    ]));
    fields
}

fn cdk_mint_info_config_fields(
    default_name: &str,
    default_description: &str,
) -> BTreeMap<String, ConfigFieldContract> {
    let string = |description, default, maximum| {
        config_field(
            description,
            ConfigValueKind::String,
            ConfigDefault::Literal(json!(default)),
        )
        .with_string_bounds(0, maximum)
    };
    BTreeMap::from([
        (
            "contact_email".into(),
            string(
                "NUT-06 operator email contact; an empty value omits it",
                "",
                254,
            ),
        ),
        (
            "contact_nostr_public_key".into(),
            string(
                "NUT-06 operator Nostr public key in hexadecimal; an empty value omits it",
                "",
                64,
            ),
        ),
        (
            "description".into(),
            string("Native CDK mint description", default_description, 1_000),
        ),
        (
            "description_long".into(),
            string(
                "Long-form NUT-06 mint description; an empty value omits it",
                "",
                10_000,
            ),
        ),
        (
            "enable_info_page".into(),
            config_field(
                "Enable CDK's human-facing mint information page",
                ConfigValueKind::Boolean,
                ConfigDefault::Literal(json!(false)),
            ),
        ),
        (
            "icon_url".into(),
            string("NUT-06 mint icon URL; an empty value omits it", "", 2_048),
        ),
        (
            "motd".into(),
            string(
                "NUT-06 message of the day; an empty value omits it",
                "",
                1_000,
            ),
        ),
        (
            "name".into(),
            string("Native CDK mint name", default_name, 100).with_string_bounds(1, 100),
        ),
        (
            "tos_url".into(),
            string(
                "NUT-06 terms-of-service URL; an empty value omits it",
                "",
                2_048,
            ),
        ),
    ])
}

#[allow(
    clippy::too_many_lines,
    reason = "the Nutshell support contract deliberately declares every authorable upstream setting inline"
)]
fn nutshell_config_fields() -> BTreeMap<String, ConfigFieldContract> {
    let integer = |description: &str, default: u32, minimum: u32, maximum: u32| {
        config_field(
            description,
            ConfigValueKind::Integer,
            ConfigDefault::Literal(json!(default)),
        )
        .with_numeric_bounds(f64::from(minimum), f64::from(maximum))
    };
    let boolean = |description: &str, default: bool| {
        config_field(
            description,
            ConfigValueKind::Boolean,
            ConfigDefault::Literal(json!(default)),
        )
    };
    let string = |description: &str, default: &str, minimum: usize, maximum: usize| {
        config_field(
            description,
            ConfigValueKind::String,
            ConfigDefault::Literal(json!(default)),
        )
        .with_string_bounds(minimum, maximum)
    };
    BTreeMap::from([
        (
            "auth_max_blind_tokens".into(),
            integer(
                "Maximum NUT-22 blind-auth tokens issued per authenticated request",
                100,
                1,
                100_000,
            ),
        ),
        (
            "auth_rate_limit_per_minute".into(),
            integer(
                "Maximum NUT-21 authentication attempts per user per minute",
                5,
                1,
                100_000,
            ),
        ),
        (
            "contact_email".into(),
            string("NUT-06 operator email; empty omits it", "", 0, 254),
        ),
        (
            "contact_nostr_public_key".into(),
            string(
                "NUT-06 operator Nostr public key; empty omits it",
                "",
                0,
                128,
            ),
        ),
        (
            "database_lock_timeout_ms".into(),
            integer("PostgreSQL migration lock timeout", 30_000, 1_000, 300_000),
        ),
        (
            "description".into(),
            string(
                "Native Nutshell mint description",
                "Proofstorm regtest Nutshell mint",
                1,
                1_000,
            ),
        ),
        (
            "description_long".into(),
            string(
                "Long-form NUT-06 mint description; empty omits it",
                "",
                0,
                10_000,
            ),
        ),
        (
            "disable_melt".into(),
            boolean("Disable BOLT11 melt operations", false),
        ),
        (
            "disable_mint".into(),
            boolean("Disable BOLT11 mint operations", false),
        ),
        (
            "global_rate_limit_per_minute".into(),
            integer("Maximum requests per client IP per minute", 60, 1, 100_000),
        ),
        (
            "icon_url".into(),
            string("NUT-06 mint icon URL; empty omits it", "", 0, 2_048),
        ),
        (
            "input_fee_ppk".into(),
            integer("Input fee in parts per thousand", 100, 0, 1_000_000),
        ),
        (
            "lightning_fee_percent".into(),
            config_field(
                "Lightning fee reserve as a percentage",
                ConfigValueKind::Number,
                ConfigDefault::Literal(json!(1.0)),
            )
            .with_numeric_bounds(0.0, 100.0),
        ),
        (
            "lightning_reserve_fee_min_sat".into(),
            integer("Minimum Lightning fee reserve", 2, 0, 1_000_000),
        ),
        (
            "clnrest_enable_mpp".into(),
            boolean("Allow multi-part Core Lightning REST payments", true),
        ),
        (
            "lnd_enable_mpp".into(),
            boolean("Allow multi-part LND payments", true),
        ),
        (
            "max_balance_sat".into(),
            integer(
                "Maximum outstanding mint balance",
                10_000_000,
                1,
                100_000_000,
            ),
        ),
        (
            "max_melt_sat".into(),
            integer("Maximum BOLT11 melt amount", 500_000, 1, 10_000_000),
        ),
        (
            "max_mint_sat".into(),
            integer("Maximum BOLT11 mint amount", 500_000, 1, 10_000_000),
        ),
        (
            "max_request_length".into(),
            integer("Maximum REST request array length", 1_000, 1, 100_000),
        ),
        (
            "max_secret_length".into(),
            integer("Maximum proof secret length", 1_024, 1, 1_000_000),
        ),
        (
            "max_witness_length".into(),
            integer("Maximum proof witness length", 1_024, 1, 1_000_000),
        ),
        (
            "melt_quote_ttl_seconds".into(),
            integer("Melt quote lifetime", 120, 0, 86_400),
        ),
        (
            "mint_quote_ttl_seconds".into(),
            integer("Mint quote lifetime", 600, 0, 86_400),
        ),
        (
            "motd".into(),
            string("NUT-06 message of the day; empty omits it", "", 0, 1_000),
        ),
        (
            "name".into(),
            string(
                "Native Nutshell mint name",
                "Proofstorm Nutshell mint",
                1,
                100,
            ),
        ),
        (
            "oidc_client_id".into(),
            string(
                "OIDC public client identifier used to validate the token authorized party",
                "cashu-client",
                1,
                255,
            ),
        ),
        (
            "oidc_discovery_url".into(),
            string(
                "NUT-21 OpenID Connect discovery URL; empty disables NUT-21 and NUT-22 authentication",
                "",
                0,
                2_048,
            ),
        ),
        (
            "quote_backend_check_rate_limit_seconds".into(),
            integer(
                "Minimum interval between unpaid quote backend checks",
                10,
                0,
                3_600,
            ),
        ),
        (
            "rate_limit".into(),
            boolean("Enable IP-based HTTP rate limiting", true),
        ),
        (
            "rate_limit_proxy_trust".into(),
            boolean("Trust proxy-supplied client IP headers", false),
        ),
        (
            "redis_cache_ttl_seconds".into(),
            integer("Redis response-cache lifetime", 604_800, 1, 31_536_000),
        ),
        (
            "regular_tasks_interval_seconds".into(),
            integer(
                "Interval for invoice checks and regular mint tasks",
                3_600,
                1,
                86_400,
            ),
        ),
        (
            "tos_url".into(),
            string("NUT-06 terms-of-service URL; empty omits it", "", 0, 2_048),
        ),
        (
            "transaction_rate_limit_per_minute".into(),
            integer(
                "Maximum transactional requests per client IP per minute",
                20,
                1,
                100_000,
            ),
        ),
        (
            "watchdog_balance_check_interval_seconds".into(),
            integer("Balance watchdog polling interval", 60, 1, 86_400),
        ),
        (
            "watchdog_enabled".into(),
            boolean(
                "Stop the mint when backend and mint balances diverge",
                false,
            ),
        ),
        (
            "websocket_read_timeout_seconds".into(),
            integer("WebSocket read timeout", 600, 1, 86_400),
        ),
    ])
}

fn managed_field(
    description: &str,
    value_kind: ConfigValueKind,
    classification: ConfigSettingClass,
) -> ConfigFieldContract {
    ConfigFieldContract {
        description: description.into(),
        value_kind,
        classification,
        default: None,
        enum_values: vec![],
        minimum: None,
        maximum: None,
        min_length: None,
        max_length: None,
        required: true,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the managed-setting inventory deliberately keeps every backend declaration together"
)]
fn managed_config_fields(backend: &str) -> BTreeMap<String, ConfigFieldContract> {
    use ConfigSettingClass::{
        GeneratedInstanceSecret as Secret, RuntimePolicy as Policy, TopologyDerived as Topology,
    };
    let string = |description, classification| {
        managed_field(description, ConfigValueKind::String, classification)
    };
    let integer = |description, classification| {
        managed_field(description, ConfigValueKind::Integer, classification)
    };
    match backend {
        "bitcoin-core" => BTreeMap::from([
            (
                "data_directory".into(),
                string("Bitcoin data directory", Policy),
            ),
            (
                "debug_level".into(),
                string("Fixed Bitcoin debug level", Policy),
            ),
            (
                "network".into(),
                string("Fixed Bitcoin regtest network", Policy),
            ),
            (
                "p2p_bind".into(),
                string("Derived P2P listen endpoint", Topology),
            ),
            (
                "rpc_allow_policy".into(),
                string("Disposable-lab RPC allow policy", Policy),
            ),
            (
                "rpc_bind".into(),
                string("Derived RPC listen endpoint", Topology),
            ),
            (
                "rpc_credentials".into(),
                string("Fixed disposable-regtest RPC credentials", Policy),
            ),
            (
                "server_mode".into(),
                string("Bitcoin RPC server mode", Policy),
            ),
            (
                "zmq_endpoints".into(),
                string("Derived raw block and transaction endpoints", Topology),
            ),
        ]),
        "lnd" => BTreeMap::from([
            (
                "accept_keysend".into(),
                string("Keysend acceptance policy", Policy),
            ),
            (
                "admin_macaroon".into(),
                string("Instance admin macaroon", Secret),
            ),
            (
                "chain_backend_endpoint".into(),
                string("Linked Bitcoin RPC and ZMQ endpoints", Topology),
            ),
            (
                "chain_backend_credentials".into(),
                string("Linked Bitcoin RPC credentials", Secret),
            ),
            (
                "data_directory".into(),
                string("LND data directory", Policy),
            ),
            (
                "debug_level".into(),
                string("Fixed LND debug level", Policy),
            ),
            (
                "external_address".into(),
                string("Derived externally advertised address", Topology),
            ),
            (
                "listen_endpoints".into(),
                string("Derived P2P, RPC, and REST endpoints", Topology),
            ),
            (
                "network".into(),
                string("Fixed Bitcoin regtest network", Policy),
            ),
            (
                "seed_backup_policy".into(),
                string("No-seed-backup startup policy", Policy),
            ),
            (
                "tls_extra_domain".into(),
                string("Derived TLS service domain", Topology),
            ),
            (
                "tls_identity".into(),
                string("Instance TLS certificate and key", Secret),
            ),
        ]),
        "cln" => BTreeMap::from([
            (
                "announce_endpoint".into(),
                string("Derived advertised P2P endpoint", Topology),
            ),
            (
                "autoconnect_policy".into(),
                string("Disabled seeker autoconnect", Policy),
            ),
            (
                "bitcoin_retry_timeout".into(),
                integer("Bitcoin RPC retry timeout", Policy),
            ),
            (
                "chain_backend_endpoint".into(),
                string("Linked Bitcoin RPC endpoint", Topology),
            ),
            (
                "chain_backend_credentials".into(),
                string("Linked Bitcoin RPC credentials", Secret),
            ),
            (
                "data_directory".into(),
                string("Core Lightning data directory", Policy),
            ),
            (
                "developer_mode".into(),
                string("Core Lightning developer mode", Policy),
            ),
            (
                "listen_endpoint".into(),
                string("Derived P2P listen endpoint", Topology),
            ),
            (
                "network".into(),
                string("Fixed Bitcoin regtest network", Policy),
            ),
            (
                "log_level".into(),
                string("Fixed Core Lightning log level", Policy),
            ),
            (
                "reconnect_policy".into(),
                string("Disabled automatic reconnect", Policy),
            ),
        ]),
        "cdk" => BTreeMap::from([
            (
                "database_engine".into(),
                string("Topology-selected SQLite or PostgreSQL engine", Topology),
            ),
            ("database_path".into(), string("Mint database path", Policy)),
            (
                "lightning_backend_credentials".into(),
                string("Linked Lightning credentials", Secret),
            ),
            (
                "lightning_backend_endpoint".into(),
                string("Linked Lightning endpoint", Topology),
            ),
            (
                "lightning_backend_kind".into(),
                string("Linked Lightning implementation kind", Topology),
            ),
            (
                "listen_address".into(),
                string("Derived mint listen address", Topology),
            ),
            (
                "max_melt".into(),
                integer("Proofstorm maximum melt amount", Policy),
            ),
            (
                "max_mint".into(),
                integer("Proofstorm maximum mint amount", Policy),
            ),
            (
                "min_melt".into(),
                integer("Proofstorm minimum melt amount", Policy),
            ),
            (
                "min_mint".into(),
                integer("Proofstorm minimum mint amount", Policy),
            ),
            (
                "mnemonic".into(),
                string("Fixed disposable-regtest mint mnemonic", Policy),
            ),
            (
                "public_url".into(),
                string("Derived mint public URL", Topology),
            ),
            (
                "unit".into(),
                string("Proofstorm-selected Cashu unit", Policy),
            ),
            (
                "work_directory".into(),
                string("Mint persistent work directory", Policy),
            ),
        ]),
        "cdk-ldk" => BTreeMap::from([
            (
                "chain_backend_endpoint".into(),
                string("Linked Bitcoin RPC endpoint", Topology),
            ),
            (
                "chain_backend_credentials".into(),
                string("Disposable-regtest Bitcoin RPC credentials", Policy),
            ),
            (
                "database_engine".into(),
                string("Topology-selected SQLite or PostgreSQL engine", Topology),
            ),
            ("database_path".into(), string("Mint database path", Policy)),
            (
                "embedded_lightning_backend".into(),
                string("Pinned embedded LDK Node backend", Policy),
            ),
            (
                "ldk_listen_endpoint".into(),
                string("Derived embedded LDK P2P endpoint", Topology),
            ),
            (
                "ldk_node_mnemonic".into(),
                string("Fixed disposable-regtest LDK seed", Policy),
            ),
            (
                "ldk_storage_directory".into(),
                string("Embedded LDK persistent storage directory", Policy),
            ),
            (
                "listen_address".into(),
                string("Derived mint listen address", Topology),
            ),
            (
                "mint_mnemonic".into(),
                string("Fixed disposable-regtest mint mnemonic", Policy),
            ),
            (
                "network".into(),
                string("Fixed Bitcoin regtest network", Policy),
            ),
            (
                "payment_methods".into(),
                string("Embedded BOLT11 and BOLT12 method set", Policy),
            ),
            (
                "public_url".into(),
                string("Derived mint public URL", Topology),
            ),
            (
                "unit".into(),
                string("Proofstorm-selected Cashu unit", Policy),
            ),
            (
                "work_directory".into(),
                string("Mint and LDK persistent work directory", Policy),
            ),
        ]),
        "cdk-bdk" => BTreeMap::from([
            (
                "bdk_mnemonic".into(),
                string("Fixed disposable-regtest BDK seed", Policy),
            ),
            (
                "bdk_storage_directory".into(),
                string("Embedded BDK persistent storage directory", Policy),
            ),
            (
                "chain_backend_endpoint".into(),
                string("Linked Bitcoin RPC endpoint", Topology),
            ),
            (
                "chain_backend_credentials".into(),
                string("Disposable-regtest Bitcoin RPC credentials", Policy),
            ),
            (
                "confirmation_target".into(),
                integer("Required on-chain confirmations", Policy),
            ),
            (
                "database_engine".into(),
                string("Topology-selected SQLite or PostgreSQL engine", Topology),
            ),
            (
                "listen_address".into(),
                string("Derived mint listen address", Topology),
            ),
            (
                "mint_mnemonic".into(),
                string("Fixed disposable-regtest mint mnemonic", Policy),
            ),
            (
                "network".into(),
                string("Fixed Bitcoin regtest network", Policy),
            ),
            (
                "payment_methods".into(),
                string("Embedded on-chain method set", Policy),
            ),
            (
                "public_url".into(),
                string("Derived mint public URL", Topology),
            ),
            (
                "unit".into(),
                string("Proofstorm-selected Cashu unit", Policy),
            ),
            (
                "work_directory".into(),
                string("Mint persistent work directory", Policy),
            ),
        ]),
        "nutshell" => BTreeMap::from([
            (
                "authentication".into(),
                string(
                    "NUT-21 clear auth and NUT-22 blind auth enabled when an OIDC discovery URL is configured",
                    Policy,
                ),
            ),
            (
                "authentication_database".into(),
                string(
                    "Persistent auth ledger colocated with the selected primary mint storage",
                    Topology,
                ),
            ),
            (
                "backend_kind".into(),
                string(
                    "Topology-selected LndRestWallet or CLNRestWallet backend",
                    Topology,
                ),
            ),
            (
                "database".into(),
                string("Topology-selected SQLite path or PostgreSQL URL", Topology),
            ),
            (
                "data_directory".into(),
                string("Persistent Nutshell mint data directory", Policy),
            ),
            (
                "derivation_path".into(),
                string(
                    "Fixed disposable-regtest sat keyset derivation path",
                    Policy,
                ),
            ),
            (
                "forwarded_allow_ips".into(),
                string("Fixed direct-service proxy allowlist", Policy),
            ),
            (
                "lightning_backend_credentials".into(),
                string(
                    "Linked LND TLS/macaroon or method-restricted CLN rune",
                    Secret,
                ),
            ),
            (
                "lightning_backend_endpoint".into(),
                string("Linked LND or Core Lightning REST endpoint", Topology),
            ),
            (
                "listen_address".into(),
                string("Derived mint listen address", Topology),
            ),
            (
                "management_rpc".into(),
                string("Disabled management RPC service", Policy),
            ),
            (
                "mint_private_key".into(),
                string("Controller-generated mint root key", Secret),
            ),
            (
                "public_url".into(),
                string("Derived mint public URL", Topology),
            ),
            (
                "redis_cache".into(),
                string("Topology-selected Redis cache endpoint", Topology),
            ),
            (
                "redis_cache_credentials".into(),
                string("Controller-generated Redis password", Secret),
            ),
            (
                "tor".into(),
                string("Disabled in the isolated regtest laboratory", Policy),
            ),
            (
                "unit".into(),
                string("Proofstorm-selected sat unit", Policy),
            ),
        ]),
        "cocod-wallet" => BTreeMap::from([
            (
                "data_directory".into(),
                string("Persistent cocod state directory", Policy),
            ),
            (
                "daemon_process".into(),
                string(
                    "Foreground cocod daemon with exclusive native state lease",
                    Policy,
                ),
            ),
            (
                "listener".into(),
                string(
                    "Loopback authenticated HTTP; explicit clients never autostart",
                    Policy,
                ),
            ),
            (
                "client_credential".into(),
                string(
                    "Native administrative bearer credential in private state",
                    Secret,
                ),
            ),
            (
                "storage_size".into(),
                string("Wallet persistent volume size", Policy),
            ),
            (
                "wallet_identity".into(),
                string("Derived wallet component identity", Topology),
            ),
            (
                "wallet_seed".into(),
                string("Native wallet seed material", Secret),
            ),
        ]),
        "nutshell-wallet" | "cdk-cli-wallet" => BTreeMap::from([
            (
                "data_directory".into(),
                string("Persistent wallet data directory", Policy),
            ),
            (
                "idle_process".into(),
                string("Persistent wallet workspace process", Policy),
            ),
            (
                "storage_size".into(),
                string("Wallet persistent volume size", Policy),
            ),
            (
                "wallet_identity".into(),
                string("Derived wallet component identity", Topology),
            ),
            (
                "wallet_seed".into(),
                string("Instance wallet seed material", Secret),
            ),
        ]),
        "attacker-workspace" => BTreeMap::from([
            (
                "idle_process".into(),
                string("Persistent attacker workspace process", Policy),
            ),
            (
                "service_account_access".into(),
                string("Disabled Kubernetes API access", Policy),
            ),
            (
                "workspace_profile".into(),
                string("Pinned adversarial tool workspace", Policy),
            ),
        ]),
        "postgresql" => BTreeMap::from([
            (
                "credentials".into(),
                string("Controller-generated instance credentials", Secret),
            ),
            (
                "data_directory".into(),
                string("PostgreSQL persistent data directory", Policy),
            ),
            (
                "listen_endpoint".into(),
                string("Derived PostgreSQL service endpoint", Topology),
            ),
            (
                "tls_mode".into(),
                string("Isolated-lab transport policy", Policy),
            ),
        ]),
        "redis" => BTreeMap::from([
            (
                "credentials".into(),
                string("Controller-generated cache credentials", Secret),
            ),
            (
                "eviction_policy".into(),
                string("Fixed allkeys-lru cache eviction policy", Policy),
            ),
            (
                "listen_endpoint".into(),
                string("Derived Redis service endpoint", Topology),
            ),
            (
                "persistence".into(),
                string(
                    "Disabled because Redis is non-authoritative cache state",
                    Policy,
                ),
            ),
        ]),
        "keycloak" => BTreeMap::from([
            (
                "administrator_credentials".into(),
                string(
                    "Controller-generated Keycloak administrator credentials",
                    Secret,
                ),
            ),
            (
                "client_id".into(),
                string("Fixed public OIDC client identifier cashu-client", Policy),
            ),
            (
                "database_credentials".into(),
                string("Linked PostgreSQL credentials", Secret),
            ),
            (
                "database_endpoint".into(),
                string("Topology-selected PostgreSQL endpoint", Topology),
            ),
            (
                "realm".into(),
                string("Fixed disposable-lab OIDC realm proofstorm", Policy),
            ),
            (
                "realm_import".into(),
                string("Controller-generated realm and test-user bootstrap", Secret),
            ),
            (
                "test_user_credentials".into(),
                string(
                    "Controller-generated disposable acceptance-user credentials",
                    Secret,
                ),
            ),
        ]),
        _ => BTreeMap::new(),
    }
}

impl ConfigFieldContract {
    fn with_numeric_bounds(mut self, minimum: f64, maximum: f64) -> Self {
        self.minimum = Some(minimum);
        self.maximum = Some(maximum);
        self
    }

    fn with_string_bounds(mut self, minimum: usize, maximum: usize) -> Self {
        self.min_length = Some(minimum);
        self.max_length = Some(maximum);
        self
    }

    fn with_enum_values(mut self, values: &[&str]) -> Self {
        self.enum_values = values.iter().map(|value| json!(value)).collect();
        self
    }
}

fn service_conditions(
    has_dependencies: bool,
    has_credential_projection: bool,
) -> BTreeSet<ComponentConditionType> {
    let mut conditions = BTreeSet::from([
        ComponentConditionType::WorkloadReady,
        ComponentConditionType::StorageReady,
        ComponentConditionType::ServiceReady,
        ComponentConditionType::ProtocolReady,
        ComponentConditionType::ComponentReady,
        ComponentConditionType::ExperimentControllable,
    ]);
    if has_dependencies {
        conditions.insert(ComponentConditionType::DependenciesReady);
    }
    if has_credential_projection {
        conditions.insert(ComponentConditionType::CredentialsReady);
    }
    conditions
}

fn ephemeral_service_conditions() -> BTreeSet<ComponentConditionType> {
    let mut conditions = service_conditions(false, false);
    conditions.remove(&ComponentConditionType::StorageReady);
    conditions
}

fn ephemeral_dependency_service_conditions() -> BTreeSet<ComponentConditionType> {
    let mut conditions = service_conditions(false, true);
    conditions.remove(&ComponentConditionType::StorageReady);
    conditions
}

fn contract(
    id: &str,
    kind: ComponentKind,
    config_version: &str,
    mut config_fields: BTreeMap<String, ConfigFieldContract>,
    service_ports: BTreeMap<String, u16>,
    execution_state_contract: &str,
    applicable_conditions: BTreeSet<ComponentConditionType>,
) -> ComponentBackendContract {
    config_fields.extend(managed_config_fields(id));
    let (execution_mounts, execution_environment) = execution_contract(id);
    let (workload_kind, storage_requirements) = observation_contract(id);
    let config_rules = if matches!(id, "cdk" | "cdk-ldk" | "cdk-bdk") {
        vec![
            ConfigRule::LessThanOrEqual {
                minimum_field: "min_mint_sat".into(),
                maximum_field: "max_mint_sat".into(),
            },
            ConfigRule::LessThanOrEqual {
                minimum_field: "min_melt_sat".into(),
                maximum_field: "max_melt_sat".into(),
            },
        ]
    } else if id == "nutshell" {
        vec![
            ConfigRule::LessThanOrEqual {
                minimum_field: "max_mint_sat".into(),
                maximum_field: "max_balance_sat".into(),
            },
            ConfigRule::LessThanOrEqual {
                minimum_field: "max_melt_sat".into(),
                maximum_field: "max_balance_sat".into(),
            },
        ]
    } else {
        vec![]
    };
    ComponentBackendContract {
        id: id.into(),
        kind,
        config_version: config_version.into(),
        config_fields,
        config_rules,
        service_ports,
        execution_state_contract: execution_state_contract.into(),
        execution_mounts,
        execution_environment,
        workload_kind,
        storage_requirements,
        protocol_probe: protocol_probe_contract(id),
        condition_reasons: default_condition_reasons(&applicable_conditions),
        condition_aggregation: default_condition_aggregation(&applicable_conditions),
        applicable_conditions,
        operation_admission: default_operation_admission(),
    }
}

fn protocol_probe_contract(backend: &str) -> Option<ProtocolProbeContract> {
    match backend {
        "bitcoin-core" | "lnd" => Some(ProtocolProbeContract::Tcp {
            port_name: "rpc".into(),
        }),
        "cln" => Some(ProtocolProbeContract::Tcp {
            port_name: "p2p".into(),
        }),
        "postgresql" => Some(ProtocolProbeContract::Tcp {
            port_name: "postgres".into(),
        }),
        "redis" => Some(ProtocolProbeContract::Tcp {
            port_name: "redis".into(),
        }),
        "keycloak" => Some(ProtocolProbeContract::HttpGet {
            port_name: "http".into(),
            path: "/realms/proofstorm/.well-known/openid-configuration".into(),
        }),
        "cdk" | "cdk-ldk" | "cdk-bdk" => Some(ProtocolProbeContract::HttpGet {
            port_name: "http".into(),
            path: "/v1/info".into(),
        }),
        // Nutshell applies its global elastic-expiry rate limiter to /v1/info.
        // A recurring remote HTTP probe would therefore consume application
        // quota and eventually lock out its own stable probe identity. The
        // component-local readiness probe verifies HTTP; this probe verifies
        // the Service-DNS and network path without spending request quota.
        "nutshell" => Some(ProtocolProbeContract::Tcp {
            port_name: "http".into(),
        }),
        _ => None,
    }
}

fn compile_protocol_probe(
    probe: &ProtocolProbeContract,
    ports: &BTreeMap<String, u16>,
) -> Result<ProtocolProbePlan, String> {
    let resolve_port = |name: &str| {
        ports.get(name).copied().ok_or_else(|| {
            format!("backend_protocol_probe_port_missing: service port {name:?} is not declared")
        })
    };
    match probe {
        ProtocolProbeContract::Tcp { port_name } => Ok(ProtocolProbePlan::Tcp {
            port: resolve_port(port_name)?,
        }),
        ProtocolProbeContract::HttpGet { port_name, path } => Ok(ProtocolProbePlan::HttpGet {
            port: resolve_port(port_name)?,
            path: path.clone(),
        }),
    }
}

fn observation_contract(
    backend: &str,
) -> (WorkloadControllerKind, Vec<StorageRequirementTemplate>) {
    let stateful_data = || StorageRequirementTemplate::StatefulClaimTemplate {
        template_name: "data".into(),
    };
    let component_data = || StorageRequirementTemplate::ComponentClaim {
        suffix: "-data".into(),
    };
    match backend {
        "bitcoin-core" | "lnd" | "cln" | "postgresql" => {
            (WorkloadControllerKind::StatefulSet, vec![stateful_data()])
        }
        "cdk" | "cdk-ldk" | "cdk-bdk" | "nutshell" | "nutshell-wallet" | "cdk-cli-wallet"
        | "cocod-wallet" => (WorkloadControllerKind::Deployment, vec![component_data()]),
        _ => (WorkloadControllerKind::Deployment, vec![]),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the built-in execution contracts are clearer when each backend mount is declared together"
)]
fn execution_contract(
    backend: &str,
) -> (
    Vec<ExecutionMountTemplateContract>,
    BTreeMap<String, String>,
) {
    use ExecutionStorageTemplateSource as Source;

    let binding =
        |name: &str, mount_path: &str, read_only: bool, source| ExecutionMountTemplateContract {
            name: name.into(),
            mount_path: mount_path.into(),
            read_only,
            requirement: ExecutionMountRequirement::Required,
            source,
        };
    let alternative_binding =
        |name: &str, mount_path: &str, read_only: bool, group: &str, source| {
            ExecutionMountTemplateContract {
                name: name.into(),
                mount_path: mount_path.into(),
                read_only,
                requirement: ExecutionMountRequirement::AtLeastOne {
                    group: group.into(),
                },
                source,
            }
        };
    match backend {
        "bitcoin-core" => (
            vec![binding(
                "data",
                "/home/bitcoin/.bitcoin",
                false,
                Source::StatefulData,
            )],
            BTreeMap::from([("HOME".into(), "/home/bitcoin".into())]),
        ),
        "lnd" => (
            vec![binding(
                "data",
                "/home/lnd/.lnd",
                false,
                Source::StatefulData,
            )],
            BTreeMap::from([("HOME".into(), "/home/lnd".into())]),
        ),
        "cln" => (
            vec![binding(
                "data",
                "/home/cln/.lightning",
                false,
                Source::StatefulData,
            )],
            BTreeMap::from([("HOME".into(), "/home/cln".into())]),
        ),
        "postgresql" => (
            vec![binding(
                "data",
                "/var/lib/postgresql/data",
                false,
                Source::StatefulData,
            )],
            BTreeMap::from([("PGDATA".into(), "/var/lib/postgresql/data/pgdata".into())]),
        ),
        "cdk" => (
            vec![
                binding("config", "/config", true, Source::ComponentConfig),
                binding("data", "/app/data", false, Source::ComponentPersistentData),
                alternative_binding(
                    "lnd",
                    "/lnd",
                    true,
                    "payment-backend",
                    Source::LinkedStatefulData {
                        link_kind: crate::LinkKind::PaymentBackend,
                        binding: crate::DependencyBinding::Payment {
                            method: crate::PaymentMethod::Bolt11,
                            unit: "sat".into(),
                        },
                        target_implementation: "lnd".into(),
                    },
                ),
                alternative_binding(
                    "cln",
                    "/cln",
                    true,
                    "payment-backend",
                    Source::LinkedStatefulData {
                        link_kind: crate::LinkKind::PaymentBackend,
                        binding: crate::DependencyBinding::Payment {
                            method: crate::PaymentMethod::Bolt11,
                            unit: "sat".into(),
                        },
                        target_implementation: "cln".into(),
                    },
                ),
            ],
            BTreeMap::from([
                ("CDK_MINTD_WORK_DIR".into(), "/app/data".into()),
                ("HOME".into(), "/app/data".into()),
            ]),
        ),
        "cdk-ldk" | "cdk-bdk" => (
            vec![
                binding("config", "/config", true, Source::ComponentConfig),
                binding("data", "/app/data", false, Source::ComponentPersistentData),
            ],
            BTreeMap::from([
                ("CDK_MINTD_WORK_DIR".into(), "/app/data".into()),
                ("HOME".into(), "/app/data".into()),
            ]),
        ),
        "nutshell" => (
            vec![
                binding("data", "/app/data", false, Source::ComponentPersistentData),
                alternative_binding(
                    "lnd",
                    "/lnd",
                    true,
                    "payment-backend",
                    Source::LinkedStatefulData {
                        link_kind: crate::LinkKind::PaymentBackend,
                        binding: crate::DependencyBinding::Payment {
                            method: crate::PaymentMethod::Bolt11,
                            unit: "sat".into(),
                        },
                        target_implementation: "lnd".into(),
                    },
                ),
                alternative_binding(
                    "cln",
                    "/cln",
                    true,
                    "payment-backend",
                    Source::LinkedStatefulData {
                        link_kind: crate::LinkKind::PaymentBackend,
                        binding: crate::DependencyBinding::Payment {
                            method: crate::PaymentMethod::Bolt11,
                            unit: "sat".into(),
                        },
                        target_implementation: "cln".into(),
                    },
                ),
            ],
            BTreeMap::from([
                ("CASHU_DIR".into(), "/app/data".into()),
                ("HOME".into(), "/app/data".into()),
            ]),
        ),
        "cocod-wallet" => (
            vec![binding(
                "wallet",
                "/wallet",
                false,
                Source::ComponentPersistentData,
            )],
            BTreeMap::from([
                ("HOME".into(), "/wallet".into()),
                ("PROOFSTORM_WALLET".into(), "{component_id}".into()),
                ("COCOD_URL".into(), "http://127.0.0.1:62626".into()),
                ("COCOD_LISTEN_HOST".into(), "127.0.0.1".into()),
                ("COCOD_LISTEN_PORT".into(), "62626".into()),
            ]),
        ),
        "nutshell-wallet" | "cdk-cli-wallet" => (
            vec![binding(
                "wallet",
                "/wallet",
                false,
                Source::ComponentPersistentData,
            )],
            BTreeMap::from([
                ("HOME".into(), "/wallet".into()),
                ("PROOFSTORM_WALLET".into(), "{component_id}".into()),
            ]),
        ),
        "attacker-workspace" => (vec![], BTreeMap::from([("HOME".into(), "/tmp".into())])),
        _ => (vec![], BTreeMap::new()),
    }
}

fn default_condition_reasons(
    applicable: &BTreeSet<ComponentConditionType>,
) -> BTreeMap<ComponentConditionType, BTreeSet<ComponentConditionReason>> {
    use ComponentConditionReason as Reason;
    use ComponentConditionType as Condition;

    let reasons = BTreeMap::from([
        (
            Condition::WorkloadReady,
            BTreeSet::from([
                Reason::WorkloadAvailable,
                Reason::WorkloadUnavailable,
                Reason::ImagePullFailed,
                Reason::ImagePullBackoff,
                Reason::InvalidImageName,
                Reason::ContainerConfigError,
                Reason::ContainerCrashLoop,
                Reason::ContainerStartError,
                Reason::ContainerExited,
                Reason::PodUnschedulable,
                Reason::NotObserved,
                Reason::StaleRevision,
                Reason::IntentionallyStopped,
            ]),
        ),
        (
            Condition::StorageReady,
            BTreeSet::from([
                Reason::StorageBound,
                Reason::StoragePending,
                Reason::NotObserved,
            ]),
        ),
        (
            Condition::CredentialsReady,
            BTreeSet::from([
                Reason::CredentialsProjected,
                Reason::CredentialsMissing,
                Reason::NotObserved,
            ]),
        ),
        (
            Condition::ServiceReady,
            BTreeSet::from([
                Reason::EndpointsReady,
                Reason::EndpointsMissing,
                Reason::NotObserved,
            ]),
        ),
        (
            Condition::ProtocolReady,
            BTreeSet::from([
                Reason::ProtocolResponding,
                Reason::ProtocolProbePending,
                Reason::ProtocolProbeFailed,
                Reason::IntentionallyStopped,
            ]),
        ),
        (
            Condition::DependenciesReady,
            BTreeSet::from([
                Reason::DependenciesSatisfied,
                Reason::DependenciesUnsatisfied,
                Reason::NotObserved,
            ]),
        ),
        (
            Condition::ComponentReady,
            BTreeSet::from([
                Reason::ComponentOperational,
                Reason::ComponentNotOperational,
                Reason::IntentionallyStopped,
                Reason::StaleRevision,
            ]),
        ),
        (
            Condition::ExperimentControllable,
            BTreeSet::from([Reason::ControlAvailable, Reason::ControlUnavailable]),
        ),
    ]);
    reasons
        .into_iter()
        .filter(|(condition, _)| applicable.contains(condition))
        .collect()
}

fn default_condition_aggregation(
    applicable: &BTreeSet<ComponentConditionType>,
) -> Vec<ConditionAggregationContract> {
    let component = [
        ComponentConditionType::WorkloadReady,
        ComponentConditionType::StorageReady,
        ComponentConditionType::ServiceReady,
        ComponentConditionType::ProtocolReady,
        ComponentConditionType::DependenciesReady,
    ]
    .into_iter()
    .filter(|condition| applicable.contains(condition))
    .collect::<BTreeSet<_>>();
    vec![ConditionAggregationContract {
        condition: ComponentConditionType::ComponentReady,
        all_of: component,
    }]
}

fn default_operation_admission() -> Vec<OperationAdmissionContract> {
    use OperationClass as Operation;
    use ReadinessPrerequisite as Requirement;

    let admission = |operation, prerequisites| OperationAdmissionContract {
        operation,
        prerequisites,
    };
    vec![
        admission(
            Operation::Inspect,
            BTreeSet::from([Requirement::AcceptedIdentity]),
        ),
        admission(
            Operation::Start,
            BTreeSet::from([Requirement::AcceptedIdentity, Requirement::Storage]),
        ),
        admission(
            Operation::Stop,
            BTreeSet::from([Requirement::WorkloadIdentity]),
        ),
        admission(
            Operation::Restart,
            BTreeSet::from([Requirement::WorkloadIdentity]),
        ),
        admission(
            Operation::NativeExec,
            BTreeSet::from([Requirement::ExecutionContext]),
        ),
        admission(
            Operation::NetworkHeal,
            BTreeSet::from([Requirement::FaultIdentity]),
        ),
        admission(
            Operation::PeerChannelMutation,
            BTreeSet::from([Requirement::Dependencies, Requirement::Protocol]),
        ),
        admission(
            Operation::WalletPayment,
            BTreeSet::from([Requirement::Dependencies, Requirement::Protocol]),
        ),
        admission(
            Operation::Authentication,
            BTreeSet::from([Requirement::Dependencies, Requirement::Protocol]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlClass, LOCK_API_VERSION, default_catalog, resolve_lock};

    fn component(id: &str, implementation: &str, kind: ComponentKind) -> ComponentSpec {
        ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: match implementation {
                "bitcoin-core" => "bitcoin-core/30/v1",
                "lnd" => "lnd/0.20/v1",
                "cln" => "cln/26.06/v1",
                "cdk" => "cdk-mintd/0.18/v1",
                "cdk-ldk" => "cdk-mintd-ldk/0.18/v1",
                "cdk-bdk" => "cdk-mintd-bdk/0.18/v1",
                "nutshell" => "nutshell-mint/0.20/v1",
                "postgresql" => "postgresql/17/v1",
                "redis" => "redis/8.10/v1",
                "keycloak" => "keycloak/25/v1",
                "nutshell-wallet" => "nutshell-wallet/0.20/v1",
                "cdk-cli-wallet" => "cdk-cli-wallet/0.18/v1",
                "cocod-wallet" => "cocod-wallet/0.0.17/v1",
                "attacker-workspace" => "attacker-workspace/0.1/v1",
                _ => panic!("unknown test implementation {implementation:?}"),
            }
            .into(),
            control: ControlClass::Laboratory,
            config: BTreeMap::new(),
        }
    }

    #[test]
    fn defaults_are_canonical_and_do_not_replace_explicit_values() {
        let registry = default_backend_registry();
        let mut chain = component("chain", "bitcoin-core", ComponentKind::Bitcoin);
        let defaulted = registry
            .resolve_effective_component(&chain)
            .expect("resolve defaults");
        assert_eq!(defaulted.config["txindex"], json!(true));
        assert_eq!(defaulted.config["fallback_fee"], json!(0.0002));

        chain.config.insert("txindex".into(), json!(false));
        let explicit = registry
            .resolve_effective_component(&chain)
            .expect("preserve explicit value");
        assert_eq!(explicit.config["txindex"], json!(false));

        let lnd = registry
            .resolve_effective_component(&component("alice", "lnd", ComponentKind::Lightning))
            .expect("default alias");
        assert_eq!(lnd.config["alias"], json!("alice"));

        let cdk = registry
            .resolve_effective_component(&component("mint", "cdk", ComponentKind::Mint))
            .expect("default CDK policy");
        assert_eq!(cdk.config["input_fee_ppk"], json!(100));
        assert_eq!(cdk.config["use_keyset_v2"], json!(true));
        assert_eq!(cdk.config["mint_quote_ttl_seconds"], json!(600));
        assert_eq!(cdk.config["melt_quote_ttl_seconds"], json!(120));
        assert_eq!(cdk.config["description_long"], json!(""));
        assert_eq!(cdk.config["enable_info_page"], json!(false));
        assert_eq!(cdk.config["http_cache_ttl_seconds"], json!(60));
        assert_eq!(cdk.config["http_cache_tti_seconds"], json!(60));
        assert_eq!(cdk.config["max_inputs"], json!(1_000));
        assert_eq!(cdk.config["max_outputs"], json!(1_000));

        let bdk = registry
            .resolve_effective_component(&component("mint", "cdk-bdk", ComponentKind::Mint))
            .expect("default BDK limits");
        assert_eq!(bdk.config["min_mint_sat"], json!(1_000));
        assert_eq!(bdk.config["max_mint_sat"], json!(1_000_000));

        let nutshell = registry
            .resolve_effective_component(&component("mint", "nutshell", ComponentKind::Mint))
            .expect("default Nutshell authentication policy");
        assert_eq!(nutshell.config["oidc_discovery_url"], json!(""));
        assert_eq!(nutshell.config["oidc_client_id"], json!("cashu-client"));
        assert_eq!(nutshell.config["auth_rate_limit_per_minute"], json!(5));
        assert_eq!(nutshell.config["auth_max_blind_tokens"], json!(100));

        let keycloak = registry
            .resolve_effective_component(&component(
                "identity",
                "keycloak",
                ComponentKind::IdentityProvider,
            ))
            .expect("default Keycloak policy");
        assert_eq!(keycloak.config["access_token_lifespan_seconds"], json!(300));

        let mut invalid = component("mint", "cdk", ComponentKind::Mint);
        invalid.config.insert("min_mint_sat".into(), json!(10));
        invalid.config.insert("max_mint_sat".into(), json!(9));
        assert!(
            registry
                .validate_component_config(&invalid)
                .expect_err("ordered mint bounds")
                .starts_with("config_order_violation:")
        );
    }

    #[test]
    fn backend_contract_is_the_catalog_schema_and_validation_source() {
        let registry = default_backend_registry();
        let catalog = default_catalog();
        for entry in &catalog.entries {
            let backend = registry.require(&entry.id).expect("catalog backend");
            assert_eq!(entry.config_version, backend.config_version);
            assert_eq!(entry.config_schema, backend.config_schema());
            let effective = registry
                .resolve_effective_component(&component("sample", &entry.id, entry.kind))
                .expect("default configuration resolves");
            let typed = EffectiveComponentConfig::try_from_component(&effective)
                .expect("default configuration is representable by its native type");
            assert_eq!(
                serde_json::to_value(typed).expect("typed config serializes")["implementation"],
                entry.id
            );
        }

        let bitcoin_schema = registry.config_schema("bitcoin-core").expect("schema");
        assert_eq!(
            bitcoin_schema["properties"]["txindex"]["x-proofstorm-classification"],
            json!("agent_authorable")
        );
        assert_eq!(
            bitcoin_schema["properties"]["fallback_fee"]["minimum"],
            json!(0.0)
        );
        assert_eq!(
            bitcoin_schema["properties"]["fallback_fee"]["default"],
            json!(0.0002)
        );
        assert!(
            bitcoin_schema["properties"]
                .get("rpc_credentials")
                .is_none()
        );
        assert_eq!(
            bitcoin_schema["x-proofstorm-managed-settings"]["rpc_credentials"]["x-proofstorm-classification"],
            json!("runtime_policy")
        );
        assert_eq!(
            bitcoin_schema["x-proofstorm-managed-settings"]["rpc_credentials"]["readOnly"],
            json!(true)
        );
        assert!(
            bitcoin_schema["x-proofstorm-managed-settings"]["rpc_credentials"]
                .get("default")
                .is_none()
        );
    }

    #[test]
    fn backend_config_validation_is_field_addressed_and_data_driven() {
        let registry = default_backend_registry();
        let mut bitcoin = component("chain", "bitcoin-core", ComponentKind::Bitcoin);
        bitcoin.config.insert("mystery".into(), json!(true));
        assert_eq!(
            registry
                .validate_component_config(&bitcoin)
                .expect_err("unknown field"),
            "config_unknown_field: component \"chain\" field /config/mystery: field is not declared by the backend configuration contract"
        );

        bitcoin.config.clear();
        bitcoin.config.insert("txindex".into(), json!("yes"));
        assert!(
            registry
                .validate_component_config(&bitcoin)
                .expect_err("wrong type")
                .starts_with("config_wrong_type: component \"chain\" field /config/txindex:")
        );

        bitcoin.config.clear();
        bitcoin.config.insert("fallback_fee".into(), json!(-0.1));
        assert!(
            registry
                .validate_component_config(&bitcoin)
                .expect_err("bound")
                .starts_with(
                    "config_numeric_bound_violation: component \"chain\" field /config/fallback_fee:"
                )
        );

        bitcoin.config.clear();
        bitcoin
            .config
            .insert("rpc_credentials".into(), json!("agent-secret"));
        assert!(
            registry
                .validate_component_config(&bitcoin)
                .expect_err("managed setting")
                .starts_with(
                    "config_managed_field: component \"chain\" field /config/rpc_credentials:"
                )
        );

        let mut nutshell = component("mint", "nutshell", ComponentKind::Mint);
        nutshell.config.insert(
            "oidc_discovery_url".into(),
            json!("https://issuer.example/realm"),
        );
        assert!(
            registry
                .validate_component_config(&nutshell)
                .expect_err("malformed discovery URL")
                .starts_with(
                    "config_oidc_discovery_url_invalid: component \"mint\" field /config/oidc_discovery_url:"
                )
        );
        nutshell.config.insert(
            "oidc_discovery_url".into(),
            json!("https://issuer.example/realm/.well-known/openid-configuration"),
        );
        registry
            .validate_component_config(&nutshell)
            .expect("valid discovery URL");
    }

    #[test]
    fn generic_contract_rules_drive_schema_and_refusal() {
        let mut backend = default_backend_registry()
            .require("cdk")
            .expect("CDK backend")
            .clone();
        backend.config_fields.insert(
            "auth_mode".into(),
            ConfigFieldContract {
                description: "Authentication mode".into(),
                value_kind: ConfigValueKind::String,
                classification: ConfigSettingClass::AgentAuthorable,
                default: None,
                enum_values: vec![json!("clear"), json!("blind")],
                minimum: None,
                maximum: None,
                min_length: None,
                max_length: None,
                required: false,
            },
        );
        backend.config_fields.insert(
            "identity_provider".into(),
            ConfigFieldContract {
                description: "Identity provider component".into(),
                value_kind: ConfigValueKind::String,
                classification: ConfigSettingClass::ImportedSecretReference,
                default: None,
                enum_values: vec![],
                minimum: None,
                maximum: None,
                min_length: Some(1),
                max_length: Some(63),
                required: false,
            },
        );
        backend.config_rules.push(ConfigRule::RequiredWhen {
            field: "auth_mode".into(),
            equals: json!("clear"),
            required_field: "identity_provider".into(),
        });
        let schema = backend.config_schema();
        assert_eq!(
            schema["allOf"][0]["then"]["required"][0],
            "identity_provider"
        );

        let registry = BackendContractRegistry::try_new([backend]).expect("test registry");
        let mut cdk = component("mint", "cdk", ComponentKind::Mint);
        cdk.config.insert("auth_mode".into(), json!("clear"));
        assert!(
            registry
                .validate_component_config(&cdk)
                .expect_err("conditional requirement")
                .starts_with("config_conditional_required_field_missing:")
        );
    }

    #[test]
    fn registry_refuses_duplicate_missing_and_kind_mismatch() {
        let first = contract(
            "duplicate",
            ComponentKind::Bitcoin,
            "duplicate/v1",
            BTreeMap::new(),
            BTreeMap::new(),
            "proofstorm/test-state/v1",
            BTreeSet::new(),
        );
        let error = BackendContractRegistry::try_new([first.clone(), first])
            .expect_err("duplicate backend must refuse");
        assert!(error.starts_with("backend_registry_duplicate:"));

        let registry = default_backend_registry();
        assert!(
            registry
                .require("missing")
                .expect_err("missing backend must refuse")
                .starts_with("backend_registry_missing:")
        );
        let error = registry
            .resolve_effective_component(&component("wrong", "bitcoin-core", ComponentKind::Wallet))
            .expect_err("kind mismatch must refuse");
        assert!(error.starts_with("backend_kind_mismatch:"));

        let mut managed_default = registry
            .require("bitcoin-core")
            .expect("bitcoin backend")
            .clone();
        managed_default
            .config_fields
            .get_mut("rpc_credentials")
            .expect("managed credential field")
            .default = Some(ConfigDefault::Literal(json!("must-not-leak")));
        assert!(
            BackendContractRegistry::try_new([managed_default])
                .expect_err("managed defaults must refuse")
                .starts_with("backend_managed_config_default_forbidden:")
        );

        let mut invalid_selector = registry.require("cdk").expect("CDK backend").clone();
        let ExecutionStorageTemplateSource::LinkedStatefulData { binding, .. } =
            &mut invalid_selector.execution_mounts[2].source
        else {
            panic!("CDK linked mount");
        };
        *binding = crate::DependencyBinding::Chain {
            network: crate::BitcoinNetwork::Regtest,
        };
        assert!(
            BackendContractRegistry::try_new([invalid_selector])
                .expect_err("mismatched execution selector must refuse")
                .starts_with("backend_execution_selector_incompatible:")
        );

        let mut duplicate_mount = registry.require("cdk").expect("CDK backend").clone();
        duplicate_mount
            .execution_mounts
            .push(duplicate_mount.execution_mounts[2].clone());
        assert!(
            BackendContractRegistry::try_new([duplicate_mount])
                .expect_err("duplicate mount identity must refuse")
                .starts_with("backend_execution_mount_collision:")
        );
    }

    #[test]
    fn native_exec_admission_does_not_require_protocol_readiness() {
        let registry = default_backend_registry();
        let backend = registry.require("bitcoin-core").expect("bitcoin backend");
        assert!(
            backend
                .applicable_conditions
                .iter()
                .all(|condition| backend.condition_reasons.contains_key(condition))
        );
        let native = backend
            .operation_admission
            .iter()
            .find(|contract| contract.operation == OperationClass::NativeExec)
            .expect("native exec contract");
        assert_eq!(
            native.prerequisites,
            BTreeSet::from([ReadinessPrerequisite::ExecutionContext])
        );
        assert!(
            !native
                .prerequisites
                .contains(&ReadinessPrerequisite::Protocol)
        );
    }

    #[test]
    fn condition_applicability_distinguishes_dependencies_from_credentials() {
        let registry = default_backend_registry();
        let bitcoin = registry.require("bitcoin-core").expect("bitcoin backend");
        let lnd = registry.require("lnd").expect("LND backend");
        let cdk = registry.require("cdk").expect("CDK backend");
        let wallet = registry.require("nutshell-wallet").expect("wallet backend");

        assert!(
            !bitcoin
                .applicable_conditions
                .contains(&ComponentConditionType::DependenciesReady)
        );
        assert!(
            lnd.applicable_conditions
                .contains(&ComponentConditionType::DependenciesReady)
        );
        assert!(
            !lnd.applicable_conditions
                .contains(&ComponentConditionType::CredentialsReady)
        );
        assert!(
            cdk.applicable_conditions
                .contains(&ComponentConditionType::DependenciesReady)
        );
        assert!(
            cdk.applicable_conditions
                .contains(&ComponentConditionType::CredentialsReady)
        );
        assert!(
            !wallet
                .applicable_conditions
                .contains(&ComponentConditionType::ServiceReady)
        );

        for backend in [bitcoin, lnd, cdk, wallet] {
            assert_eq!(
                backend
                    .applicable_conditions
                    .contains(&ComponentConditionType::ProtocolReady),
                backend.protocol_probe.is_some(),
                "protocol applicability must have an executable probe contract"
            );
            assert_eq!(backend.condition_aggregation.len(), 1);
            assert_eq!(
                backend.condition_aggregation[0].condition,
                ComponentConditionType::ComponentReady
            );
            assert!(backend.applicable_conditions.iter().all(|condition| {
                backend
                    .condition_reasons
                    .get(condition)
                    .is_some_and(|reasons| !reasons.is_empty())
            }));
        }
    }

    #[test]
    fn rate_limited_nutshell_uses_a_quota_free_remote_probe() {
        let registry = default_backend_registry();
        let nutshell = registry.require("nutshell").expect("Nutshell backend");

        assert_eq!(
            nutshell.protocol_probe,
            Some(ProtocolProbeContract::Tcp {
                port_name: "http".into(),
            }),
            "remote health traffic must not consume Nutshell's HTTP rate limit"
        );
    }

    #[test]
    fn compiled_contract_uses_current_lock_rollout_identity() {
        let component = component("chain", "bitcoin-core", ComponentKind::Bitcoin);
        let lab = crate::LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "compile-contract".into(),
            components: vec![component.clone()],
            links: vec![],
            policy: crate::LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, crate::default_catalog()).expect("resolve current lock");
        assert_eq!(lock.api_version, LOCK_API_VERSION);
        let entry = lock.entries[0].clone();
        let expected = entry.rollout_digest.clone();
        let compiled = default_backend_registry()
            .compile_contract(&ComponentPlanInput {
                instance_key: "instance-key".into(),
                revision_digest: "sha256:revision".into(),
                component,
                lock: entry,
                relevant_links: vec![],
                linked_targets: BTreeMap::new(),
                linked_state: BTreeMap::new(),
            })
            .expect("compile contract");
        assert_eq!(compiled.rollout_digest, expected);
        assert_eq!(compiled.revision_digest, "sha256:revision");
        assert!(matches!(
            compiled.effective_config,
            EffectiveComponentConfig::BitcoinCore(BitcoinCoreConfig { txindex: true, .. })
        ));
        assert_eq!(compiled.execution_context.image, lock.entries[0].image);
        assert_eq!(compiled.target_descriptor.ports["rpc"], 18_443);
    }

    #[test]
    fn compiled_contract_refuses_a_lock_from_an_older_backend_config_contract() {
        let mut component = component("mint", "cdk", ComponentKind::Mint);
        component.control = ControlClass::Target;
        let lab = crate::LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "stale-cdk-lock".into(),
            components: vec![component.clone()],
            links: vec![],
            policy: crate::LabPolicy::default(),
        };
        let mut lock = resolve_lock(&lab, crate::default_catalog()).expect("resolve current lock");
        lock.entries[0].config_version = "cdk-mintd/0.17/v1".into();

        let error = default_backend_registry()
            .compile_contract(&ComponentPlanInput {
                instance_key: "instance-key".into(),
                revision_digest: "sha256:revision".into(),
                component,
                lock: lock.entries[0].clone(),
                relevant_links: vec![],
                linked_targets: BTreeMap::new(),
                linked_state: BTreeMap::new(),
            })
            .expect_err("a stale CDK contract must not be reinterpreted as 0.18");

        assert!(error.starts_with("backend_lock_config_version_mismatch:"));
        assert!(error.contains("cdk-mintd/0.17/v1"));
        assert!(error.contains("cdk-mintd/0.18/v1"));
    }

    #[test]
    fn execution_mount_templates_resolve_one_exact_typed_binding() {
        let backend = default_backend_registry()
            .require("cdk")
            .expect("CDK backend")
            .clone();
        let payment = |id: &str, method| LinkSpec {
            id: id.into(),
            kind: crate::LinkKind::PaymentBackend,
            from: "mint".into(),
            to: format!("{id}-node"),
            binding: Some(crate::DependencyBinding::Payment {
                method,
                unit: "sat".into(),
            }),
        };
        let mut links = vec![
            payment("bolt11", crate::PaymentMethod::Bolt11),
            payment("bolt12", crate::PaymentMethod::Bolt12),
        ];
        let target =
            |component_id: &str, backend_id: &str, version: &str| TargetDescriptorContract {
                component_id: component_id.into(),
                kind: ComponentKind::Lightning,
                backend_id: backend_id.into(),
                version: version.into(),
                ports: BTreeMap::new(),
            };
        let mut targets = BTreeMap::from([
            ("bolt11".into(), target("bolt11-node", "lnd", "0.20.0-beta")),
            ("bolt12".into(), target("bolt12-node", "lnd", "0.20.0-beta")),
        ]);
        let mounts = resolve_execution_mounts(&backend, "mint", &links, &targets)
            .expect("unselected methods and implementations do not collide");
        assert!(matches!(
            mounts[2].source,
            ExecutionStorageSource::LinkedStatefulData { ref link_id }
                if link_id == "bolt11"
        ));

        let cln_link = payment("cln-bolt11", crate::PaymentMethod::Bolt11);
        let cln_targets = BTreeMap::from([(
            "cln-bolt11".into(),
            target("cln-bolt11-node", "cln", "26.06.7"),
        )]);
        let cln_mounts = resolve_execution_mounts(&backend, "mint", &[cln_link], &cln_targets)
            .expect("CLN is an alternative exact payment-state mount");
        assert_eq!(cln_mounts[2].name, "cln");

        let error = resolve_execution_mounts(&backend, "mint", &[], &BTreeMap::new())
            .expect_err("at least one payment backend is required");
        assert!(error.starts_with("backend_execution_binding_group_missing:"));

        links.push(payment("bolt11-secondary", crate::PaymentMethod::Bolt11));
        targets.insert(
            "bolt11-secondary".into(),
            target("bolt11-secondary-node", "lnd", "0.20.0-beta"),
        );
        let error = resolve_execution_mounts(&backend, "mint", &links, &targets)
            .expect_err("duplicate exact selectors must refuse");
        assert!(error.starts_with("backend_execution_binding_ambiguous:"));
        assert!(error.contains("bolt11-secondary"));
    }

    #[test]
    fn linked_execution_mount_refuses_non_singular_storage_projection() {
        let mut mint = component("mint", "cdk", ComponentKind::Mint);
        mint.control = ControlClass::Target;
        let lightning = component("lightning", "lnd", ComponentKind::Lightning);
        let link = LinkSpec {
            id: "mint-bolt11".into(),
            kind: crate::LinkKind::PaymentBackend,
            from: "mint".into(),
            to: "lightning".into(),
            binding: Some(crate::DependencyBinding::Payment {
                method: crate::PaymentMethod::Bolt11,
                unit: "sat".into(),
            }),
        };
        let lab = crate::LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "storage-cardinality".into(),
            components: vec![mint.clone(), lightning],
            links: vec![link.clone()],
            policy: crate::LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, crate::default_catalog()).expect("lock");
        let error = default_backend_registry()
            .compile_contract(&ComponentPlanInput {
                instance_key: "instance-key".into(),
                revision_digest: "sha256:revision".into(),
                component: mint,
                lock: lock
                    .entries
                    .iter()
                    .find(|entry| entry.component_id == "mint")
                    .expect("mint lock")
                    .clone(),
                relevant_links: vec![link],
                linked_targets: BTreeMap::from([(
                    "mint-bolt11".into(),
                    TargetDescriptorContract {
                        component_id: "lightning".into(),
                        kind: ComponentKind::Lightning,
                        backend_id: "lnd".into(),
                        version: "0.20.0-beta".into(),
                        ports: BTreeMap::from([("rpc".into(), 10_009)]),
                    },
                )]),
                linked_state: BTreeMap::from([(
                    "mint-bolt11".into(),
                    LinkedStateObservationContract {
                        component_id: "lightning".into(),
                        state_contract: "proofstorm/lnd-state/v1".into(),
                        storage: vec![
                            StorageObservationContract {
                                claim_name: "credentials".into(),
                            },
                            StorageObservationContract {
                                claim_name: "logs".into(),
                            },
                        ],
                    },
                )]),
            })
            .expect_err("one mount cannot project two PVCs");
        assert!(error.starts_with("backend_link_storage_cardinality:"));
    }

    #[test]
    fn execution_context_and_target_descriptor_compose_independently() {
        let mut executor = component("attacker", "attacker-workspace", ComponentKind::Attacker);
        executor.control = crate::ControlClass::Attacker;
        let target = component("chain", "bitcoin-core", ComponentKind::Bitcoin);
        let lab = crate::LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "cross-target-contract".into(),
            components: vec![executor.clone(), target.clone()],
            links: vec![],
            policy: crate::LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, crate::default_catalog()).expect("resolve lock");
        let compile = |component: ComponentSpec| {
            let entry = lock
                .entries
                .iter()
                .find(|entry| entry.component_id == component.id)
                .expect("component lock")
                .clone();
            default_backend_registry()
                .compile_contract(&ComponentPlanInput {
                    instance_key: "instance-key".into(),
                    revision_digest: "sha256:revision".into(),
                    component,
                    lock: entry,
                    relevant_links: vec![],
                    linked_targets: BTreeMap::new(),
                    linked_state: BTreeMap::new(),
                })
                .expect("compile component")
        };
        let executor_plan = compile(executor);
        let target_plan = compile(target);
        assert!(executor_plan.execution_context.image.contains("busybox"));
        assert!(executor_plan.target_descriptor.ports.is_empty());
        assert_eq!(target_plan.target_descriptor.ports["rpc"], 18_443);
        assert_ne!(
            executor_plan.execution_context.component_id,
            target_plan.target_descriptor.component_id
        );
    }
}
