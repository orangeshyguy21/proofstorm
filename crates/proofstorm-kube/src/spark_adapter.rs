//! Experimental Spark stack: an operator federation, the open-ssp service
//! provider and the CDK Spark processor. Secrets are templates; the controller
//! generates their contents once and never rotates them.
use super::{
    AdapterError, ComponentKind, ComponentPlanContract, DependencyBinding,
    EffectiveComponentConfig, LinkKind, PostgresDatabase, RPC_PASSWORD, RPC_USER,
    RenderedComponent, TargetDescriptorContract, Value, bark::service_name, container_security,
    install_component_driver, instance_namespace, json, linked_postgres_database, metadata,
    plan_execution_credential, processor, require_plan_backend, resource, service_from_plan,
    stateful_set, target_port,
};
use proofstorm_core::{
    BitcoinNetwork, DatabaseRole, PaymentMethod,
    processor_ids::{OPEN_SSP, SPARK_OPERATORS, SPARK_PROCESSOR},
};

const OPERATORS_MOUNT: &str = "/spark-operators";
/// What consumers may read of a federation; its keys never leave its own pods.
const PUBLIC_FILES: [&str; 3] = ["ca.crt", "operators.json", "threshold"];
/// Operators append `_<n>_ephemeral` to their linked database name.
const DATABASE_NAME_LIMIT: usize = 63 - "_4_ephemeral".len();

fn spark_regtest() -> DependencyBinding {
    DependencyBinding::Spark {
        network: BitcoinNetwork::Regtest,
    }
}

/// The plan matches its backend contract, and carries exactly the expected
/// number of links, all of the listed kinds.
fn validate(
    plan: &ComponentPlanContract,
    backend: &str,
    kind: ComponentKind,
    links: &[LinkKind],
) -> Result<(), AdapterError> {
    require_plan_backend(plan, backend, kind)?;
    let contract = proofstorm_core::default_backend_registry()
        .require(backend)
        .map_err(AdapterError::InvalidPlan)?;
    let mounts_match = plan.execution_context.mounts.len() == contract.execution_mounts.len()
        && contract.execution_mounts.iter().all(|expected| {
            plan.execution_context.mounts.iter().any(|actual| {
                actual.name == expected.name
                    && actual.mount_path == expected.mount_path
                    && actual.read_only == expected.read_only
            })
        });
    let storage_matches = if contract.storage_requirements.is_empty() {
        plan.storage.is_empty()
    } else {
        plan.storage.len() == 1
            && plan.storage[0].claim_name == format!("data-{}-0", plan.component_id)
    };
    if !service_name(&plan.component_id)
        || plan.target_descriptor.component_id != plan.component_id
        || plan.target_descriptor.backend_id != backend
        || plan.target_descriptor.kind != kind
        || plan.target_descriptor.ports != contract.service_ports
        || plan.execution_context.component_id != plan.component_id
        || plan.execution_context.state_contract != contract.execution_state_contract
        || !mounts_match
        || plan.workload.kind != contract.workload_kind
        || !storage_matches
        || plan.relevant_links.len() != links.len()
        || plan
            .relevant_links
            .iter()
            .any(|link| !links.contains(&link.kind))
    {
        return Err(AdapterError::InvalidPlan(format!(
            "{backend} identity, storage or dependency contract mismatch"
        )));
    }
    Ok(())
}

/// The one `kind` link from `from` with this binding, resolved to a component
/// of the given kind and implementation.
fn dependency<'a>(
    plan: &'a ComponentPlanContract,
    from: &str,
    kind: LinkKind,
    binding: &DependencyBinding,
    target_kind: ComponentKind,
    implementation: &str,
) -> Result<&'a TargetDescriptorContract, AdapterError> {
    let links = plan
        .relevant_links
        .iter()
        .filter(|link| link.from == from && link.kind == kind)
        .collect::<Vec<_>>();
    let [link] = links.as_slice() else {
        return Err(missing(from, kind, implementation));
    };
    plan.linked_targets
        .get(&link.id)
        .filter(|target| {
            link.binding.as_ref() == Some(binding)
                && target.component_id == link.to
                && target.kind == target_kind
                && target.backend_id == implementation
                && service_name(&target.component_id)
        })
        .ok_or_else(|| missing(from, kind, implementation))
}

