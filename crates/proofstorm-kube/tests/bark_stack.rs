//! Bark renderer contracts and ordinary catalog-to-cell rendering.
use proofstorm_core::{
    ComponentPlanContract, DatabaseRole, DependencyBinding, EffectiveComponentConfig,
};
use proofstorm_kube::{RenderedComponent, render_bark_server_component, render_cln_hold_component};
use serde_json::{Value, json};
#[path = "support/bark.rs"]
mod bark;

#[test]
fn both_catalogs_render_the_complete_bark_stack_with_exact_images() {
    use proofstorm_core::{CatalogPlatform, CellSpec, catalog_for_platform, resolve_lock};

    let spec: CellSpec = serde_json::from_str(include_str!(
        "../../proofstorm-core/tests/fixtures/bark-topology.json"
    ))
    .unwrap();
    for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
        let catalog = catalog_for_platform(platform);
        let lock = resolve_lock(&spec, &catalog).unwrap();
        let rendered =
            proofstorm_kube::render_cell("bark-qualified", "sha256:test", &spec, &lock).unwrap();
        for (id, backend) in [
            ("ark", "bark-server"),
            ("cln", "cln-hold"),
            ("processor", "cdk-bark-processor"),
        ] {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == backend)
                .unwrap();
            let workload = rendered
                .stateful_sets
                .iter()
                .find(|workload| workload.metadata.name.as_deref() == Some(id))
                .unwrap();
            let pod = workload
                .spec
                .as_ref()
                .unwrap()
                .template
                .spec
                .as_ref()
                .unwrap();
            assert_eq!(pod.containers[0].image.as_ref(), Some(&entry.image));
            assert_eq!(
                workload
                    .spec
                    .as_ref()
                    .unwrap()
                    .volume_claim_templates
                    .as_ref()
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                !rendered
                    .deployments
                    .iter()
                    .any(|workload| workload.metadata.name.as_deref() == Some(id))
            );
        }
    }
}

fn pod(rendered: &RenderedComponent) -> Value {
    serde_json::to_value(&rendered.stateful_sets[0]).unwrap()["spec"]["template"]["spec"].clone()
}
fn env<'a>(pod: &'a Value, key: &str) -> &'a Value {
    pod["containers"][0]["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == key)
        .unwrap()
}

