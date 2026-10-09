//! Backend/rendering contracts only: no fabricated Bark catalog image or live qualification.
use proofstorm_core::{
    CatalogPlatform, CellSpec, ComponentConditionType, ComponentKind, ComponentPlanContract,
    DependencyBinding, EffectiveComponentConfig, LinkKind, PaymentMethod, WorkloadControllerKind,
    catalog_for_platform, default_backend_registry, processor_ids::BARK_PROCESSOR, resolve_lock,
};
use proofstorm_kube::render_bark_processor_component;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[path = "support/bark.rs"]
mod bark;
use bark::plan;

fn pod(plan: &ComponentPlanContract) -> Value {
    let rendered = render_bark_processor_component(plan).unwrap();
    assert!(rendered.deployments.is_empty());
    serde_json::to_value(&rendered.stateful_sets[0]).unwrap()["spec"]["template"]["spec"].clone()
}

fn env(pod: &Value) -> BTreeMap<&str, &str> {
    pod["containers"][0]["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["value"].as_str().unwrap()))
        .collect()
}

#[test]
fn contract_requires_owned_storage_and_keeps_runtime_settings_out_of_authored_config() {
    let plan = plan();
    assert_eq!(plan.workload.kind, WorkloadControllerKind::StatefulSet);
    assert_eq!(plan.storage[0].claim_name, "data-processor-0");
    assert!(
        plan.applicable_conditions
            .contains(&ComponentConditionType::StorageReady)
    );
    assert!(
        plan.applicable_conditions
            .contains(&ComponentConditionType::DependenciesReady)
    );
    let registry = default_backend_registry();
    let cell: CellSpec = serde_json::from_str(include_str!(
        "../../proofstorm-core/tests/fixtures/bark-topology.json"
    ))
    .unwrap();
    let component = cell
        .components
        .iter()
        .find(|c| c.id == "processor")
        .unwrap();
    let resolved = registry.resolve_effective_component(component).unwrap();
    assert_eq!(resolved.config["event_poll_interval_ms"], 5000);
    // Unset means every method upstream supports.
    assert_eq!(
        resolved.config["payment_methods"],
        json!(["bolt11", "onchain", "arkoor"])
    );
    let mut subset = component.clone();
    subset
        .config
        .insert("payment_methods".into(), json!(["arkoor", "onchain"]));
    assert_eq!(
        registry
            .resolve_effective_component(&subset)
            .unwrap()
            .config["payment_methods"],
        json!(["arkoor", "onchain"])
    );
    for (name, value) in [
        ("event_poll_interval_ms", json!(0)),
        ("event_poll_interval_ms", json!(60_001)),
        ("event_poll_interval_ms", json!("5000")),
        ("event_poll_interval_ms", json!(1.5)),
        ("mnemonic", json!("authored seed")),
        ("network", json!("mainnet")),
        ("backend_endpoint", json!("https://example.com")),
        ("payment_methods", json!("arkoor")),
        ("payment_methods", json!([])),
        ("payment_methods", json!(["bolt12"])),
        ("payment_methods", json!(["lightning"])),
        ("payment_methods", json!(["bolt11", "bolt11"])),
        ("payment_methods", json!([11])),
        ("rpc_credentials", json!("other")),
        ("data_dir", json!("/tmp")),
    ] {
        let mut invalid = component.clone();
        invalid.config.insert(name.into(), value);
        assert!(
            registry.resolve_effective_component(&invalid).is_err(),
            "{name}"
        );
    }
    for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
        let catalog = catalog_for_platform(platform);
        assert!(catalog.entries.iter().any(|e| e.id == BARK_PROCESSOR));
        resolve_lock(&cell, &catalog).unwrap();
    }
}

