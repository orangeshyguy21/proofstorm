//! Reserved server/CLN backends. Images stay unpublished until managed qualification.
use super::{
    AdapterError, ComponentKind, ComponentPlanContract, DependencyBinding,
    EffectiveComponentConfig, LinkKind, RPC_PASSWORD, RPC_USER, RenderedComponent, Value,
    WorkloadControllerKind,
    bark::{dependency, required_port, service_name},
    container_security, install_component_driver, instance_namespace, json, metadata,
    require_plan_backend, resource, service_from_plan, stateful_set,
};
use proofstorm_core::{
    BitcoinNetwork, DatabaseRole, PaymentMethod,
    processor_ids::{BARK_SERVER, CLN_HOLD},
};

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
    if !service_name(&plan.component_id)
        || plan.target_descriptor.component_id != plan.component_id
        || plan.target_descriptor.backend_id != backend
        || plan.target_descriptor.kind != kind
        || plan.target_descriptor.ports != contract.service_ports
        || plan.execution_context.component_id != plan.component_id
        || plan.execution_context.state_contract != contract.execution_state_contract
        || plan.execution_context.mounts.len() != 1
        || plan.execution_context.mounts[0].name != "data"
        || plan.execution_context.mounts[0].mount_path != "/data"
        || plan.execution_context.mounts[0].read_only
        || !matches!(
            plan.execution_context.mounts[0].source,
            proofstorm_core::ExecutionStorageSource::StatefulData
        )
        || plan.workload.kind != WorkloadControllerKind::StatefulSet
        || plan.storage.len() != 1
        || plan.storage[0].claim_name != format!("data-{}-0", plan.component_id)
        || plan.relevant_links.iter().any(|link| {
            matches!(
                link.kind,
                LinkKind::ChainBackend
                    | LinkKind::ArkBackend
                    | LinkKind::PaymentBackend
                    | LinkKind::DatabaseBackend
                    | LinkKind::AuthenticationBackend
            ) && !links.contains(&link.kind)
        })
    {
        return Err(AdapterError::InvalidPlan(
            "Bark stack identity, storage or dependency contract mismatch".into(),
        ));
    }
    Ok(())
}

fn workload(plan: &ComponentPlanContract, command: &str, readiness: &Value) -> Value {
    stateful_set(
        &plan.instance_key,
        &instance_namespace(&plan.instance_key),
        &plan.component_id,
        &plan.execution_context.image,
        Some(vec![crate::drivers::DRIVER_PATH.into(), command.into()]),
        &[],
        "/data",
        readiness,
        Some(plan),
    )
}

fn config(
    plan: &ComponentPlanContract,
    data: &Value,
) -> Result<k8s_openapi::api::core::v1::ConfigMap, AdapterError> {
    resource(json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":metadata(&format!("{}-config", plan.component_id),&plan.instance_key,&instance_namespace(&plan.instance_key),Some(&plan.component_id)),"data":data}))
}

fn tls_volume(id: &str, role: &str, client_only: bool) -> Value {
    let keys: &[&str] = if client_only {
        &["ca.pem", "client.pem", "client.key"]
    } else {
        &[
            "ca.pem",
            "ca.key",
            "server.pem",
            "server.key",
            "client.pem",
            "client.key",
        ]
    };
    json!({"name":format!("{role}-tls"),"secret":{"secretName":format!("{id}-{role}-tls"),"defaultMode":288,
        "items":keys.iter().map(|key| json!({"key":key,"path":key.replace(".key", "-key.pem")})).collect::<Vec<_>>()}})
}

fn tls_mount(role: &str) -> Value {
    json!({"name":format!("{role}-tls"),"mountPath":format!("/{role}-tls"),"readOnly":true})
}

/// Render the matched CLN/hold pair with owned native state and separate TLS identities.
/// # Errors
/// Refuses incompatible plans and ambiguous or untyped dependencies.
pub fn render_cln_hold(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    validate(
        plan,
        CLN_HOLD,
        ComponentKind::Lightning,
        &[LinkKind::ChainBackend],
    )?;
    if plan.effective_config != EffectiveComponentConfig::ClnHold {
        return Err(AdapterError::InvalidPlan(
            "CLN/hold typed configuration missing".into(),
        ));
    }
    let chain = dependency(
        plan,
        LinkKind::ChainBackend,
        &DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest,
        },
        ComponentKind::Bitcoin,
        "bitcoin-core",
    )?;
    let rpc = required_port(chain, "rpc")?;
    let id = &plan.component_id;
    let alias = &id[..id.len().min(32)];
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    let native = format!(
        "network=regtest\nlightning-dir=/data\nalias={alias}\nbitcoin-rpcconnect={}\nbitcoin-rpcport={rpc}\nbitcoin-rpcuser={RPC_USER}\nbitcoin-rpcpassword={RPC_PASSWORD}\nbind-addr=0.0.0.0:9735\nannounce-addr={id}:9735\ngrpc-host=0.0.0.0\ngrpc-port=9988\nplugin=/usr/local/bin/hold\nhold-grpc-host=0.0.0.0\nhold-grpc-port=9292\nhold-database=sqlite:///data/regtest/hold/hold.sqlite3\nautoconnect-seeker-peers=0\n",
        chain.component_id
    );
    rendered.config_maps.push(config(
        plan,
        &json!({"lightning.conf":native,"rpc.cookie":format!("{RPC_USER}:{RPC_PASSWORD}\n")}),
    )?);
    for role in ["cln", "hold"] {
        rendered.secrets.push(resource(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":metadata(&format!("{id}-{role}-tls"),&plan.instance_key,&instance_namespace(&plan.instance_key),Some(id)),
            "stringData":{"PROOFSTORM_SECRET_KIND":format!("bark-{role}-tls"),"PROOFSTORM_TLS_SERVER_NAME":id}}))?);
    }
    let mut workload = workload(
        plan,
        "exec-cln-hold",
        &json!({"exec":{"command":[crate::drivers::DRIVER_PATH,"cln-hold-ready"]},"timeoutSeconds":5}),
    );
    let pod = &mut workload["spec"]["template"]["spec"];
    pod["containers"][0]["env"] = json!([{"name":"HOME","value":"/data"}]);
    pod["containers"][0]["ports"] = json!([{"name":"p2p","containerPort":9735},{"name":"grpc","containerPort":9988},{"name":"hold","containerPort":9292}]);
    pod["containers"][0]["volumeMounts"] = json!([{"name":"data","mountPath":"/data"},{"name":"config","mountPath":"/config","readOnly":true},tls_mount("cln"),tls_mount("hold")]);
    pod["volumes"] = json!([{"name":"config","configMap":{"name":format!("{id}-config"),"defaultMode":288}},tls_volume(id,"cln",false),tls_volume(id,"hold",false)]);
    pod["initContainers"] = json!([wait_chain(
        plan,
        &format!("http://{}:{rpc}", chain.component_id),
        "/config/rpc.cookie",
        "/config"
    )]);
    rendered.stateful_sets.push(resource(workload)?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}

