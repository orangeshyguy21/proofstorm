//! Exercise real MCP handlers and the CLI application service against one cluster/store.
use super::*;
use std::sync::{Arc, Mutex};

fn cluster_client() -> Client {
    let objects = Arc::new(Mutex::new(BTreeMap::<String, serde_json::Value>::new()));
    Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let objects = objects.clone();
            async move {
                let method = request.method().clone();
                let mut path = request.uri().path().to_string();
                let body = request.into_body().collect_bytes().await.unwrap();
                let mut objects = objects.lock().unwrap();
                let missing = serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"absent","code":404});
                let (code, value) = match method {
                    http::Method::GET if path == "/api/v1/namespaces/kube-system" => (
                        200,
                        serde_json::json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"shared-cluster"}}),
                    ),
                    http::Method::GET if path.ends_with("/configmaps") => (
                        200,
                        serde_json::json!({"apiVersion":"v1","kind":"ConfigMapList","metadata":{},"items":objects.iter().filter(|(p,_)|p.contains("/configmaps/")).map(|(_,v)|v).collect::<Vec<_>>()}),
                    ),
                    http::Method::GET if path.ends_with("/proofstormlabactions") => (
                        200,
                        serde_json::json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormLabActionList","metadata":{},"items":[]}),
                    ),
                    http::Method::GET => objects
                        .get(&path)
                        .cloned()
                        .map_or((404, missing), |value| (200, value)),
                    http::Method::PATCH | http::Method::PUT | http::Method::POST => {
                        let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        if method == http::Method::POST {
                            path =
                                format!("{}/{}", path, value["metadata"]["name"].as_str().unwrap());
                        }
                        value["metadata"]["uid"] = serde_json::json!(format!(
                            "uid-{}",
                            value["metadata"]["name"].as_str().unwrap()
                        ));
                        value["metadata"]["resourceVersion"] = serde_json::json!("1");
                        if value["kind"] == "ProofstormLab" {
                            value["status"] = serde_json::json!({"phase":"Pending","observedRevisionDigest":value["spec"]["revisionDigest"],"instanceNamespace":format!("proofstorm-{}",value["spec"]["instanceKey"].as_str().unwrap()),"components":[],"inventory":[]});
                        }
                        objects.insert(path, value.clone());
                        (200, value)
                    }
                    http::Method::DELETE => {
                        let value = objects.remove(&path).unwrap();
                        if value["kind"] == "ProofstormLab" {
                            let key = value["spec"]["instanceKey"].as_str().unwrap();
                            let name = format!("proofstorm-teardown-{key}");
                            objects.insert(format!("/api/v1/namespaces/system/configmaps/{name}"), serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"uid":format!("uid-{name}")},"data":{"instanceNamespace":format!("proofstorm-{key}"),"verifiedAbsent":"true","inventoryDigest":"digest"}}));
                        }
                        (
                            200,
                            serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200}),
                        )
                    }
                    _ => panic!("unexpected request {method} {path}"),
                };
                Ok::<_, std::io::Error>(
                    http::Response::builder()
                        .status(code)
                        .body(kube::client::Body::from(
                            serde_json::to_vec(&value).unwrap(),
                        ))
                        .unwrap(),
                )
            }
        }),
        "system",
    )
}

#[tokio::test]
async fn mcp_creation_and_cli_lifecycle_share_identity_and_teardown() {
    let store = tests::seeded_store();
    for cap in [Capability::ExperimentRead, Capability::LabOperate] {
        store.grant("alpha", "designer", cap).unwrap();
    }
    let mcp = ProofstormMcp::new(store.clone(), "alpha", "designer")
        .unwrap()
        .with_kubernetes(cluster_client(), "system");
    let cli = mcp.labs().unwrap();
    let plan = mcp
        .proofstorm_lab_plan(Parameters(
            serde_json::from_value(serde_json::json!({
                "plan_id":"transport-plan","idempotency_key":"transport-plan",
                "components":[{"id":"chain","implementation":"bitcoin-core"}],
                "connections":[],"runtime_requirements":[]
            }))
            .unwrap(),
        ))
        .unwrap()
        .0;
    let applied = mcp
        .proofstorm_lab_apply(Parameters(LabApplyRequest {
            instance_id: "transport-lab".into(),
            plan_id: plan.plan_id,
            expected_plan_digest: plan.plan_digest,
            idempotency_key: "transport-apply".into(),
        }))
        .await
        .unwrap()
        .0;
    let view = cli.inspect("transport-lab", 0).await.unwrap();
    assert_eq!(view.lab.instance_id, applied.instance_id);
    assert!(
        store
            .lab_handle("alpha", "designer", "transport-lab")
            .is_err()
    );
    let read = mcp
        .proofstorm_lab_read(Parameters(ReadDraftRequest {
            instance_id: Some("transport-lab".into()),
            draft_id: String::new(),
        }))
        .unwrap()
        .0;
    let closed = cli.down("transport-lab", 2).await.unwrap();
    let key = closed.runtime.unwrap().instance.instance_key;
    assert!(
        mcp.proofstorm_lab_wait(Parameters(LabWaitRequest {
            instance_id: "transport-lab".into(),
            expected_instance_key: Some(key),
            expected_generation: None,
            target_phase: InstancePhase::Closed,
            timeout_seconds: 2
        }))
        .await
        .unwrap()
        .0
        .reached
    );

    let named = cli
        .up("cli-name", &read.lab)
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    let status = mcp
        .proofstorm_lab_status(Parameters(InstanceRequest {
            instance_id: "cli-name".into(),
        }))
        .await
        .unwrap()
        .0;
    assert_eq!(status.instance_id, named.id);
    assert_eq!(status.instance_key, named.instance_key);
    let closing = mcp
        .proofstorm_lab_close(Parameters(CloseLabRequest {
            instance_id: "cli-name".into(),
            expected_instance_key: named.instance_key.clone(),
        }))
        .await
        .unwrap()
        .0;
    assert_eq!(closing.phase, InstancePhase::Closing);
    mcp.proofstorm_lab_finish(Parameters(DeveloperFinishRequest {
        name: "cli-name".into(),
        expected_instance_key: named.instance_key,
        timeout_seconds: 2,
    }))
    .await
    .unwrap();
    assert!(store.lab_handle("alpha", "designer", "cli-name").is_err());
}