fn missing(from: &str, kind: LinkKind, implementation: &str) -> AdapterError {
    AdapterError::InvalidPlan(format!(
        "Spark component {from:?} requires one {kind:?} binding to {implementation}"
    ))
}

fn port(target: &TargetDescriptorContract, name: &str) -> Result<u16, AdapterError> {
    match target_port(target, name)? {
        0 => Err(AdapterError::InvalidPlan(format!(
            "Spark port {name:?} of {:?} must be nonzero",
            target.component_id
        ))),
        port => Ok(port),
    }
}

fn config_map(
    plan: &ComponentPlanContract,
    data: &Value,
) -> Result<k8s_openapi::api::core::v1::ConfigMap, AdapterError> {
    resource(json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":metadata(&format!("{}-config", plan.component_id),&plan.instance_key,&instance_namespace(&plan.instance_key),Some(&plan.component_id)),
        "data":data}))
}

fn secret(
    plan: &ComponentPlanContract,
    suffix: &str,
    data: &Value,
) -> Result<k8s_openapi::api::core::v1::Secret, AdapterError> {
    resource(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":metadata(&format!("{}-{suffix}", plan.component_id),&plan.instance_key,&instance_namespace(&plan.instance_key),Some(&plan.component_id)),
        "stringData":data}))
}

/// A consumer's read-only view of its federation: the CA, the operator list and the threshold.
fn public_operators_volume(operators: &str) -> Value {
    json!({"name":"spark-operators","secret":{"secretName":format!("{operators}-identity"),"defaultMode":288,
        "items":PUBLIC_FILES.iter().map(|key| json!({"key":key,"path":key})).collect::<Vec<_>>()}})
}

fn operators_mount() -> Value {
    json!({"name":"spark-operators","mountPath":OPERATORS_MOUNT,"readOnly":true})
}

fn env(pairs: &[(&str, String)]) -> Vec<Value> {
    pairs
        .iter()
        .map(|(name, value)| json!({"name":name,"value":value}))
        .collect()
}

