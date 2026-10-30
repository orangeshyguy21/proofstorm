//! Experimental Spark backends. Catalog entries exist only for platforms with
//! pinned images; endpoints, keys and the network come from typed links.
use crate::processor_ids::{OPEN_SSP, SPARK_OPERATORS, SPARK_PROCESSOR};

use super::{
    BTreeMap, ComponentBackendContract, ComponentConditionType, ComponentKind, ComponentSpec,
    ConfigDefault, ConfigRule, ConfigSettingClass, ConfigValueKind, Deserialize,
    EffectiveComponentConfig, ExecutionMountRequirement, ExecutionMountTemplateContract,
    ExecutionStorageTemplateSource, JsonSchema, ProtocolProbeContract, Serialize,
    StorageRequirementTemplate, WorkloadControllerKind, config_field, contract, json,
    managed_field, required_config_value, service_conditions, typed_config_error,
};

/// A FROST federation: one pod and two PostgreSQL databases per operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SparkOperatorsConfig {
    pub operators: u64,
    pub threshold: u64,
}

/// open-ssp settings an agent may set; the rest stay upstream or come from links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenSspConfig {
    pub max_swap_total_sats: u64,
}

pub(super) fn contracts() -> [ComponentBackendContract; 3] {
    [operators_contract(), ssp_contract(), processor_contract()]
}

fn operators_contract() -> ComponentBackendContract {
    let mut conditions = service_conditions(true, false);
    // State lives in the operators' PostgreSQL databases.
    conditions.remove(&ComponentConditionType::StorageReady);
    let mut backend = contract(
        SPARK_OPERATORS,
        ComponentKind::SparkServer,
        "spark-operators/0.0/v1",
        BTreeMap::from([
            (
                "operators".into(),
                config_field(
                    "Operators in the federation, each one pod with its own main and ephemeral PostgreSQL databases. Qualified with 3",
                    ConfigValueKind::Integer,
                    ConfigDefault::Literal(json!(3)),
                )
                .with_numeric_bounds(1.0, 5.0),
            ),
            (
                "threshold".into(),
                config_field(
                    "FROST signing threshold: how many operators must sign. At most operators. Qualified with 2",
                    ConfigValueKind::Integer,
                    ConfigDefault::Literal(json!(2)),
                )
                .with_numeric_bounds(1.0, 5.0),
            ),
        ]),
        BTreeMap::from([("grpc".into(), 8535), ("ssp".into(), 8536)]),
        "proofstorm/spark-operators-state/v1",
        conditions,
    );
    backend.config_rules = vec![ConfigRule::LessThanOrEqual {
        minimum_field: "threshold".into(),
        maximum_field: "operators".into(),
    }];
    backend.workload_kind = WorkloadControllerKind::StatefulSet;
    backend.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: "grpc".into(),
    });
    insert_managed(
        &mut backend,
        [
            (
                "backend_endpoint",
                "Bitcoin RPC/ZMQ and PostgreSQL endpoints from typed links",
                ConfigSettingClass::TopologyDerived,
            ),
            (
                "rpc_credentials",
                "Fixed regtest Bitcoin credentials in scoped read-only configuration",
                ConfigSettingClass::RuntimePolicy,
            ),
            (
                "credentials",
                "Controller-generated identity key per operator and TLS certificates from one per-component CA; consumers get only the public operator list and CA",
                ConfigSettingClass::GeneratedInstanceSecret,
            ),
            (
                "native_settings",
                "Pinned operator configuration with managed regtest overrides",
                ConfigSettingClass::RuntimePolicy,
            ),
        ],
    );
    backend
}

