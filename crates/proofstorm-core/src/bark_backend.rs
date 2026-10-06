//! Reserved Bark backends. Catalog publication remains gated on
//! managed-stack qualification and actual image identities.
use super::{
    BTreeMap, ComponentBackendContract, ComponentKind, ComponentSpec, ConfigDefault,
    ConfigSettingClass, ConfigValueKind, Deserialize, EffectiveComponentConfig,
    ExecutionMountRequirement, ExecutionMountTemplateContract, ExecutionStorageTemplateSource,
    JsonSchema, ProtocolProbeContract, Serialize, StorageRequirementTemplate,
    WorkloadControllerKind, config_field, contract, json, managed_field, required_config_value,
    service_conditions, typed_config_error,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BarkProcessorConfig {
    pub event_poll_interval_ms: u64,
}

pub(super) fn stack_contracts() -> [ComponentBackendContract; 2] {
    [
        stack_contract(
            crate::processor_ids::BARK_SERVER,
            ComponentKind::ArkServer,
            "bark-server/0.7/v1",
            "proofstorm/bark-server-state/v1",
            BTreeMap::from([("rpc".into(), 3535)]),
        ),
        stack_contract(
            crate::processor_ids::CLN_HOLD,
            ComponentKind::Lightning,
            "cln-hold/26.06/v1",
            "proofstorm/cln-hold-state/v1",
            BTreeMap::from([
                ("p2p".into(), 9735),
                ("grpc".into(), 9988),
                ("hold".into(), 9292),
            ]),
        ),
    ]
}

fn stack_contract(
    id: &str,
    kind: ComponentKind,
    version: &str,
    state: &str,
    ports: BTreeMap<String, u16>,
) -> ComponentBackendContract {
    let mut backend = contract(
        id,
        kind,
        version,
        BTreeMap::new(),
        ports,
        state,
        service_conditions(true, false),
    );
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
    backend.execution_environment = BTreeMap::from([("HOME".into(), "/data".into())]);
    backend.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: if kind == ComponentKind::ArkServer {
            "rpc"
        } else {
            "grpc"
        }
        .into(),
    });
    for (name, description, classification) in [
        (
            "network",
            "Owned Bitcoin regtest only",
            ConfigSettingClass::RuntimePolicy,
        ),
        (
            "backend_endpoint",
            "Endpoints derived from validated typed dependencies",
            ConfigSettingClass::TopologyDerived,
        ),
        (
            "rpc_credentials",
            "Fixed regtest Bitcoin credentials in scoped read-only configuration",
            ConfigSettingClass::RuntimePolicy,
        ),
        (
            "credentials",
            "Preserved, separately scoped CLN and hold TLS identities",
            ConfigSettingClass::GeneratedInstanceSecret,
        ),
        (
            "data_dir",
            "Owned persistent native identity and payment state; never silently reinitialized",
            ConfigSettingClass::RuntimePolicy,
        ),
        (
            "native_settings",
            "Pinned upstream defaults with managed regtest overrides",
            ConfigSettingClass::RuntimePolicy,
        ),
    ] {
        backend.config_fields.insert(
            name.into(),
            managed_field(description, ConfigValueKind::String, classification),
        );
    }
    backend
}

pub(super) fn effective_config(
    component: &ComponentSpec,
) -> Result<EffectiveComponentConfig, String> {
    Ok(EffectiveComponentConfig::BarkProcessor(
        BarkProcessorConfig {
            event_poll_interval_ms: required_config_value(component, "event_poll_interval_ms")?
                .as_u64()
                .ok_or_else(|| typed_config_error(component, "event_poll_interval_ms"))?,
        },
    ))
}

pub(super) fn processor_contract() -> ComponentBackendContract {
    let mut backend = contract(
        crate::processor_ids::BARK_PROCESSOR,
        ComponentKind::PaymentProcessor,
        "cdk-bark-processor/0.1/v1",
        BTreeMap::from([(
            "event_poll_interval_ms".into(),
            config_field(
                "Interval between native Bark payment event polling passes in milliseconds",
                ConfigValueKind::Integer,
                ConfigDefault::Literal(json!(5000)),
            )
            .with_numeric_bounds(1.0, 60_000.0),
        )]),
        BTreeMap::from([("grpc".into(), 50051)]),
        "proofstorm/cdk-bark-processor-state/v1",
        // Secret validity is enforced at provisioning/startup and by authenticated
        // readiness. CredentialsReady currently observes linked PVCs only.
        service_conditions(true, false),
    );
    backend.workload_kind = WorkloadControllerKind::StatefulSet;
    backend.storage_requirements = vec![StorageRequirementTemplate::StatefulClaimTemplate {
        template_name: "data".into(),
    }];
    backend.execution_mounts = [
        (
            "data",
            "/data",
            false,
            ExecutionStorageTemplateSource::StatefulData,
        ),
        (
            "config",
            "/chain-rpc",
            true,
            ExecutionStorageTemplateSource::ComponentConfig,
        ),
    ]
    .into_iter()
    .map(
        |(name, path, read_only, source)| ExecutionMountTemplateContract {
            name: name.into(),
            mount_path: path.into(),
            read_only,
            source,
            requirement: ExecutionMountRequirement::Required,
        },
    )
    .collect();
    backend.protocol_probe = Some(ProtocolProbeContract::Tcp {
        port_name: "grpc".into(),
    });
    for (name, description, classification) in [
        (
            "network",
            "Owned Bitcoin regtest only",
            ConfigSettingClass::RuntimePolicy,
        ),
        (
            "payment_methods",
            "BOLT11/sat only",
            ConfigSettingClass::RuntimePolicy,
        ),
        (
            "backend_endpoint",
            "Bark server and Bitcoin endpoints from typed links",
            ConfigSettingClass::TopologyDerived,
        ),
        (
            "rpc_credentials",
            "Existing fixed regtest RPC credentials in a component-scoped read-only file",
            ConfigSettingClass::RuntimePolicy,
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
            "Complete persistent wallet directory, including SQLite and payment mappings",
            ConfigSettingClass::RuntimePolicy,
        ),
    ] {
        backend.config_fields.insert(
            name.into(),
            managed_field(description, ConfigValueKind::String, classification),
        );
    }
    backend
}
