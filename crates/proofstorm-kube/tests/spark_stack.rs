//! Spark renderer contracts on synthetic plans; the images are unpublished.
use proofstorm_core::{
    DependencyBinding, EffectiveComponentConfig, LinkKind, SparkOperatorsConfig,
    processor_ids::{OPEN_SSP, SPARK_OPERATORS, SPARK_PROCESSOR},
};
use proofstorm_kube::{
    RenderedComponent, render_ldk_server_component, render_open_ssp_component,
    render_spark_operators_component, render_spark_processor_component,
};
use serde_json::{Value, json};
#[path = "support/spark.rs"]
mod spark;

fn json_of<T: serde::Serialize>(resource: &T) -> Value {
    serde_json::to_value(resource).unwrap()
}

fn pod(rendered: &RenderedComponent) -> Value {
    json_of(&rendered.stateful_sets[0])["spec"]["template"]["spec"].clone()
}

fn container<'a>(pod: &'a Value, name: &str) -> &'a Value {
    pod["containers"]
        .as_array()
        .unwrap()
        .iter()
        .chain(pod["initContainers"].as_array().unwrap())
        .find(|container| container["name"] == name)
        .unwrap_or_else(|| panic!("container {name}"))
}

fn env(container: &Value, name: &str) -> Value {
    container["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("env {name}"))
        .clone()
}

fn volume<'a>(pod: &'a Value, name: &str) -> &'a Value {
    pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == name)
        .unwrap_or_else(|| panic!("volume {name}"))
}