/// Wait until the fixed `files` are non-empty and every `host:port` accepts
/// TCP. Positional arguments keep component identities out of shell source.
fn wait_for(
    plan: &ComponentPlanContract,
    name: &str,
    files: &[&str],
    targets: &[(&str, u16)],
    mounts: Vec<Value>,
) -> Value {
    let checks = files.iter().fold(String::new(), |mut checks, file| {
        checks.push_str("test -s ");
        checks.push_str(file);
        checks.push_str(" || ready=0; ");
        checks
    });
    let script = format!(
        "n=0; while [ \"$n\" -lt 120 ]; do ready=1; {checks}for target in \"$@\"; do /opt/proofstorm/driver tcp \"${{target%:*}}\" \"${{target##*:}}\" || ready=0; done; [ \"$ready\" = 1 ] && exit 0; n=$((n+1)); sleep 1; done; echo '{name} dependencies did not become ready' >&2; exit 1"
    );
    let mut volume_mounts = mounts;
    volume_mounts
        .push(json!({"name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true}));
    let mut command = vec![json!("sh"), json!("-ec"), json!(script), json!("--")];
    command.extend(
        targets
            .iter()
            .map(|(host, port)| json!(format!("{host}:{port}"))),
    );
    json!({"name":format!("wait-for-{name}-dependencies"),"image":plan.execution_context.image,"imagePullPolicy":"IfNotPresent",
        "command":command,"securityContext":container_security(),"volumeMounts":volume_mounts})
}

/// The operators' primary database binding, whose name prefixes each operator's databases.
fn operator_database(plan: &ComponentPlanContract) -> Result<PostgresDatabase, AdapterError> {
    let link = plan
        .relevant_links
        .iter()
        .find(|link| link.kind == LinkKind::DatabaseBackend)
        .ok_or_else(|| missing(&plan.component_id, LinkKind::DatabaseBackend, "postgresql"))?;
    let binding = link
        .binding
        .as_ref()
        .filter(|binding| {
            matches!(
                binding,
                DependencyBinding::Database {
                    role: DatabaseRole::Primary,
                    ..
                }
            )
        })
        .ok_or_else(|| missing(&plan.component_id, LinkKind::DatabaseBackend, "postgresql"))?;
    dependency(
        plan,
        &plan.component_id,
        LinkKind::DatabaseBackend,
        binding,
        ComponentKind::Database,
        "postgresql",
    )?;
    let database = linked_postgres_database(plan, DatabaseRole::Primary)?
        .ok_or_else(|| missing(&plan.component_id, LinkKind::DatabaseBackend, "postgresql"))?;
    let name = &database.database;
    if name.is_empty()
        || name.len() > DATABASE_NAME_LIMIT
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        || database.port == 0
    {
        return Err(AdapterError::InvalidPlan(format!(
            "Spark operator database name must be a PostgreSQL identifier of at most {DATABASE_NAME_LIMIT} bytes"
        )));
    }
    Ok(database)
}

/// Render the operator federation: one pod per operator with a FROST signer
/// sidecar, stable per-pod DNS names, and its own pair of databases.
/// # Errors
/// Refuses incompatible plans, missing dependencies and invalid database names.
pub fn render_operators(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    validate(
        plan,
        SPARK_OPERATORS,
        ComponentKind::SparkServer,
        &[LinkKind::ChainBackend, LinkKind::DatabaseBackend],
    )?;
    let EffectiveComponentConfig::SparkOperators(config) = &plan.effective_config else {
        return Err(AdapterError::InvalidPlan(
            "Spark operator configuration missing".into(),
        ));
    };
    if !(1..=5).contains(&config.operators)
        || !(1..=config.operators).contains(&config.threshold)
        || u64::from(plan.workload.desired_replicas) != config.operators
    {
        return Err(AdapterError::InvalidPlan(
            "Spark federation needs 1 to 5 operators and a threshold of at most that many".into(),
        ));
    }
    let id = &plan.component_id;
    let chain = dependency(
        plan,
        id,
        LinkKind::ChainBackend,
        &DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest,
        },
        ComponentKind::Bitcoin,
        "bitcoin-core",
    )?;
    let rpc = port(chain, "rpc")?;
    let zmq = port(chain, "zmq_block")?;
    let database = operator_database(plan)?;
    let grpc = port(&plan.target_descriptor, "grpc")?;
    let ssp = port(&plan.target_descriptor, "ssp")?;
    let mut rendered = RenderedComponent::default();
    // Headless, so each operator has a stable name its peers and clients dial.
    let mut service = service_from_plan(plan);
    service["spec"]["clusterIP"] = json!("None");
    rendered.services.push(resource(service)?);
    let bitcoind = format!(
        "bitcoind:\n  regtest:\n    network: regtest\n    host: {}:{rpc}\n    rpcuser: {RPC_USER}\n    rpcpassword: {RPC_PASSWORD}\n    zmqpubrawblock: tcp://{}:{zmq}\n",
        chain.component_id, chain.component_id
    );
    rendered.config_maps.push(config_map(
        plan,
        &json!({"operator.config.yaml":format!("{bitcoind}{}", include_str!("../drivers/spark_operator.config.yaml"))}),
    )?);
    rendered.secrets.push(secret(
        plan,
        "identity",
        &json!({"PROOFSTORM_SECRET_KIND":"spark-operators",
            "PROOFSTORM_SPARK_OPERATORS":config.operators.to_string(),
            "PROOFSTORM_SPARK_THRESHOLD":config.threshold.to_string(),
            "PROOFSTORM_SPARK_SERVICE":id}),
    )?);
    let workload = operators_workload(plan, &database, (&chain.component_id, rpc), grpc, ssp);
    rendered.stateful_sets.push(resource(workload)?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}

/// The operators' `StatefulSet`: no claims, because their state lives in
/// `PostgreSQL`, and every pod started together.
fn operators_workload(
    plan: &ComponentPlanContract,
    database: &PostgresDatabase,
    chain: (&str, u16),
    grpc: u16,
    ssp: u16,
) -> Value {
    let id = &plan.component_id;
    let mut workload = stateful_set(
        &plan.instance_key,
        &instance_namespace(&plan.instance_key),
        id,
        &plan.execution_context.image,
        Some(vec![
            crate::drivers::DRIVER_PATH.into(),
            "exec-spark-operator".into(),
        ]),
        &[],
        "/data",
        &json!({"exec":{"command":[crate::drivers::DRIVER_PATH,"spark-operator-ready"]},"timeoutSeconds":3}),
        Some(plan),
    );
    workload["spec"]["replicas"] = json!(plan.workload.desired_replicas);
    // Every operator must run for the federation's key generation.
    workload["spec"]["podManagementPolicy"] = json!("Parallel");
    workload["spec"]
        .as_object_mut()
        .expect("StatefulSet spec")
        .remove("volumeClaimTemplates");
    let pod = &mut workload["spec"]["template"]["spec"];
    let password = json!({"name":"PGPASSWORD","valueFrom":{"secretKeyRef":{"name":database.secret,"key":"POSTGRES_PASSWORD"}}});
    let mut component_env = env(&[
        ("PROOFSTORM_SPARK_DATABASE_HOST", database.host.clone()),
        ("PROOFSTORM_SPARK_DATABASE_PORT", database.port.to_string()),
        ("PROOFSTORM_SPARK_DATABASE_NAME", database.database.clone()),
    ]);
    component_env.push(password.clone());
    pod["containers"][0]["env"] = json!(component_env);
    pod["containers"][0]["ports"] =
        json!([{"name":"grpc","containerPort":grpc},{"name":"ssp","containerPort":ssp}]);
    pod["containers"][0]["volumeMounts"] = json!([
        {"name":"identity","mountPath":OPERATORS_MOUNT,"readOnly":true},
        {"name":"config","mountPath":"/config","readOnly":true},
        {"name":"signer","mountPath":"/run/spark"}]);
    pod["containers"]
        .as_array_mut()
        .expect("containers")
        .push(json!({"name":"signer","image":plan.execution_context.image,"imagePullPolicy":"IfNotPresent",
            "command":[crate::drivers::DRIVER_PATH,"exec-spark-signer"],"securityContext":container_security(),
            "volumeMounts":[{"name":"signer","mountPath":"/run/spark"}]}));
    pod["volumes"] = json!([
        {"name":"identity","secret":{"secretName":format!("{id}-identity"),"defaultMode":288}},
        {"name":"config","configMap":{"name":format!("{id}-config"),"defaultMode":288}},
        {"name":"signer","emptyDir":{"medium":"Memory"}}]);
    pod["initContainers"] = json!([
        {"name":"ensure-spark-databases","image":database.image,"imagePullPolicy":"IfNotPresent",
            "command":["sh","-ec",include_str!("../drivers/spark_operator_database.sh"),"ensure-spark-databases",database.host,database.port.to_string(),database.database],
            "env":[password,{"name":"PROOFSTORM_POD_NAME","valueFrom":{"fieldRef":{"fieldPath":"metadata.name"}}}],
            "securityContext":container_security(),
            "volumeMounts":[{"name":"identity","mountPath":OPERATORS_MOUNT,"readOnly":true}]},
        wait_for(plan, "spark-operator", &[], &[chain], vec![])]);
    workload
}

/// Render open-ssp with a retained identity, the federation's public material
/// and its LDK Server's macaroon read from the node's own storage.
/// # Errors
/// Refuses incompatible plans, missing dependencies and mismatched credentials.
pub fn render_ssp(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    validate(
        plan,
        OPEN_SSP,
        ComponentKind::SparkServer,
        &[LinkKind::SparkBackend, LinkKind::PaymentBackend],
    )?;
    let EffectiveComponentConfig::OpenSsp(config) = &plan.effective_config else {
        return Err(AdapterError::InvalidPlan(
            "open-ssp configuration missing".into(),
        ));
    };
    let id = &plan.component_id;
    let operators = dependency(
        plan,
        id,
        LinkKind::SparkBackend,
        &spark_regtest(),
        ComponentKind::SparkServer,
        SPARK_OPERATORS,
    )?;
    let node = dependency(
        plan,
        id,
        LinkKind::PaymentBackend,
        &DependencyBinding::Payment {
            method: PaymentMethod::Bolt11,
            unit: "sat".into(),
        },
        ComponentKind::Lightning,
        "ldk-server",
    )?;
    let credentials = plan_execution_credential(plan, "ldk-server")?;
    if credentials.source_component_id != node.component_id {
        return Err(AdapterError::InvalidPlan(
            "open-ssp LDK credential target mismatch".into(),
        ));
    }
    let http = port(&plan.target_descriptor, "http")?;
    let node_rpc = port(node, "rpc")?;
    let operator_grpc = port(operators, "grpc")?;
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.secrets.push(secret(
        plan,
        "identity",
        &json!({"PROOFSTORM_SECRET_KIND":"open-ssp"}),
    )?);
    let mut workload = stateful_set(
        &plan.instance_key,
        &instance_namespace(&plan.instance_key),
        id,
        &plan.execution_context.image,
        Some(vec![
            crate::drivers::DRIVER_PATH.into(),
            "exec-open-ssp".into(),
        ]),
        &[],
        "/data",
        &json!({"exec":{"command":[crate::drivers::DRIVER_PATH,"open-ssp-ready"]},"timeoutSeconds":5}),
        Some(plan),
    );
    let pod = &mut workload["spec"]["template"]["spec"];
    pod["containers"][0]["env"] =
        json!(ssp_env(plan, config, http, (&node.component_id, node_rpc)));
    pod["containers"][0]["ports"] = json!([{"name":"http","containerPort":http}]);
    let ldk_mount = json!({"name":"ldk-server","mountPath":"/ldk-server","readOnly":true});
    pod["containers"][0]["volumeMounts"] = json!([
        {"name":"data","mountPath":"/data"},
        operators_mount(),
        {"name":"ssp-identity","mountPath":"/ssp-identity","readOnly":true},
        // open-ssp insists on owning its seed file; the driver copies it here.
        {"name":"ssp-seed","mountPath":"/run/open-ssp"},
        ldk_mount.clone()]);
    pod["volumes"] = json!([
        public_operators_volume(&operators.component_id),
        {"name":"ssp-identity","secret":{"secretName":format!("{id}-identity"),"defaultMode":288,
            "items":[{"key":"mnemonic","path":"mnemonic"},{"key":"admin-token","path":"admin-token"}]}},
        {"name":"ssp-seed","emptyDir":{"medium":"Memory"}},
        {"name":"ldk-server","persistentVolumeClaim":{"claimName":credentials.claim_name,"readOnly":true}}]);
    pod["initContainers"] = json!([wait_for(
        plan,
        "open-ssp",
        &[
            "/ldk-server/regtest/macaroons/admin.macaroon",
            "/ldk-server/tls.crt"
        ],
        &[
            (&node.component_id, node_rpc),
            (&operators.component_id, operator_grpc)
        ],
        vec![ldk_mount],
    )]);
    rendered.stateful_sets.push(resource(workload)?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}

/// open-ssp's native settings; the driver adds the operator list and secrets.
fn ssp_env(
    plan: &ComponentPlanContract,
    config: &proofstorm_core::OpenSspConfig,
    http: u16,
    (node, node_rpc): (&str, u16),
) -> Vec<Value> {
    let id = &plan.component_id;
    // Fees must stay zero: open-ssp refuses fee-bearing swaps.
    env(&[
        ("SSP_LISTEN_ADDR", format!("0.0.0.0:{http}")),
        ("SSP_NETWORK", "REGTEST".into()),
        ("SSP_PUBLIC_URL", format!("http://{id}:{http}")),
        ("SSP_DATA_DIR", "/data".into()),
        ("SSP_MIN_SPLIT_CHILD_SATS", "1".into()),
        ("SSP_SWAP_FEE_SATS", "0".into()),
        (
            "MAX_SWAP_TOTAL_SATS",
            config.max_swap_total_sats.to_string(),
        ),
        ("LDK_BACKEND", "server".into()),
        ("LDK_GRPC_ADDR", format!("{node}:{node_rpc}")),
        (
            "LDK_MACAROON_FILE",
            "/ldk-server/regtest/macaroons/admin.macaroon".into(),
        ),
        ("LDK_TLS_CERT_FILE", "/ldk-server/tls.crt".into()),
        ("RUST_LOG", "info".into()),
    ])
}

/// Render the persistent CDK Spark processor. It links only to its SSP and
/// reaches that SSP's operators through the SSP's own link.
/// # Errors
/// Refuses incompatible plans and ambiguous or missing dependencies.
pub fn render_processor(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    validate(
        plan,
        SPARK_PROCESSOR,
        ComponentKind::PaymentProcessor,
        &[LinkKind::SparkBackend, LinkKind::SparkBackend],
    )?;
    if plan.effective_config != EffectiveComponentConfig::SparkProcessor {
        return Err(AdapterError::InvalidPlan(
            "Spark processor configuration missing".into(),
        ));
    }
    let id = &plan.component_id;
    let ssp = dependency(
        plan,
        id,
        LinkKind::SparkBackend,
        &spark_regtest(),
        ComponentKind::SparkServer,
        OPEN_SSP,
    )?;
    let operators = dependency(
        plan,
        &ssp.component_id,
        LinkKind::SparkBackend,
        &spark_regtest(),
        ComponentKind::SparkServer,
        SPARK_OPERATORS,
    )?;
    let grpc = port(&plan.target_descriptor, "grpc")?;
    let ssp_http = port(ssp, "http")?;
    let operator_grpc = port(operators, "grpc")?;
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.secrets.push(secret(
        plan,
        "identity",
        &json!({"PROOFSTORM_SECRET_KIND":"spark-processor"}),
    )?);
    rendered.secrets.push(secret(
        plan,
        "payment-tls",
        &json!({"PROOFSTORM_SECRET_KIND":"payment-processor-tls","PROOFSTORM_TLS_SERVER_NAME":id}),
    )?);
    let mut workload = stateful_set(
        &plan.instance_key,
        &instance_namespace(&plan.instance_key),
        id,
        &plan.execution_context.image,
        Some(vec![
            crate::drivers::DRIVER_PATH.into(),
            "exec-spark-processor".into(),
        ]),
        &[],
        "/data",
        &json!({"exec":{"command":[crate::drivers::DRIVER_PATH,"processor-settings",format!("https://127.0.0.1:{grpc}"),"/processor-client/tls",SPARK_PROCESSOR]},"timeoutSeconds":3}),
        Some(plan),
    );
    let pod = &mut workload["spec"]["template"]["spec"];
    pod["containers"][0]["env"] = json!(env(&[
        (
            "PROOFSTORM_SPARK_SSP_URL",
            format!("http://{}:{ssp_http}", ssp.component_id)
        ),
        ("PROOFSTORM_SPARK_PROCESSOR_PORT", grpc.to_string()),
        ("RUST_LOG", "info".into()),
    ]));
    pod["containers"][0]["ports"] = json!([{"name":"grpc","containerPort":grpc}]);
    pod["containers"][0]["volumeMounts"] = json!([
        {"name":"data","mountPath":"/data"},
        operators_mount(),
        {"name":"identity","mountPath":"/processor-identity","readOnly":true},
        processor::tls_mount("processor-server","/processor-server/tls"),
        processor::tls_mount("processor-client","/processor-client/tls"),
        {"name":"config","mountPath":"/run/spark-processor"}]);
    pod["volumes"] = json!([
        public_operators_volume(&operators.component_id),
        {"name":"identity","secret":{"secretName":format!("{id}-identity"),"defaultMode":288,"items":[{"key":"mnemonic","path":"mnemonic"}]}},
        processor::tls_volume("processor-server",id,"server"),
        processor::tls_volume("processor-client",id,"client"),
        {"name":"config","emptyDir":{"medium":"Memory"}}]);
    pod["initContainers"] = json!([wait_for(
        plan,
        "spark-processor",
        &[],
        &[
            (&ssp.component_id, ssp_http),
            (&operators.component_id, operator_grpc)
        ],
        vec![],
    )]);
    rendered.stateful_sets.push(resource(workload)?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}
