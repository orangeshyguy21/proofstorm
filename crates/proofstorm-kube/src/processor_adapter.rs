//! LDK Server and the CDK gRPC payment boundary are independent workloads.
use super::{
    AdapterError, BTreeMap, BTreeSet, CdkMintConfig, ComponentKind, ComponentPlanContract,
    DependencyBinding, EffectiveComponentConfig, LinkKind, RPC_PASSWORD, RPC_USER,
    RenderedComponent, TargetDescriptorContract, Value, container_security,
    install_component_driver, instance_affinity, instance_namespace, json, labels, metadata,
    mint_common_config, plan_execution_credential, plan_linked_target, plan_pod_metadata,
    plan_workload_metadata, pod_security, require_plan_backend, resource, service_from_plan,
    stateful_set, target_port,
};
use proofstorm_core::PaymentMethod;

pub(super) fn grpc_target(
    plan: &ComponentPlanContract,
) -> Result<Option<&TargetDescriptorContract>, AdapterError> {
    if plan.backend_id != "cdk" {
        return Ok(None);
    }
    if !plan.relevant_links.iter().any(|link| {
        link.kind == LinkKind::PaymentBackend
            && plan
                .linked_targets
                .get(&link.id)
                .is_some_and(|target| target.kind == ComponentKind::PaymentProcessor)
    }) {
        return Ok(None);
    }
    payment_target(plan, "cdk-ldk-server-processor").map(Some)
}

