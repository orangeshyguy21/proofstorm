//! Contracts for independently controlled CDK payment processors and LDK Server.
use super::{
    BTreeMap, ComponentBackendContract, ComponentConditionType, ComponentKind, ComponentSpec,
    ConfigDefault, ConfigSettingClass, ConfigValueKind, Deserialize, EffectiveComponentConfig,
    ExecutionMountRequirement, ExecutionMountTemplateContract, ExecutionStorageTemplateSource,
    JsonSchema, ProtocolProbeContract, Serialize, StorageRequirementTemplate,
    WorkloadControllerKind, config_field, contract, json, managed_field, required_config_value,
    service_conditions, typed_config_error,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LdkServerProcessorConfig {
    pub fee_reserve_min_sat: u64,
    pub fee_reserve_percent: f64,
    pub max_payment_scan_pages: u64,
}

pub(super) fn effective_config(
    component: &ComponentSpec,
) -> Result<EffectiveComponentConfig, String> {
    let integer = |name| {
        required_config_value(component, name)?
            .as_u64()
            .ok_or_else(|| typed_config_error(component, name))
    };
    Ok(EffectiveComponentConfig::LdkServerProcessor(
        LdkServerProcessorConfig {
            fee_reserve_min_sat: integer("fee_reserve_min_sat")?,
            fee_reserve_percent: required_config_value(component, "fee_reserve_percent")?
                .as_f64()
                .ok_or_else(|| typed_config_error(component, "fee_reserve_percent"))?,
            max_payment_scan_pages: integer("max_payment_scan_pages")?,
        },
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "the two declarative contracts keep all runtime requirements together"
)]
pub(super) fn contracts() -> Vec<ComponentBackendContract> {
    let mut node = contract(
        "ldk-server",
        ComponentKind::Lightning,
        "ldk-server/0.1/v1",
        BTreeMap::from([(
            "alias".into(),
            config_field(
                "Native LDK Server node alias",
                ConfigValueKind::String,
                ConfigDefault::ComponentId,
            )
            .with_string_bounds(1, 32),
        )]),
        BTreeMap::from([("p2p".into(), 9735), ("rpc".into(), 3536)]),
        "proofstorm/ldk-server-state/v1",
        service_conditions(true, false),
    );
    node.workload_kind = WorkloadControllerKind::StatefulSet;
    node.storage_requirements = vec![StorageRequirementTemplate::StatefulClaimTemplate {
        template_name: "data".into(),
    }];
    node.execution_mounts = vec![
        mount(
            "data",
            "/data",
            false,
            ExecutionStorageTemplateSource::StatefulData,
        ),
        mount(
            "config",
            "/config",
            true,
            ExecutionStorageTemplateSource::ComponentConfig,
        ),
    ];
    node.execution_environment = BTreeMap::from([("HOME".into(), "/data".into())]);
    node.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: "rpc".into(),
    });
    let integer = |description, default, min, max| {
        config_field(
            description,
            ConfigValueKind::Integer,
            ConfigDefault::Literal(json!(default)),
        )
        .with_numeric_bounds(min, max)
    };
    let mut conditions = service_conditions(true, true);
    conditions.remove(&ComponentConditionType::StorageReady);
    let mut processor = contract(
        "cdk-ldk-server-processor",
        ComponentKind::PaymentProcessor,
        "cdk-ldk-server-processor/0.1/v1",
        BTreeMap::from([
            (
                "fee_reserve_min_sat".into(),
                integer("Minimum outgoing fee reserve in sats", 2, 0.0, 1_000_000.0),
            ),
            (
                "fee_reserve_percent".into(),
                config_field(
                    "Outgoing fee reserve as a fraction of payment amount",
                    ConfigValueKind::Number,
                    ConfigDefault::Literal(json!(0.01)),
                )
                .with_numeric_bounds(0.0, 1.0),
            ),
            (
                "max_payment_scan_pages".into(),
                integer(
                    "Maximum LDK payment-history pages per reconciliation",
                    32,
                    1.0,
                    1024.0,
                ),
            ),
        ]),
        BTreeMap::from([("grpc".into(), 50051)]),
        "proofstorm/cdk-ldk-server-processor-state/v1",
        conditions,
    );
    processor.execution_mounts = vec![mount(
        "ldk-server",
        "/ldk-server",
        true,
        ExecutionStorageTemplateSource::LinkedStatefulData {
            link_kind: crate::LinkKind::PaymentBackend,
            binding: crate::DependencyBinding::Payment {
                method: crate::PaymentMethod::Bolt11,
                unit: "sat".into(),
            },
            target_implementation: "ldk-server".into(),
        },
    )];
    processor.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: "grpc".into(),
    });
    // The authoritative payment history lives in the linked LDK node.
    for contract in [&mut node, &mut processor] {
        for (name, description, classification) in [
            (
                "network",
                "Cell-local Bitcoin regtest only",
                ConfigSettingClass::RuntimePolicy,
            ),
            (
                "credentials",
                "Private runtime-generated transport credentials",
                ConfigSettingClass::GeneratedInstanceSecret,
            ),
            (
                "backend_endpoint",
                "Endpoint derived from typed topology links",
                ConfigSettingClass::TopologyDerived,
            ),
        ] {
            contract.config_fields.insert(
                name.into(),
                managed_field(description, ConfigValueKind::String, classification),
            );
        }
    }
    vec![node, processor]
}

fn mount(
    name: &str,
    path: &str,
    read_only: bool,
    source: ExecutionStorageTemplateSource,
) -> ExecutionMountTemplateContract {
    ExecutionMountTemplateContract {
        name: name.into(),
        mount_path: path.into(),
        read_only,
        source,
        requirement: ExecutionMountRequirement::Required,
    }
}