/// Render Bark with private admin APIs, client-only Lightning TLS and guarded native initialization.
/// # Errors
/// Refuses incompatible plans, missing dependencies and invalid database identities.
pub fn render_server(plan: &ComponentPlanContract) -> Result<RenderedComponent, AdapterError> {
    validate(
        plan,
        BARK_SERVER,
        ComponentKind::ArkServer,
        &[
            LinkKind::ChainBackend,
            LinkKind::PaymentBackend,
            LinkKind::DatabaseBackend,
        ],
    )?;
    if plan.effective_config != EffectiveComponentConfig::BarkServer {
        return Err(AdapterError::InvalidPlan(
            "Bark server typed configuration missing".into(),
        ));
    }
    let chain = dependency(
        plan,
        LinkKind::ChainBackend,
        &DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest,
        },
        ComponentKind::Bitcoin,
        "bitcoin-core",
    )?;
    let cln = dependency(
        plan,
        LinkKind::PaymentBackend,
        &DependencyBinding::Payment {
            method: PaymentMethod::Bolt11,
            unit: "sat".into(),
        },
        ComponentKind::Lightning,
        CLN_HOLD,
    )?;
    let database = server_database(plan)?;
    let env = server_env(chain, cln, &database)?;
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.config_maps.push(config(
        plan,
        &json!({"rpc.cookie":format!("{RPC_USER}:{RPC_PASSWORD}\n")}),
    )?);
    let mut workload = workload(
        plan,
        "exec-bark-server",
        &json!({"exec":{"command":["captaind","rpc","wallet"]},"timeoutSeconds":5}),
    );
    server_pod(
        plan,
        &mut workload["spec"]["template"]["spec"],
        &env,
        cln,
        &database,
        &format!(
            "http://{}:{}",
            chain.component_id,
            required_port(chain, "rpc")?
        ),
    );
    rendered.stateful_sets.push(resource(workload)?);
    install_component_driver(&mut rendered)?;
    Ok(rendered)
}

fn server_database(plan: &ComponentPlanContract) -> Result<super::PostgresDatabase, AdapterError> {
    let database_link = plan
        .relevant_links
        .iter()
        .find(|link| link.kind == LinkKind::DatabaseBackend)
        .ok_or_else(|| AdapterError::InvalidPlan("Bark database binding missing".into()))?;
    let database_binding = database_link
        .binding
        .as_ref()
        .ok_or_else(|| AdapterError::InvalidPlan("Bark database binding missing".into()))?;
    dependency(
        plan,
        LinkKind::DatabaseBackend,
        database_binding,
        ComponentKind::Database,
        "postgresql",
    )?;
    if !matches!(
        database_binding,
        DependencyBinding::Database {
            role: DatabaseRole::Primary,
            ..
        }
    ) {
        return Err(AdapterError::InvalidPlan(
            "Bark requires a primary database".into(),
        ));
    }
    let name = database_link.database_name().unwrap_or_default();
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(AdapterError::InvalidPlan(
            "Bark database name must be a PostgreSQL identifier".into(),
        ));
    }
    let database = super::linked_postgres_database(plan, DatabaseRole::Primary)?
        .ok_or_else(|| AdapterError::InvalidPlan("Bark database missing".into()))?;
    if database.port == 0 {
        return Err(AdapterError::InvalidPlan(
            "Bark database port must be nonzero".into(),
        ));
    }
    Ok(database)
}

