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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentBackendContract {
    pub id: String,
    pub kind: ComponentKind,
    pub config_defaults: BTreeMap<String, ConfigDefault>,
    pub service_ports: BTreeMap<String, u16>,
    pub execution_state_contract: String,
    pub execution_mounts: Vec<ExecutionMountContract>,
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
pub enum ExecutionStorageSource {
    StatefulData,
    ComponentPersistentData,
    ComponentConfig,
    LinkedStatefulData { link_kind: crate::LinkKind },
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
    pub linked_targets: BTreeMap<String, TargetDescriptorContract>,
    pub linked_state: BTreeMap<String, LinkedStateObservationContract>,
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
    pub effective_config: BTreeMap<String, Value>,
    pub relevant_links: Vec<LinkSpec>,
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
        let mut effective = component.clone();
        for (name, default) in &contract.config_defaults {
            effective
                .config
                .entry(name.clone())
                .or_insert_with(|| match default {
                    ConfigDefault::Literal(value) => value.clone(),
                    ConfigDefault::ComponentId => Value::String(component.id.clone()),
                });
        }
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
        let effective = self.resolve_effective_component(&input.component)?;
        let mut relevant_links = input.relevant_links.clone();
        relevant_links.sort();
        for link in &relevant_links {
            let Some(target) = input.linked_targets.get(&link.to) else {
                return Err(format!(
                    "backend_link_target_missing: component {:?} link target {:?} has no descriptor",
                    input.component.id, link.to
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
        let credentials = compile_credential_observations(backend, input, &relevant_links)?;
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
            effective_config: effective.config,
            relevant_links,
            linked_targets: input.linked_targets.clone(),
            execution_context: ExecutionContextContract {
                component_id: input.component.id.clone(),
                image: input.lock.image.clone(),
                state_contract: backend.execution_state_contract.clone(),
                mounts: backend.execution_mounts.clone(),
                environment: backend.execution_environment.clone(),
            },
            target_descriptor: TargetDescriptorContract {
                component_id: input.component.id.clone(),
                kind: input.component.kind,
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

fn compile_credential_observations(
    backend: &ComponentBackendContract,
    input: &ComponentPlanInput,
    relevant_links: &[LinkSpec],
) -> Result<Vec<CredentialObservationContract>, String> {
    let mut credentials = Vec::new();
    for mount in &backend.execution_mounts {
        let ExecutionStorageSource::LinkedStatefulData { link_kind } = mount.source else {
            continue;
        };
        for link in relevant_links.iter().filter(|link| link.kind == link_kind) {
            let linked = input.linked_state.get(&link.to).ok_or_else(|| {
                format!(
                    "backend_link_state_missing: component {:?} link target {:?} has no state observation contract",
                    input.component.id, link.to
                )
            })?;
            for linked_storage in &linked.storage {
                credentials.push(CredentialObservationContract {
                    identity: format!(
                        "{}:{}:{}",
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
        }
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
pub fn default_backend_registry() -> BackendContractRegistry {
    BackendContractRegistry {
        entries: default_backend_contracts()
            .into_iter()
            .map(|contract| (contract.id.clone(), contract))
            .collect(),
    }
}

fn default_backend_contracts() -> Vec<ComponentBackendContract> {
    vec![
        contract(
            "bitcoin-core",
            ComponentKind::Bitcoin,
            BTreeMap::from([
                ("fallback_fee".into(), ConfigDefault::Literal(json!(0.0002))),
                ("txindex".into(), ConfigDefault::Literal(json!(true))),
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
            BTreeMap::from([("alias".into(), ConfigDefault::ComponentId)]),
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
            BTreeMap::from([("alias".into(), ConfigDefault::ComponentId)]),
            BTreeMap::from([("p2p".into(), 9_735)]),
            "proofstorm/cln-state/v1",
            service_conditions(true, false),
        ),
        contract(
            "cdk",
            ComponentKind::Mint,
            BTreeMap::from([
                (
                    "description".into(),
                    ConfigDefault::Literal(json!("Proofstorm regtest CDK mint")),
                ),
                (
                    "name".into(),
                    ConfigDefault::Literal(json!("Proofstorm CDK mint")),
                ),
            ]),
            BTreeMap::from([("http".into(), 3_338)]),
            "proofstorm/cdk-mint-state/v1",
            service_conditions(true, true),
        ),
        contract(
            "nutshell-wallet",
            ComponentKind::Wallet,
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
            "attacker-workspace",
            ComponentKind::Attacker,
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

fn contract(
    id: &str,
    kind: ComponentKind,
    config_defaults: BTreeMap<String, ConfigDefault>,
    service_ports: BTreeMap<String, u16>,
    execution_state_contract: &str,
    applicable_conditions: BTreeSet<ComponentConditionType>,
) -> ComponentBackendContract {
    let (execution_mounts, execution_environment) = execution_contract(id);
    let (workload_kind, storage_requirements) = observation_contract(id);
    ComponentBackendContract {
        id: id.into(),
        kind,
        config_defaults,
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
        "cdk" => Some(ProtocolProbeContract::HttpGet {
            port_name: "http".into(),
            path: "/v1/info".into(),
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
        "bitcoin-core" | "lnd" | "cln" => {
            (WorkloadControllerKind::StatefulSet, vec![stateful_data()])
        }
        "cdk" | "nutshell-wallet" => (WorkloadControllerKind::Deployment, vec![component_data()]),
        _ => (WorkloadControllerKind::Deployment, vec![]),
    }
}

fn execution_contract(backend: &str) -> (Vec<ExecutionMountContract>, BTreeMap<String, String>) {
    use ExecutionStorageSource as Source;

    let binding = |name: &str, mount_path: &str, read_only: bool, source| ExecutionMountContract {
        name: name.into(),
        mount_path: mount_path.into(),
        read_only,
        source,
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
        "cdk" => (
            vec![
                binding("config", "/config", true, Source::ComponentConfig),
                binding("data", "/app/data", false, Source::ComponentPersistentData),
                binding(
                    "lnd",
                    "/lnd",
                    true,
                    Source::LinkedStatefulData {
                        link_kind: crate::LinkKind::LightningBackend,
                    },
                ),
            ],
            BTreeMap::from([
                ("CDK_MINTD_WORK_DIR".into(), "/app/data".into()),
                ("HOME".into(), "/app/data".into()),
            ]),
        ),
        "nutshell-wallet" => (
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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlClass, LOCK_API_VERSION, resolve_lock};

    fn component(id: &str, implementation: &str, kind: ComponentKind) -> ComponentSpec {
        ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: "v1alpha1".into(),
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
    }

    #[test]
    fn registry_refuses_duplicate_missing_and_kind_mismatch() {
        let first = contract(
            "duplicate",
            ComponentKind::Bitcoin,
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
    fn compiled_contract_uses_current_lock_rollout_identity() {
        let component = component("chain", "bitcoin-core", ComponentKind::Bitcoin);
        let lab = crate::LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "compile-contract".into(),
            components: vec![component.clone()],
            links: vec![],
            policy: crate::LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &crate::default_catalog()).expect("resolve current lock");
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
        assert_eq!(compiled.effective_config["txindex"], json!(true));
        assert_eq!(compiled.execution_context.image, lock.entries[0].image);
        assert_eq!(compiled.target_descriptor.ports["rpc"], 18_443);
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
        let lock = resolve_lock(&lab, &crate::default_catalog()).expect("resolve lock");
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
