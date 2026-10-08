//! Persistent CDK Bark processor; server and CLN/hold rendering follow separately.
use super::{
    AdapterError, ComponentKind, ComponentPlanContract, DependencyBinding,
    EffectiveComponentConfig, LinkKind, RPC_PASSWORD, RPC_USER, RenderedComponent,
    TargetDescriptorContract, Value, WorkloadControllerKind, container_security,
    install_component_driver, instance_namespace, json, metadata, processor, require_plan_backend,
    resource, service_from_plan, stateful_set, target_port,
};
use proofstorm_core::{
    BarkProcessorConfig, BitcoinNetwork, ProcessorProfile, method_list,
    processor_ids::{BARK_PROCESSOR, BARK_SERVER},
};

pub(super) fn dependency<'a>(
    plan: &'a ComponentPlanContract,
    kind: LinkKind,
    binding: &DependencyBinding,
    target_kind: ComponentKind,
    implementation: &str,
) -> Result<&'a TargetDescriptorContract, AdapterError> {
    let links: Vec<_> = plan
        .relevant_links
        .iter()
        .filter(|link| link.kind == kind)
        .collect();
    let target = links
        .first()
        .and_then(|link| plan.linked_targets.get(&link.id));
    if links.len() != 1
        || !target.is_some_and(|target| {
            links[0].from == plan.component_id
                && links[0].to == target.component_id
                && links[0].binding.as_ref() == Some(binding)
                && target.kind == target_kind
                && target.backend_id == implementation
                && service_name(&target.component_id)
        })
    {
        return Err(AdapterError::InvalidPlan(format!(
            "Bark component requires one {kind:?} binding to {implementation}"
        )));
    }
    Ok(target.expect("validated target"))
}

pub(super) fn service_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