/// The items a consumer may read from its federation's identity Secret.
fn public_projection(pod: &Value) -> (String, Vec<String>) {
    let secret = &volume(pod, "spark-operators")["secret"];
    let items = secret["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["key"].as_str().unwrap().to_owned())
        .collect();
    (secret["secretName"].as_str().unwrap().to_owned(), items)
}

#[test]
fn operators_render_one_pod_each_with_a_signer_and_their_own_databases() {
    let mut cell = spark::cell();
    cell.components
        .iter_mut()
        .find(|component| component.id == "operators")
        .unwrap()
        .config = [
        ("operators".to_owned(), json!(4)),
        ("threshold".to_owned(), json!(3)),
    ]
    .into();
    let plan = spark::plan_for(&cell, SPARK_OPERATORS);
    // Stop and start restore this many pods.
    assert_eq!(plan.workload.desired_replicas, 4);
    let mut mismatched = plan.clone();
    mismatched.effective_config = EffectiveComponentConfig::SparkOperators(SparkOperatorsConfig {
        operators: 3,
        threshold: 3,
    });
    assert!(render_spark_operators_component(&mismatched).is_err());
    let rendered = render_spark_operators_component(&plan).unwrap();
    let workload = json_of(&rendered.stateful_sets[0]);
    assert_eq!(workload["spec"]["replicas"], 4);
    assert_eq!(workload["spec"]["podManagementPolicy"], "Parallel");
    assert!(workload["spec"].get("volumeClaimTemplates").is_none());
    assert_eq!(json_of(&rendered.services[0])["spec"]["clusterIP"], "None");
    let secret = json_of(&rendered.secrets[0]);
    assert_eq!(secret["metadata"]["name"], "operators-identity");
    assert_eq!(
        secret["stringData"],
        json!({"PROOFSTORM_SECRET_KIND":"spark-operators","PROOFSTORM_SPARK_OPERATORS":"4",
            "PROOFSTORM_SPARK_THRESHOLD":"3","PROOFSTORM_SPARK_SERVICE":"operators"})
    );
    let config = json_of(&rendered.config_maps[0])["data"]["operator.config.yaml"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        config.starts_with("bitcoind:\n  regtest:\n    network: regtest\n    host: chain:18443\n")
    );
    assert!(config.contains("zmqpubrawblock: tcp://chain:28334\n"));
    assert!(config.contains("\nknobs:\n"));
    let pod = pod(&rendered);
    let component = container(&pod, "component");
    assert_eq!(
        component["command"],
        json!(["/opt/proofstorm/driver", "exec-spark-operator"])
    );
    assert_eq!(
        env(component, "PROOFSTORM_SPARK_DATABASE_NAME")["value"],
        "operators_primary"
    );
    // The owner password comes from the server's Secret, never a literal.
    assert_eq!(
        env(component, "PGPASSWORD")["valueFrom"]["secretKeyRef"]["key"],
        "POSTGRES_PASSWORD"
    );
    assert_eq!(
        container(&pod, "signer")["command"],
        json!(["/opt/proofstorm/driver", "exec-spark-signer"])
    );
    let database = container(&pod, "ensure-spark-databases");
    let command = database["command"].as_array().unwrap();
    assert_eq!(command[..2], [json!("sh"), json!("-ec")]);
    assert_eq!(
        command[3..],
        [
            json!("ensure-spark-databases"),
            json!("postgres"),
            json!("5432"),
            json!("operators_primary")
        ]
    );
    assert_eq!(
        env(database, "PROOFSTORM_POD_NAME")["valueFrom"]["fieldRef"]["fieldPath"],
        "metadata.name"
    );
    assert_eq!(volume(&pod, "identity")["secret"].get("items"), None);
}

#[test]
fn open_ssp_reads_only_public_federation_files_and_its_nodes_macaroon() {
    let plan = spark::plan(OPEN_SSP);
    let rendered = render_open_ssp_component(&plan).unwrap();
    assert_eq!(
        json_of(&rendered.secrets[0])["stringData"],
        json!({"PROOFSTORM_SECRET_KIND":"open-ssp"})
    );
    let pod = pod(&rendered);
    assert_eq!(
        public_projection(&pod),
        (
            "operators-identity".into(),
            vec!["ca.crt".into(), "operators.json".into(), "threshold".into()]
        )
    );
    assert_eq!(
        volume(&pod, "ldk-server")["persistentVolumeClaim"],
        json!({"claimName":"data-ssp-node-0","readOnly":true})
    );
    let component = container(&pod, "component");
    for (name, value) in [
        ("LDK_GRPC_ADDR", "ssp-node:3536"),
        (
            "LDK_MACAROON_FILE",
            "/ldk-server/regtest/macaroons/admin.macaroon",
        ),
        ("SSP_SWAP_FEE_SATS", "0"),
        ("MAX_SWAP_TOTAL_SATS", "1000000"),
        ("SSP_PUBLIC_URL", "http://ssp:5000"),
    ] {
        assert_eq!(env(component, name)["value"], value, "{name}");
    }
    let wait = container(&pod, "wait-for-open-ssp-dependencies");
    let command = wait["command"].as_array().unwrap();
    assert_eq!(
        command[3..],
        [json!("--"), json!("ssp-node:3536"), json!("operators:8535")]
    );
}

#[test]
fn the_processor_reaches_its_ssps_operators_without_their_keys() {
    let plan = spark::plan(SPARK_PROCESSOR);
    assert_eq!(
        plan.relevant_links
            .iter()
            .map(|link| (link.from.as_str(), link.to.as_str()))
            .collect::<Vec<_>>(),
        [("processor", "ssp"), ("ssp", "operators")]
    );
    let rendered = render_spark_processor_component(&plan).unwrap();
    let kinds = rendered
        .secrets
        .iter()
        .map(|secret| json_of(secret)["stringData"]["PROOFSTORM_SECRET_KIND"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [json!("spark-processor"), json!("payment-processor-tls")]
    );
    let pod = pod(&rendered);
    assert_eq!(public_projection(&pod).0, "operators-identity");
    let component = container(&pod, "component");
    assert_eq!(
        env(component, "PROOFSTORM_SPARK_SSP_URL")["value"],
        "http://ssp:5000"
    );
    assert_eq!(
        component["readinessProbe"]["exec"]["command"],
        json!([
            "/opt/proofstorm/driver",
            "processor-settings",
            "https://127.0.0.1:50051",
            "/processor-client/tls",
            "cdk-spark-processor"
        ])
    );
}

fn render(
    backend: &str,
    plan: &proofstorm_core::ComponentPlanContract,
) -> Result<RenderedComponent, proofstorm_kube::AdapterError> {
    match backend {
        SPARK_OPERATORS => render_spark_operators_component(plan),
        OPEN_SSP => render_open_ssp_component(plan),
        _ => render_spark_processor_component(plan),
    }
}

#[test]
fn ambiguous_or_foreign_dependencies_are_refused_before_rendering() {
    for backend in [SPARK_OPERATORS, OPEN_SSP, SPARK_PROCESSOR] {
        let plan = spark::plan(backend);
        let mut extra = plan.clone();
        let mut duplicate = extra.relevant_links[0].clone();
        duplicate.id = "duplicate".into();
        extra.relevant_links.push(duplicate);
        assert!(render(backend, &extra).is_err(), "{backend} extra link");
        let mut wrong = plan.clone();
        wrong.relevant_links[0].binding = Some(DependencyBinding::Authentication {
            protocol: proofstorm_core::AuthenticationProtocol::Oidc,
        });
        assert!(render(backend, &wrong).is_err(), "{backend} wrong binding");
        let mut foreign = plan.clone();
        foreign.relevant_links[0].kind = LinkKind::ArkBackend;
        assert!(render(backend, &foreign).is_err(), "{backend} foreign kind");
    }
    // Operators need room for `_<n>_ephemeral` in each database name.
    let mut cell = spark::cell();
    cell.links
        .iter_mut()
        .find(|link| link.id == "operators-database")
        .unwrap()
        .binding = Some(DependencyBinding::Database {
        role: proofstorm_core::DatabaseRole::Primary,
        database: Some("a".repeat(52)),
    });
    assert!(render_spark_operators_component(&spark::plan_for(&cell, SPARK_OPERATORS)).is_err());
}

#[test]
fn the_macaroon_era_ldk_server_renders_like_the_api_key_era() {
    let plan = spark::plan_for(&spark::cell(), "ldk-server");
    assert_eq!(plan.target_descriptor.backend_id, "ldk-server");
    let pod = pod(&render_ldk_server_component(&plan).unwrap());
    assert_eq!(pod["containers"][0]["command"], json!(["ldk-server"]));
}