fn ssp_contract() -> ComponentBackendContract {
    let mut backend = contract(
        OPEN_SSP,
        ComponentKind::SparkServer,
        "open-ssp/0.1/v1",
        BTreeMap::from([(
            "max_swap_total_sats".into(),
            config_field(
                "Largest swap the SSP fills, in sats (MAX_SWAP_TOTAL_SATS). Upstream default 1000000",
                ConfigValueKind::Integer,
                ConfigDefault::Literal(json!(1_000_000)),
            )
            .with_numeric_bounds(1.0, 2_100_000_000_000_000.0),
        )]),
        BTreeMap::from([("http".into(), 5000)]),
        "proofstorm/open-ssp-state/v1",
        service_conditions(true, true),
    );
    stateful_data(&mut backend);
    // The SSP authenticates to its LDK Server with the macaroon and TLS
    // certificate the node keeps in its own storage.
    backend
        .execution_mounts
        .push(ExecutionMountTemplateContract {
            name: "ldk-server".into(),
            mount_path: "/ldk-server".into(),
            read_only: true,
            source: ExecutionStorageTemplateSource::LinkedStatefulData {
                link_kind: crate::LinkKind::PaymentBackend,
                binding: crate::DependencyBinding::Payment {
                    method: crate::PaymentMethod::Bolt11,
                    unit: "sat".into(),
                },
                target_implementation: "ldk-server".into(),
            },
            requirement: ExecutionMountRequirement::Required,
        });
    backend.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: "http".into(),
    });
    insert_managed(
        &mut backend,
        [
            (
                "backend_endpoint",
                "Operator list and LDK Server endpoint from typed links",
                ConfigSettingClass::TopologyDerived,
            ),
            (
                "credentials",
                "Controller-generated Spark mnemonic and admin token retained with owned state, plus the linked LDK Server's macaroon and TLS certificate",
                ConfigSettingClass::GeneratedInstanceSecret,
            ),
            (
                "data_dir",
                "Owned persistent SSP identity and swap state; never silently reinitialized",
                ConfigSettingClass::RuntimePolicy,
            ),
            (
                "native_settings",
                "Pinned upstream defaults with managed regtest overrides; upstream requires a zero swap fee",
                ConfigSettingClass::RuntimePolicy,
            ),
        ],
    );
    backend
}

fn processor_contract() -> ComponentBackendContract {
    let mut backend = contract(
        SPARK_PROCESSOR,
        ComponentKind::PaymentProcessor,
        "cdk-spark-processor/0.1/v1",
        BTreeMap::new(),
        BTreeMap::from([("grpc".into(), 50051)]),
        "proofstorm/cdk-spark-processor-state/v1",
        service_conditions(true, false),
    );
    stateful_data(&mut backend);
    backend.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: "grpc".into(),
    });
    insert_managed(
        &mut backend,
        [
            (
                "backend_endpoint",
                "SSP endpoint and identity, and the SSP's operator list, from typed links",
                ConfigSettingClass::TopologyDerived,
            ),
            (
                "mnemonic",
                "Controller-generated wallet identity retained with owned state",
                ConfigSettingClass::GeneratedInstanceSecret,
            ),
            (
                "credentials",
                "Controller-generated processor mutual TLS identities",
                ConfigSettingClass::GeneratedInstanceSecret,
            ),
            (
                "data_dir",
                "Persistent wallet directory, including the quote mapping database",
                ConfigSettingClass::RuntimePolicy,
            ),
        ],
    );
    backend
}

fn stateful_data(backend: &mut ComponentBackendContract) {
    backend.workload_kind = WorkloadControllerKind::StatefulSet;
    backend.storage_requirements = vec![StorageRequirementTemplate::StatefulClaimTemplate {
        template_name: "data".into(),
    }];
    backend.execution_mounts = vec![ExecutionMountTemplateContract {
        name: "data".into(),
        mount_path: "/data".into(),
        read_only: false,
        source: ExecutionStorageTemplateSource::StatefulData,
        requirement: ExecutionMountRequirement::Required,
    }];
}

/// Every Spark service runs on owned regtest; the remaining managed settings
/// differ per service.
fn insert_managed<const N: usize>(
    backend: &mut ComponentBackendContract,
    fields: [(&str, &str, ConfigSettingClass); N],
) {
    let network = (
        "network",
        "Owned Bitcoin regtest only",
        ConfigSettingClass::RuntimePolicy,
    );
    for (name, description, classification) in std::iter::once(network).chain(fields) {
        backend.config_fields.insert(
            name.into(),
            managed_field(description, ConfigValueKind::String, classification),
        );
    }
}

pub(super) fn effective_config(
    component: &ComponentSpec,
) -> Result<EffectiveComponentConfig, String> {
    let integer = |name: &str| {
        required_config_value(component, name)?
            .as_u64()
            .ok_or_else(|| typed_config_error(component, name))
    };
    match component.implementation.as_str() {
        SPARK_OPERATORS => Ok(EffectiveComponentConfig::SparkOperators(
            SparkOperatorsConfig {
                operators: integer("operators")?,
                threshold: integer("threshold")?,
            },
        )),
        OPEN_SSP => Ok(EffectiveComponentConfig::OpenSsp(OpenSspConfig {
            max_swap_total_sats: integer("max_swap_total_sats")?,
        })),
        _ => Ok(EffectiveComponentConfig::SparkProcessor),
    }
}