fn assert_security(pod: &Value) {
    assert_eq!(pod["automountServiceAccountToken"], false);
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        pod["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(
        pod["containers"][0]["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(
        pod["containers"][0]["securityContext"]["capabilities"]["drop"],
        json!(["ALL"])
    );
}

#[test]
fn renderer_uses_the_full_owned_wallet_and_narrow_credential_projections() {
    let plan = plan();
    let rendered = render_bark_processor_component(&plan).unwrap();
    let stateful = serde_json::to_value(&rendered.stateful_sets[0]).unwrap();
    assert_eq!(
        stateful["spec"]["volumeClaimTemplates"][0]["metadata"]["name"],
        "data"
    );
    let pod = pod(&plan);
    assert_eq!(pod, self::pod(&plan));
    assert_security(&pod);
    let environment = env(&pod);
    for (key, value) in [
        ("BARK_NETWORK", "regtest"),
        ("BARK_PAYMENT_METHODS", "bolt11,onchain,arkoor"),
        ("BARK_DATA_DIR", "/data"),
        ("BARK_BITCOIND_COOKIEFILE", "/chain-rpc/rpc.cookie"),
        ("TLS_ENABLE", "true"),
        ("ALLOW_INSECURE", "false"),
    ] {
        assert_eq!(environment[key], value);
    }
    assert!(!environment.contains_key("BARK_MNEMONIC"));
    assert!(!environment.contains_key("BARK_ESPLORA_ADDRESS"));
    assert_eq!(
        pod["containers"][0]["command"],
        json!(["/opt/proofstorm/driver", "exec-bark-processor"])
    );
    assert_eq!(
        pod["containers"][0]["readinessProbe"]["exec"]["command"],
        json!([
            "/opt/proofstorm/driver",
            "processor-settings",
            "https://127.0.0.1:50051",
            "/processor-client/tls",
            BARK_PROCESSOR,
            "bolt11,onchain,arkoor"
        ])
    );
    let volumes = pod["volumes"].as_array().unwrap();
    for (name, keys) in [
        ("identity", vec!["mnemonic"]),
        (
            "processor-server",
            vec!["ca.pem", "server.pem", "server.key"],
        ),
        (
            "processor-client",
            vec!["ca.pem", "client.pem", "client.key"],
        ),
    ] {
        let volume = volumes.iter().find(|v| v["name"] == name).unwrap();
        assert_eq!(volume["secret"]["defaultMode"], 288);
        let actual: Vec<_> = volume["secret"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        assert_eq!(actual, keys);
    }
    assert!(
        volumes
            .iter()
            .all(|v| v.get("persistentVolumeClaim").is_none())
    );
    let mounts = pod["containers"][0]["volumeMounts"].as_array().unwrap();
    let data = mounts.iter().find(|v| v["name"] == "data").unwrap();
    assert_eq!(data["mountPath"], "/data");
    assert!(data.get("subPath").is_none());
    for name in ["config", "identity", "processor-server", "processor-client"] {
        assert_eq!(
            mounts.iter().find(|v| v["name"] == name).unwrap()["readOnly"],
            true
        );
    }
    assert_eq!(
        rendered.config_maps[0].data.as_ref().unwrap()["rpc.cookie"],
        format!(
            "{}:{}\n",
            proofstorm_kube::BITCOIN_RPC_USER,
            proofstorm_kube::BITCOIN_RPC_PASSWORD
        )
    );
    let seed = rendered
        .secrets
        .iter()
        .find(|s| s.metadata.name.as_deref() == Some("processor-identity"))
        .unwrap();
    assert_eq!(
        seed.string_data.as_ref().unwrap(),
        &BTreeMap::from([("PROOFSTORM_SECRET_KIND".into(), "bark-processor".into())])
    );
    assert_eq!(pod["initContainers"][0]["name"], "proofstorm-driver");
    assert_eq!(
        pod["initContainers"][1]["volumeMounts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn endpoints_and_polling_come_from_the_compiled_plan() {
    let mut plan = plan();
    for (link_id, id, port) in [
        ("processor-chain", "other-chain", 28443),
        ("processor-ark", "other-ark", 4535),
    ] {
        let target = plan.linked_targets.get_mut(link_id).unwrap();
        target.component_id = id.into();
        target.ports.insert("rpc".into(), port);
        plan.relevant_links
            .iter_mut()
            .find(|l| l.id == link_id)
            .unwrap()
            .to = id.into();
    }
    let EffectiveComponentConfig::BarkProcessor(config) = &mut plan.effective_config else {
        panic!("profile")
    };
    config.event_poll_interval_ms = 1234;
    config.payment_methods = [
        PaymentMethod::Custom("arkoor".into()),
        PaymentMethod::Bolt11,
    ]
    .into();
    let pod = pod(&plan);
    let environment = env(&pod);
    assert_eq!(environment["BARK_PAYMENT_METHODS"], "bolt11,arkoor");
    assert_eq!(
        pod["containers"][0]["readinessProbe"]["exec"]["command"][5],
        "bolt11,arkoor"
    );
    assert_eq!(environment["BARK_SERVER_ADDRESS"], "http://other-ark:4535");
    assert_eq!(
        environment["BARK_BITCOIND_ADDRESS"],
        "http://other-chain:28443"
    );
    assert_eq!(environment["BARK_EVENT_POLL_INTERVAL_MS"], "1234");
    let command = pod["initContainers"][1]["command"].as_array().unwrap();
    assert_eq!(
        &command[4..],
        &[
            json!("other-chain"),
            json!("28443"),
            json!("other-ark"),
            json!("4535")
        ]
    );
}

#[test]
fn corrupted_dependencies_and_storage_are_refused_before_rendering() {
    for link_id in ["processor-chain", "processor-ark"] {
        for mutation in [
            "missing",
            "duplicate",
            "binding",
            "source",
            "target-id",
            "kind",
            "implementation",
            "port",
            "hostname",
        ] {
            let mut plan = plan();
            let index = plan
                .relevant_links
                .iter()
                .position(|l| l.id == link_id)
                .unwrap();
            match mutation {
                "missing" => {
                    plan.relevant_links.remove(index);
                }
                "duplicate" => {
                    plan.relevant_links.push(plan.relevant_links[index].clone());
                }
                "binding" => {
                    plan.relevant_links[index].binding = Some(DependencyBinding::Payment {
                        method: proofstorm_core::PaymentMethod::Bolt11,
                        unit: "sat".into(),
                    });
                }
                "source" => plan.relevant_links[index].from = "other".into(),
                "target-id" => {
                    plan.linked_targets.get_mut(link_id).unwrap().component_id = "other".into();
                }
                "kind" => {
                    plan.linked_targets.get_mut(link_id).unwrap().kind = ComponentKind::Wallet;
                }
                "implementation" => {
                    plan.linked_targets.get_mut(link_id).unwrap().backend_id = "other".into();
                }
                "port" => {
                    plan.linked_targets.get_mut(link_id).unwrap().ports.clear();
                }
                "hostname" => {
                    plan.linked_targets.get_mut(link_id).unwrap().component_id =
                        "outside.example".into();
                    plan.relevant_links[index].to = "outside.example".into();
                }
                _ => unreachable!(),
            }
            assert!(
                render_bark_processor_component(&plan).is_err(),
                "{link_id}/{mutation}"
            );
        }
    }
    for mutation in [
        "claim",
        "mount",
        "workload",
        "identity",
        "extra-port",
        "zero-port",
        "unused",
        "no-methods",
        "bolt12",
    ] {
        let mut plan = plan();
        let EffectiveComponentConfig::BarkProcessor(config) = &mut plan.effective_config else {
            panic!("profile")
        };
        match mutation {
            "no-methods" => config.payment_methods.clear(),
            "bolt12" => {
                config.payment_methods.insert(PaymentMethod::Bolt12);
            }
            "claim" => plan.storage[0].claim_name = "data-other-0".into(),
            "mount" => plan.execution_context.mounts[0].read_only = true,
            "workload" => plan.workload.kind = WorkloadControllerKind::Deployment,
            "identity" => plan.target_descriptor.component_id = "other".into(),
            "extra-port" => {
                plan.target_descriptor.ports.insert("admin".into(), 3536);
            }
            "zero-port" => {
                plan.target_descriptor.ports.insert("grpc".into(), 0);
            }
            "unused" => plan.relevant_links[0].kind = LinkKind::PaymentBackend,
            _ => unreachable!(),
        }
        assert!(
            render_bark_processor_component(&plan).is_err(),
            "{mutation}"
        );
    }
}