/// Render one persistent Bark processor advertising its configured methods,
/// without publishing an image.
/// # Errors
/// Refuses incompatible plans, ambiguous dependencies and mismatched identities.
pub fn render_processor(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, BARK_PROCESSOR, ComponentKind::PaymentProcessor)?;
    let EffectiveComponentConfig::BarkProcessor(config) = &plan.effective_config else {
        return Err(AdapterError::InvalidPlan(
            "Bark processor configuration missing".into(),
        ));
    };
    validate_identity(plan, config)?;
    let chain = dependency(
        plan,
        LinkKind::ChainBackend,
        &DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest,
        },
        ComponentKind::Bitcoin,
        "bitcoin-core",
    )?;
    let server = dependency(
        plan,
        LinkKind::ArkBackend,
        &DependencyBinding::Ark {
            network: BitcoinNetwork::Regtest,
        },
        ComponentKind::ArkServer,
        BARK_SERVER,
    )?;
    let port = required_port(&plan.target_descriptor, "grpc")?;
    let rpc = required_port(chain, "rpc")?;
    let ark = required_port(server, "rpc")?;
    let id = &plan.component_id;
    let namespace = instance_namespace(&plan.instance_key);
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered
        .config_maps
        .push(resource(json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":metadata(&format!("{id}-config"),&plan.instance_key,&namespace,Some(id)),
        "data":{"rpc.cookie":format!("{RPC_USER}:{RPC_PASSWORD}\n")}}))?);
    for (suffix, data) in [
        (
            "identity",
            json!({"PROOFSTORM_SECRET_KIND":"bark-processor"}),
        ),
        (
            "payment-tls",
            json!({"PROOFSTORM_SECRET_KIND":"payment-processor-tls","PROOFSTORM_TLS_SERVER_NAME":id}),
        ),
    ] {
        rendered.secrets.push(resource(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":metadata(&format!("{id}-{suffix}"),&plan.instance_key,&namespace,Some(id)),"stringData":data}))?);
    }
    let workload = workload(plan, port, chain, rpc, server, ark, config);
    rendered.stateful_sets.push(resource(workload)?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}

fn validate_identity(
    plan: &ComponentPlanContract,
    config: &BarkProcessorConfig,
) -> Result<(), AdapterError> {
    let backend = proofstorm_core::default_backend_registry()
        .require(BARK_PROCESSOR)
        .map_err(AdapterError::InvalidPlan)?;
    let mounts_match = plan.execution_context.mounts.len() == backend.execution_mounts.len()
        && backend.execution_mounts.iter().all(|expected| {
            plan.execution_context.mounts.iter().any(|actual| {
                actual.name == expected.name
                    && actual.mount_path == expected.mount_path
                    && actual.read_only == expected.read_only
                    && matches!(
                        (&actual.source, &expected.source),
                        (
                            proofstorm_core::ExecutionStorageSource::StatefulData,
                            proofstorm_core::ExecutionStorageTemplateSource::StatefulData
                        ) | (
                            proofstorm_core::ExecutionStorageSource::ComponentConfig,
                            proofstorm_core::ExecutionStorageTemplateSource::ComponentConfig
                        )
                    )
            })
        });
    if !(1..=60_000).contains(&config.event_poll_interval_ms)
        || !ProcessorProfile::Bark.accepts_methods(&config.payment_methods)
        || !mounts_match
        || plan.execution_context.component_id != plan.component_id
        || plan.execution_context.state_contract != backend.execution_state_contract
        || !service_name(&plan.component_id)
        || plan.target_descriptor.component_id != plan.component_id
        || plan.target_descriptor.backend_id != BARK_PROCESSOR
        || plan.target_descriptor.kind != ComponentKind::PaymentProcessor
        || plan.target_descriptor.ports.len() != 1
        || plan.workload.kind != WorkloadControllerKind::StatefulSet
        || plan.storage.len() != 1
        || plan.storage[0].claim_name != format!("data-{}-0", plan.component_id)
        || plan.relevant_links.iter().any(|link| {
            matches!(
                link.kind,
                LinkKind::PaymentBackend
                    | LinkKind::DatabaseBackend
                    | LinkKind::AuthenticationBackend
            )
        })
    {
        return Err(AdapterError::InvalidPlan(
            "Bark processor identity, storage or configuration contract mismatch".into(),
        ));
    }
    Ok(())
}

pub(super) fn required_port(
    target: &TargetDescriptorContract,
    name: &str,
) -> Result<u16, AdapterError> {
    let port = target_port(target, name)?;
    if port == 0 {
        return Err(AdapterError::InvalidPlan(
            "Bark dependency port must be nonzero".into(),
        ));
    }
    Ok(port)
}

fn workload(
    plan: &ComponentPlanContract,
    port: u16,
    chain: &TargetDescriptorContract,
    rpc: u16,
    server: &TargetDescriptorContract,
    ark: u16,
    config: &BarkProcessorConfig,
) -> Value {
    let id = &plan.component_id;
    let methods = method_list(&config.payment_methods);
    let mut workload = stateful_set(
        &plan.instance_key,
        &instance_namespace(&plan.instance_key),
        id,
        &plan.execution_context.image,
        Some(vec![
            crate::drivers::DRIVER_PATH.into(),
            "exec-bark-processor".into(),
        ]),
        &[],
        "/data",
        &json!({"exec":{"command":[crate::drivers::DRIVER_PATH,"processor-settings",format!("https://127.0.0.1:{port}"),"/processor-client/tls",BARK_PROCESSOR,methods]},"timeoutSeconds":3}),
        Some(plan),
    );
    let pod = &mut workload["spec"]["template"]["spec"];
    let env = [
        ("SERVER_ADDRESS", "0.0.0.0".into()),
        ("SERVER_PORT", port.to_string()),
        ("TLS_ENABLE", "true".into()),
        ("ALLOW_INSECURE", "false".into()),
        ("TLS_CERT_PATH", "/processor-server/tls/server.pem".into()),
        ("TLS_KEY_PATH", "/processor-server/tls/server.key".into()),
        ("TLS_CLIENT_CA_PATH", "/processor-server/tls/ca.pem".into()),
        ("BARK_NETWORK", "regtest".into()),
        ("BARK_PAYMENT_METHODS", methods),
        ("BARK_DATA_DIR", "/data".into()),
        (
            "BARK_SERVER_ADDRESS",
            format!("http://{}:{ark}", server.component_id),
        ),
        (
            "BARK_BITCOIND_ADDRESS",
            format!("http://{}:{rpc}", chain.component_id),
        ),
        ("BARK_BITCOIND_COOKIEFILE", "/chain-rpc/rpc.cookie".into()),
        (
            "BARK_EVENT_POLL_INTERVAL_MS",
            config.event_poll_interval_ms.to_string(),
        ),
    ];
    pod["containers"][0]["env"] = json!(
        env.into_iter()
            .map(|(name, value)| json!({"name":name,"value":value}))
            .collect::<Vec<_>>()
    );
    pod["containers"][0]["ports"] = json!([{"name":"grpc","containerPort":port}]);
    pod["containers"][0]["volumeMounts"] = json!([
        {"name":"data","mountPath":"/data"},
        {"name":"config","mountPath":"/chain-rpc","readOnly":true},
        {"name":"identity","mountPath":"/processor-identity","readOnly":true},
        processor::tls_mount("processor-server","/processor-server/tls"),
        processor::tls_mount("processor-client","/processor-client/tls")]);
    pod["volumes"] = json!([
        {"name":"config","configMap":{"name":format!("{id}-config"),"defaultMode":288}},
        {"name":"identity","secret":{"secretName":format!("{id}-identity"),"defaultMode":288,"items":[{"key":"mnemonic","path":"mnemonic"}]}},
        processor::tls_volume("processor-server",id,"server"), processor::tls_volume("processor-client",id,"client")]);
    // Positional arguments keep component identities out of shell source.
    pod["initContainers"] = json!([{"name":"wait-for-bark-dependencies","image":plan.execution_context.image,
        "command":["sh","-ec","n=0; while [ \"$n\" -lt 120 ]; do if /opt/proofstorm/driver tcp \"$1\" \"$2\" && /opt/proofstorm/driver tcp \"$3\" \"$4\"; then exit 0; fi; n=$((n+1)); sleep 1; done; echo 'Bark dependencies did not become ready' >&2; exit 1","--",chain.component_id,rpc.to_string(),server.component_id,ark.to_string()],
        "securityContext":container_security(),"volumeMounts":[{"name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true}]}]);
    workload
}