fn payment_target<'a>(
    plan: &'a ComponentPlanContract,
    implementation: &str,
) -> Result<&'a TargetDescriptorContract, AdapterError> {
    let links = plan
        .relevant_links
        .iter()
        .filter(|link| link.kind == LinkKind::PaymentBackend)
        .collect::<Vec<_>>();
    let methods = links
        .iter()
        .filter_map(|link| match &link.binding {
            Some(DependencyBinding::Payment { method, unit }) if unit == "sat" => Some(*method),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let target = links
        .first()
        .and_then(|link| plan.linked_targets.get(&link.id));
    if links.len() != 2
        || methods != BTreeSet::from([PaymentMethod::Bolt11, PaymentMethod::Bolt12])
        || !target.is_some_and(|target| {
            target.backend_id == implementation
                && links.iter().all(|link| {
                    link.to == target.component_id
                        && plan.linked_targets.get(&link.id) == Some(target)
                })
        })
    {
        return Err(AdapterError::InvalidPlan(format!(
            "component {:?} requires bolt11/sat and bolt12/sat bindings to one {implementation}",
            plan.component_id
        )));
    }
    Ok(target.expect("validated payment target"))
}

/// Render a persistent native LDK Server node.
/// # Errors
/// Rejects incompatible plans, missing dependencies and invalid resources.
/// # Panics
/// Panics if the internal `StatefulSet` builder stops returning container mounts.
pub fn render_node(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, "ldk-server", ComponentKind::Lightning)?;
    let EffectiveComponentConfig::LdkServer(config) = &plan.effective_config else {
        return Err(AdapterError::InvalidPlan(
            "LDK Server configuration missing".into(),
        ));
    };
    let chain = plan_linked_target(plan, LinkKind::ChainBackend)?;
    let rpc = target_port(chain, "rpc")?;
    let grpc = target_port(&plan.target_descriptor, "rpc")?;
    let p2p = target_port(&plan.target_descriptor, "p2p")?;
    let id = &plan.component_id;
    let namespace = instance_namespace(&plan.instance_key);
    let name = format!("{id}-config");
    let native = format!(
        "[node]\nnetwork = \"regtest\"\nalias = {}\nlistening_addresses = [\"0.0.0.0:{p2p}\"]\nannouncement_addresses = [\"{id}:{p2p}\"]\ngrpc_service_address = \"0.0.0.0:{grpc}\"\n\n[storage.disk]\ndir_path = \"/data\"\n\n[tls]\nhosts = [\"{id}\"]\n\n[bitcoind]\nrpc_address = \"{}:{rpc}\"\nrpc_user = \"{RPC_USER}\"\nrpc_password = \"{RPC_PASSWORD}\"\n",
        serde_json::to_string(&config.alias)?,
        chain.component_id
    );
    let mut rendered = RenderedComponent::default();
    rendered.config_maps.push(resource(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":metadata(&name,&plan.instance_key,&namespace,Some(id)),"data":{"config.toml":native}}))?);
    rendered.services.push(resource(service_from_plan(plan))?);
    let mut workload = stateful_set(
        &plan.instance_key,
        &namespace,
        id,
        &plan.execution_context.image,
        Some(vec!["ldk-server".into()]),
        &["/config/config.toml".into()],
        "/data",
        &json!({"exec":{"command":["ldk-server-cli","--config","/config/config.toml","--base-url",format!("127.0.0.1:{grpc}"),"get-node-info"]},"timeoutSeconds":3}),
        Some(plan),
    );
    let pod = &mut workload["spec"]["template"]["spec"];
    pod["containers"][0]["env"] = json!([{"name":"HOME","value":"/data"}]);
    pod["containers"][0]["volumeMounts"]
        .as_array_mut()
        .expect("node mounts")
        .push(json!({"name":"config","mountPath":"/config","readOnly":true}));
    pod["volumes"] = json!([{"name":"config","configMap":{"name":name}}]);
    rendered.stateful_sets.push(resource(workload)?);
    Ok(rendered)
}

/// Render the independently controlled LDK Server payment processor.
/// # Errors
/// Rejects ambiguous payment bindings, missing credentials and invalid resources.
pub fn render_processor(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(
        plan,
        "cdk-ldk-server-processor",
        ComponentKind::PaymentProcessor,
    )?;
    let EffectiveComponentConfig::LdkServerProcessor(config) = &plan.effective_config else {
        return Err(AdapterError::InvalidPlan(
            "LDK processor configuration missing".into(),
        ));
    };
    let node = payment_target(plan, "ldk-server")?;
    let credentials = plan_execution_credential(plan, "ldk-server")?;
    if credentials.source_component_id != node.component_id {
        return Err(AdapterError::InvalidPlan(
            "LDK credential target mismatch".into(),
        ));
    }
    let port = target_port(&plan.target_descriptor, "grpc")?;
    let node_port = target_port(node, "rpc")?;
    let id = &plan.component_id;
    let namespace = instance_namespace(&plan.instance_key);
    let labels = labels(&plan.instance_key, Some(id));
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.secrets.push(resource(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque","metadata":metadata(&format!("{id}-payment-tls"),&plan.instance_key,&namespace,Some(id)),"stringData":{"PROOFSTORM_SECRET_KIND":"payment-processor-tls","PROOFSTORM_TLS_SERVER_NAME":id}}))?);
    let env = BTreeMap::from([
        ("SERVER_ADDRESS", "0.0.0.0".into()),
        ("SERVER_PORT", port.to_string()),
        ("TLS_ENABLE", "true".into()),
        ("ALLOW_INSECURE", "false".into()),
        ("TLS_CERT_PATH", "/processor-server/tls/server.pem".into()),
        ("TLS_KEY_PATH", "/processor-server/tls/server.key".into()),
        ("TLS_CLIENT_CA_PATH", "/processor-server/tls/ca.pem".into()),
        ("LDK_ADDRESS", format!("{}:{node_port}", node.component_id)),
        ("LDK_TLS_CERT_PATH", "/ldk-server/tls.crt".into()),
        ("LDK_PAYMENT_METHODS", "bolt11,bolt12".into()),
        (
            "LDK_FEE_RESERVE_MIN_SAT",
            config.fee_reserve_min_sat.to_string(),
        ),
        (
            "LDK_FEE_RESERVE_PERCENT",
            config.fee_reserve_percent.to_string(),
        ),
        (
            "LDK_MAX_PAYMENT_SCAN_PAGES",
            config.max_payment_scan_pages.to_string(),
        ),
    ]);
    let env = env
        .iter()
        .map(|(name, value)| json!({"name":name,"value":value}))
        .collect::<Vec<_>>();
    let wait = format!(
        "for attempt in $(seq 1 120); do if test -s /ldk-server/regtest/api_key && test -s /ldk-server/tls.crt && /opt/proofstorm/driver tcp {} {node_port}; then exit 0; fi; sleep 1; done; echo 'LDK Server dependency did not become ready' >&2; exit 1",
        node.component_id
    );
    rendered.deployments.push(resource(json!({
        "apiVersion":"apps/v1","kind":"Deployment","metadata":plan_workload_metadata(plan),
        "spec":{"replicas":1,"strategy":{"type":"Recreate"},"selector":{"matchLabels":labels},"template":{"metadata":plan_pod_metadata(plan,&labels),"spec":{
            "serviceAccountName":"proofstorm-workload","automountServiceAccountToken":false,"enableServiceLinks":false,"securityContext":pod_security(1000),"affinity":instance_affinity(&plan.instance_key),
            "containers":[{"name":"component","image":plan.execution_context.image,"imagePullPolicy":"IfNotPresent","command":[crate::drivers::DRIVER_PATH,"exec-ldk-processor"],"env":env,"securityContext":container_security(),
                "ports":[{"name":"grpc","containerPort":port}],
                "readinessProbe":{"exec":{"command":[crate::drivers::DRIVER_PATH,"processor-settings",format!("https://127.0.0.1:{port}"),"/processor-client/tls"]},"timeoutSeconds":3,"periodSeconds":3},
                "volumeMounts":[{"name":"ldk-server","mountPath":"/ldk-server","readOnly":true},tls_mount("processor-server","/processor-server/tls"),tls_mount("processor-client","/processor-client/tls")]}],
            "initContainers":[{"name":"wait-for-ldk-server","image":plan.execution_context.image,"command":["sh","-ec",wait],"securityContext":container_security(),"volumeMounts":[{"name":"ldk-server","mountPath":"/ldk-server","readOnly":true},{"name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true}]}],
            "volumes":[{"name":"ldk-server","persistentVolumeClaim":{"claimName":credentials.claim_name}},tls_volume("processor-server",id,"server"),tls_volume("processor-client",id,"client")]
        }}}
    }))?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}

pub(super) fn tls_volume(name: &str, processor: &str, role: &str) -> Value {
    json!({"name":name,"secret":{"secretName":format!("{processor}-payment-tls"),"defaultMode":288,"items":[{"key":"ca.pem","path":"ca.pem"},{"key":format!("{role}.pem"),"path":format!("{role}.pem")},{"key":format!("{role}.key"),"path":format!("{role}.key")}]}})
}

pub(super) fn tls_mount(name: &str, path: &str) -> Value {
    json!({"name":name,"mountPath":path,"readOnly":true})
}

pub(super) fn mint_config(
    plan: &ComponentPlanContract,
    config: &CdkMintConfig,
    port: u16,
    target: &TargetDescriptorContract,
    database: &str,
) -> Result<String, AdapterError> {
    let grpc_port = target_port(target, "grpc")?;
    Ok(format!(
        "{}[payment_backend]\nbackend = \"grpcprocessor\"\nunit = \"sat\"\nmin_mint = {}\nmax_mint = {}\nmin_melt = {}\nmax_melt = {}\n\n[grpc_processor]\nsupported_units = [\"sat\"]\naddress = \"{}\"\nport = {grpc_port}\ntls_dir = \"/payment-processor/tls\"\nallow_insecure = false\n\n{database}",
        mint_common_config(&plan.component_id, port, config),
        config.min_mint_sat,
        config.max_mint_sat,
        config.min_melt_sat,
        config.max_melt_sat,
        target.component_id
    ))
}

pub(super) fn wait_for_processor(
    plan: &ComponentPlanContract,
    target: &TargetDescriptorContract,
) -> Result<Value, AdapterError> {
    let port = target_port(target, "grpc")?;
    Ok(
        json!({"name":"wait-for-payment-processor","image":plan.execution_context.image,"command":["sh","-ec",format!("for attempt in $(seq 1 120); do if /opt/proofstorm/driver processor-settings https://{}:{port} /payment-processor/tls; then exit 0; fi; sleep 1; done; echo 'Payment processor dependency did not become ready' >&2; exit 1",target.component_id)],"securityContext":container_security(),"volumeMounts":[tls_mount("payment-processor","/payment-processor/tls"),{"name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true}]}),
    )
}