fn wait_chain(plan: &ComponentPlanContract, url: &str, cookie: &str, mount: &str) -> Value {
    json!({"name":"wait-for-bitcoin","image":plan.execution_context.image,
        "command":[crate::drivers::DRIVER_PATH,"wait-bark-chain",url,cookie],"securityContext":container_security(),
        "volumeMounts":[{"name":"config","mountPath":mount,"readOnly":true},{"name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true}]})
}

fn server_env(
    chain: &super::TargetDescriptorContract,
    cln: &super::TargetDescriptorContract,
    db: &super::PostgresDatabase,
) -> Result<Value, AdapterError> {
    let cln_config = json!([{"uri":format!("https://{}:{}",cln.component_id,required_port(cln,"grpc")?),"priority":0,
        "server_cert_path":"/cln-tls/ca.pem","client_cert_path":"/cln-tls/client.pem","client_key_path":"/cln-tls/client-key.pem",
        "hold_invoice":{"uri":format!("https://{}:{}",cln.component_id,required_port(cln,"hold")?),
            "server_cert_path":"/hold-tls/ca.pem","client_cert_path":"/hold-tls/client.pem","client_key_path":"/hold-tls/client-key.pem"}}]);
    let mut env: Vec<Value> = [
        ("DATA_DIR", "/data/native".into()),
        ("NETWORK", "regtest".into()),
        ("RPC__PUBLIC_ADDRESS", "0.0.0.0:3535".into()),
        ("RPC__ADMIN_ADDRESS", "127.0.0.1:3536".into()),
        ("RPC__INTEGRATION_ADDRESS", "127.0.0.1:3537".into()),
        (
            "BITCOIND__URL",
            format!(
                "http://{}:{}",
                chain.component_id,
                required_port(chain, "rpc")?
            ),
        ),
        ("BITCOIND__COOKIE", "/chain-rpc/rpc.cookie".into()),
        ("POSTGRES__HOST", db.host.clone()),
        ("POSTGRES__PORT", db.port.to_string()),
        ("POSTGRES__NAME", db.database.clone()),
        ("POSTGRES__USER", super::POSTGRES_OWNER.into()),
        ("CLN_ARRAY", cln_config.to_string()),
    ]
    .into_iter()
    .map(|(key, value)| json!({"name":format!("BARK_SERVER__{key}"),"value":value}))
    .collect();
    env.push(json!({"name":"BARK_SERVER__POSTGRES__PASSWORD","valueFrom":{"secretKeyRef":{"name":db.secret,"key":"POSTGRES_PASSWORD"}}}));
    Ok(json!(env))
}

fn server_pod(
    plan: &ComponentPlanContract,
    pod: &mut Value,
    env: &Value,
    cln: &super::TargetDescriptorContract,
    db: &super::PostgresDatabase,
    chain_url: &str,
) {
    let mut mounts = json!([{"name":"data","mountPath":"/data"},{"name":"config","mountPath":"/chain-rpc","readOnly":true},tls_mount("cln"),tls_mount("hold"),{"name":"runtime","mountPath":"/runtime"}]);
    pod["containers"][0]["env"] = env.clone();
    pod["containers"][0]["ports"] = json!([{"name":"rpc","containerPort":3535}]);
    pod["containers"][0]["volumeMounts"] = mounts.clone();
    mounts
        .as_array_mut()
        .expect("mounts")
        .push(json!({"name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true}));
    pod["volumes"] = json!([{"name":"config","configMap":{"name":format!("{}-config",plan.component_id),"defaultMode":288}},tls_volume(&cln.component_id,"cln",true),tls_volume(&cln.component_id,"hold",true),{"name":"runtime","emptyDir":{"medium":"Memory"}}]);
    let database_guard = |mode: &str| {
        json!({"name":format!("{mode}-bark-database"),"image":db.image,
        "command":["sh","-ec",include_str!("../drivers/bark_database.sh"),"--",mode,db.host,db.port.to_string(),db.database],
        "env":[{"name":"PGPASSWORD","valueFrom":{"secretKeyRef":{"name":db.secret,"key":"POSTGRES_PASSWORD"}}}],
        "securityContext":container_security(),"volumeMounts":[{"name":"data","mountPath":"/data","readOnly":true},{"name":"runtime","mountPath":"/runtime","readOnly":true}]})
    };
    pod["initContainers"] = json!([
        wait_chain(plan, chain_url, "/chain-rpc/rpc.cookie", "/chain-rpc"),
        database_guard("check"),
        {"name":"initialize-bark-server","image":plan.execution_context.image,"command":[crate::drivers::DRIVER_PATH,"prepare-bark-server"],"env":env,"securityContext":container_security(),"volumeMounts":mounts},
        database_guard("seal")]);
}