#[test]
fn bark_server_scopes_credentials_and_never_exposes_privileged_rpc() {
    let plan = bark::stack_plan("bark-server");
    let rendered = render_bark_server_component(&plan).unwrap();
    assert!(rendered.secrets.is_empty());
    assert!(rendered.deployments.is_empty());
    assert_eq!(plan.storage[0].claim_name, "data-ark-0");
    let service = serde_json::to_value(&rendered.services[0]).unwrap();
    assert_eq!(
        service["spec"]["ports"],
        json!([{"name":"rpc","port":3535,"targetPort":3535}])
    );
    let pod = pod(&rendered);
    assert_eq!(env(&pod, "BARK_SERVER__DATA_DIR")["value"], "/data/native");
    assert_eq!(
        env(&pod, "BARK_SERVER__RPC__ADMIN_ADDRESS")["value"],
        "127.0.0.1:3536"
    );
    assert_eq!(
        env(&pod, "BARK_SERVER__RPC__INTEGRATION_ADDRESS")["value"],
        "127.0.0.1:3537"
    );
    assert_eq!(
        env(&pod, "BARK_SERVER__POSTGRES__PASSWORD")["valueFrom"]["secretKeyRef"]["name"],
        "postgres-credentials"
    );
    for role in ["cln", "hold"] {
        let volume = pod["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == format!("{role}-tls"))
            .unwrap();
        assert_eq!(volume["secret"]["secretName"], format!("cln-{role}-tls"));
        let keys: Vec<_> = volume["secret"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["ca.pem", "client.pem", "client.key"]);
    }
    assert!(!pod.to_string().contains("persistentVolumeClaim"));
    let init: Vec<_> = pod["initContainers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        init,
        [
            "proofstorm-driver",
            "wait-for-bitcoin",
            "check-bark-database",
            "initialize-bark-server",
            "seal-bark-database"
        ]
    );
    assert_eq!(pod["containers"][0]["command"][1], "exec-bark-server");
    assert_eq!(pod["automountServiceAccountToken"], false);
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
}

#[test]
fn cln_hold_owns_full_native_state_and_separate_ca_keys() {
    let plan = bark::stack_plan("cln-hold");
    let rendered = render_cln_hold_component(&plan).unwrap();
    assert_eq!(plan.storage[0].claim_name, "data-cln-0");
    assert_eq!(rendered.secrets.len(), 2);
    assert_eq!(
        rendered.secrets[0].string_data.as_ref().unwrap()["PROOFSTORM_SECRET_KIND"],
        "bark-cln-tls"
    );
    assert_eq!(
        rendered.secrets[1].string_data.as_ref().unwrap()["PROOFSTORM_SECRET_KIND"],
        "bark-hold-tls"
    );
    let native = &rendered.config_maps[0].data.as_ref().unwrap()["lightning.conf"];
    assert!(native.contains("plugin=/usr/local/bin/hold\n"));
    assert!(native.contains("hold-database=sqlite:///data/regtest/hold/hold.sqlite3\n"));
    assert!(native.contains("bitcoin-rpcconnect=chain\n"));
    let pod = pod(&rendered);
    for role in ["cln", "hold"] {
        let volume = pod["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == format!("{role}-tls"))
            .unwrap();
        assert!(
            volume["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["key"] == "ca.key" && v["path"] == "ca-key.pem")
        );
    }
    assert_eq!(pod["containers"][0]["command"][1], "exec-cln-hold");
    assert_eq!(
        pod["containers"][0]["readinessProbe"]["exec"]["command"][1],
        "cln-hold-ready"
    );
    assert!(
        pod["initContainers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "wait-for-bitcoin")
    );
}

#[test]
fn descriptors_and_bindings_are_checked_again_at_rendering() {
    for backend in ["bark-server", "cln-hold"] {
        let original = bark::stack_plan(backend);
        let render: fn(
            &ComponentPlanContract,
        ) -> Result<RenderedComponent, proofstorm_kube::AdapterError> = if backend == "bark-server"
        {
            render_bark_server_component
        } else {
            render_cln_hold_component
        };
        for mutate in [
            |p: &mut ComponentPlanContract| p.target_descriptor.component_id = "other".into(),
            |p: &mut ComponentPlanContract| p.storage[0].claim_name = "foreign".into(),
            |p: &mut ComponentPlanContract| p.execution_context.mounts[0].read_only = true,
            |p: &mut ComponentPlanContract| {
                p.target_descriptor.ports.insert("admin".into(), 3536);
            },
            |p: &mut ComponentPlanContract| p.relevant_links.push(p.relevant_links[0].clone()),
            |p: &mut ComponentPlanContract| p.relevant_links[0].to = "other".into(),
            |p: &mut ComponentPlanContract| p.relevant_links[0].binding = None,
            |p: &mut ComponentPlanContract| {
                p.effective_config = EffectiveComponentConfig::NutshellWallet;
            },
        ] {
            let mut invalid = original.clone();
            mutate(&mut invalid);
            assert!(render(&invalid).is_err());
        }
    }
    let mut plan = bark::stack_plan("bark-server");
    plan.relevant_links
        .iter_mut()
        .find(|l| l.id == "ark-database")
        .unwrap()
        .binding = Some(DependencyBinding::Database {
        role: DatabaseRole::Primary,
        database: Some("quote'; DROP TABLE wallet".into()),
    });
    assert!(render_bark_server_component(&plan).is_err());
    // Keep this module's shared processor fixture compiled too.
    assert_eq!(bark::plan().backend_id, "cdk-bark-processor");
}

#[test]
fn peer_and_network_links_do_not_become_backend_requirements() {
    use proofstorm_core::{ComponentKind, LinkKind, LinkSpec, TargetDescriptorContract};
    for (backend, kind) in [
        ("cln-hold", LinkKind::LightningPeer),
        ("bark-server", LinkKind::NetworkPath),
    ] {
        let mut plan = bark::stack_plan(backend);
        let render = if backend == "cln-hold" {
            render_cln_hold_component
        } else {
            render_bark_server_component
        };
        let before = render(&plan).unwrap();
        plan.relevant_links.push(LinkSpec {
            id: "network-peer".into(),
            from: plan.component_id.clone(),
            to: "peer".into(),
            kind,
            binding: None,
        });
        plan.linked_targets.insert(
            "network-peer".into(),
            TargetDescriptorContract {
                component_id: "peer".into(),
                kind: ComponentKind::Lightning,
                backend_id: "cln".into(),
                version: "26.06.7".into(),
                ports: std::collections::BTreeMap::from([("p2p".into(), 9735)]),
            },
        );
        let after = render(&plan).unwrap();
        assert_eq!(after.stateful_sets, before.stateful_sets);
        assert_eq!(after.config_maps, before.config_maps);
        assert_eq!(after.secrets, before.secrets);
        assert_eq!(after.services, before.services);
        plan.relevant_links.last_mut().unwrap().kind = LinkKind::AuthenticationBackend;
        assert!(render(&plan).is_err());
    }
}
