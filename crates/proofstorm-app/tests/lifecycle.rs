use http::{Request, Response};
use kube::client::Body;
use proofstorm_app::{Runtime, lab::Labs};
use proofstorm_core::{
    Capability, ComponentKind, ComponentSpec, ControlClass, LabPolicy, LabSpec, OperationPhase,
    native::{NativeCommand, NativeOutput},
};
use proofstorm_store::{LabHandlePhase, Store, Workspace};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Cluster {
    objects: BTreeMap<String, Value>,
    requests: Vec<(String, String)>,
    fail_materialize: bool,
    fail_reads: bool,
    conflict_lease: bool,
}

fn client(cluster: Arc<Mutex<Cluster>>) -> kube::Client {
    kube::Client::new(
        tower::service_fn(move |request: Request<Body>| {
            let cluster = cluster.clone();
            async move {
                let method = request.method().to_string();
                let path = request.uri().path().to_string();
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut cluster = cluster.lock().unwrap();
                cluster.requests.push((method.clone(), path.clone()));
                let (status,body)=match method.as_str() {
                "PATCH"=> {
                    let mut value:Value=serde_json::from_slice(&bytes).unwrap();
                    if value.get("spec").is_none() && cluster.conflict_lease {
                        cluster.conflict_lease = false;
                        (409, json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","message":"controller updated metadata","code":409}))
                    } else if value.get("spec").is_some() && path.contains("/proofstormlabs/") && cluster.fail_materialize {
                        cluster.fail_materialize=false;
                        (503,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"injected interruption","code":503}))
                    } else {
                        if value.get("spec").is_none() {
                            let object=cluster.objects.get_mut(&path).unwrap();
                            for (key,v) in value["metadata"]["annotations"].as_object().unwrap() {if v.is_null() {object["metadata"]["annotations"].as_object_mut().unwrap().remove(key);} else {object["metadata"]["annotations"][key]=v.clone();}}
                            value=object.clone();
                        } else if path.contains("/proofstormlabs/") {
                            value["metadata"]["annotations"]=json!({});
                            value["status"]=json!({"phase":"Pending","observedRevisionDigest":value["spec"]["revisionDigest"],"instanceNamespace":format!("proofstorm-{}",value["spec"]["instanceKey"].as_str().unwrap()),"components":[],"inventory":[]});
                        }
                        cluster.objects.insert(path,value.clone());
                        (200,value)
                    }
                },
                "DELETE"=> {
                    let lab=cluster.objects.remove(&path).unwrap();
                    let key=lab["spec"]["instanceKey"].as_str().unwrap();
                    let name=format!("proofstorm-teardown-{key}");
                    cluster.objects.insert(format!("/api/v1/namespaces/system/configmaps/{name}"),json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name},"data":{"instanceNamespace":format!("proofstorm-{key}"),"inventoryDigest":"digest","verifiedAbsent":"true"}}));
                    (200,json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200}))
                },
                "GET" if cluster.fail_reads => (503,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"PRIVATE-RUNTIME-DETAIL","code":503})),
                "GET" if path.ends_with("/proofstormlabs")=> {
                    let items=cluster.objects.iter().filter(|(p,_)|p.contains("/proofstormlabs/")).map(|(_,v)|v.clone()).collect::<Vec<_>>();
                    (200,json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormLabList","metadata":{},"items":items}))
                },
                "GET"=>cluster.objects.get(&path).cloned().map_or((404,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"absent","code":404})),|v|(200,v)),
                _=>panic!("unexpected request {method} {path}"),
            };
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "system",
    )
}

fn spec() -> LabSpec {
    LabSpec {
        api_version: proofstorm_core::API_VERSION.into(),
        name: "demo".into(),
        components: vec![ComponentSpec {
            id: "chain".into(),
            kind: ComponentKind::Bitcoin,
            implementation: "bitcoin-core".into(),
            version: Some("30.0".into()),
            config_version: "bitcoin-core/30/v1".into(),
            control: ControlClass::Laboratory,
            config: BTreeMap::new(),
        }],
        links: vec![],
        policy: LabPolicy::default(),
    }
}

fn seed(store: &Store) {
    store
        .put_workspace(&Workspace {
            id: "local".into(),
            name: "local".into(),
        })
        .unwrap();
    store.put_principal("developer").unwrap();
    for cap in [
        Capability::CatalogRead,
        Capability::LabCreate,
        Capability::LabRead,
        Capability::LabPublish,
        Capability::LabMaterialize,
        Capability::LabStatus,
        Capability::LabClose,
        Capability::LabConnect,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::ExperimentClose,
        Capability::LabOperate,
        Capability::ExperimentRead,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
        Capability::ActionCancel,
    ] {
        store.grant("local", "developer", cap).unwrap();
    }
}
fn service(store: Store, cluster: Arc<Mutex<Cluster>>) -> Labs {
    Labs::new(
        store,
        Runtime::new(client(cluster), "system".into()),
        "local".into(),
        "developer".into(),
    )
}
fn command() -> NativeCommand {
    NativeCommand {
        private_io: None,
        script: String::new(),
        argv: vec!["bitcoin-cli".into(), "-help".into()],
        timeout_seconds: 10,
        output: NativeOutput::default(),
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end reconnection and teardown contract"
)]
async fn resume_observe_collect_close_and_reuse_name() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let first = labs.up("demo", &spec()).await.unwrap();
    let replay = labs.up("demo", &spec()).await.unwrap();
    assert_eq!(first.lab, replay.lab);
    assert_eq!(first.sessions.sessions, replay.sessions.sessions);
    // Pending aggregate readiness must not prevent dispatch to component admission.
    let op = labs
        .exec("demo", "chain", command(), "command-one")
        .await
        .unwrap();
    assert_eq!(op.phase, OperationPhase::Running);
    let action_path = cluster
        .lock()
        .unwrap()
        .objects
        .keys()
        .find(|p| p.contains("/proofstormlabactions/"))
        .unwrap()
        .clone();
    cluster
        .lock()
        .unwrap()
        .objects
        .get_mut(&action_path)
        .unwrap()["status"] =
        json!({"phase":"Succeeded","artifact":{"exit_code":0,"cleanup_verified":true}});
    let requests_before = cluster.lock().unwrap().requests.len();
    let view = labs.inspect("demo", 0).await.unwrap();
    assert_eq!(
        view.activity[0].phase,
        OperationPhase::Running,
        "inspect must not synchronize"
    );
    assert!(
        cluster.lock().unwrap().requests[requests_before..]
            .iter()
            .all(|(method, _)| method == "GET")
    );
    drop(labs);
    drop(store);
    let reopened = Store::open(&path).unwrap();
    let labs = service(reopened, cluster.clone());
    labs.sync("demo").await.unwrap();
    assert_eq!(
        labs.inspect("demo", 0).await.unwrap().activity[0].phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        labs.exec("demo", "chain", command(), "command-one")
            .await
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    let closed = labs.down("demo", 1).await.unwrap();
    assert_eq!(closed.lab.phase, LabHandlePhase::Closed);
    assert!(
        closed
            .runtime
            .unwrap()
            .teardown_receipt
            .unwrap()
            .verified_absent
    );
    assert_eq!(
        labs.down("demo", 1).await.unwrap().lab.phase,
        LabHandlePhase::Closed
    );
    let fresh = labs.up("demo", &spec()).await.unwrap();
    assert_ne!(fresh.lab.instance_id, first.lab.instance_id);
    assert_eq!(fresh.lab.generation, 2);
    assert!(fresh.activity.is_empty());
}

#[tokio::test]
async fn interrupted_up_resumes_and_finished_session_does_not_block_work() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster {
        fail_materialize: true,
        ..Default::default()
    }));
    let labs = service(store, cluster);
    let error = labs.up("demo", &spec()).await.unwrap_err();
    assert_eq!(error.details.unwrap()["stage"], "published");
    let ready = labs.up("demo", &spec()).await.unwrap();
    assert_eq!(ready.lab.generation, 1);
    let mut changed = spec();
    changed.name = "different".into();
    assert!(labs.up("demo", &changed).await.is_err());
    labs.store
        .finish_session(
            "local",
            "developer",
            &ready.sessions.sessions[0].id,
            "manual-release",
        )
        .unwrap();
    let operation = labs
        .exec("demo", "chain", command(), "command-one")
        .await
        .unwrap();
    assert_ne!(operation.session_id, ready.sessions.sessions[0].id);
    assert_eq!(
        labs.store
            .session("local", "developer", &ready.sessions.sessions[0].id)
            .unwrap()
            .phase,
        proofstorm_core::SessionPhase::Finished
    );
}

#[tokio::test]
async fn shutdown_latch_prevents_an_action_from_racing_finalization() {
    let store = Store::memory().unwrap();
    seed(&store);
    let labs = service(store, Arc::new(Mutex::new(Cluster::default())));
    let view = labs.up("demo", &spec()).await.unwrap();
    labs.store
        .set_lab_phase("local", "developer", &view.lab, LabHandlePhase::Closing)
        .unwrap();
    let result = labs.store.create_operation(
        "local",
        "developer",
        &view.lab.instance_id,
        &view.lab.run_id(),
        "",
        "race",
        proofstorm_core::OperationKind::ComponentExecLive,
        &json!({"component":"chain"}),
        "race",
        Capability::ComponentExecLive,
    );
    assert!(result.unwrap_err().to_string().contains("closing"));
    assert!(labs.inspect("demo", 0).await.unwrap().activity.is_empty());
}

#[tokio::test]
async fn external_configuration_is_private_and_status_has_no_credentials() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster.clone());
    let view = labs.up("demo", &spec()).await.unwrap();
    let instance = view.runtime.unwrap().instance;
    let ns = proofstorm_kube::instance_namespace(&instance.instance_key);
    {
        let mut api = cluster.lock().unwrap();
        api.objects.insert(format!("/api/v1/namespaces/{ns}/services/chain"),json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"chain","labels":{"proofstorm.dev/instance":instance.instance_key}},"spec":{"ports":[{"port":18443}]}}));
        api.objects.insert(format!("/api/v1/namespaces/{ns}/pods"),json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"chain-0"},"status":{"conditions":[{"type":"Ready","status":"True"}]}}]}));
    }
    let connection = labs.connect("demo", "chain", "rpc", 0).await.unwrap();
    let text = serde_json::to_string(&connection.descriptor).unwrap();
    assert!(text.contains("127.0.0.1"));
    assert!(text.contains("bypasses_lab_network_policies"));
    assert!(!text.contains(proofstorm_kube::BITCOIN_RPC_PASSWORD));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("app.json");
    connection.write_config(&file).unwrap();
    let config: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(config["password"], proofstorm_kube::BITCOIN_RPC_PASSWORD);
    assert!(
        connection.write_config(&file).is_err(),
        "do not overwrite existing configuration"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(labs.connect("demo", "chain", "p2p", 0).await.is_err());
    labs.store
        .revoke("local", "developer", Capability::LabConnect)
        .unwrap();
    assert!(labs.connect("demo", "chain", "rpc", 0).await.is_err());
}

#[tokio::test]
async fn partial_startup_can_be_inspected_and_closed_without_reprovisioning() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster {
        fail_materialize: true,
        ..Default::default()
    }));
    let labs = service(store, cluster.clone());
    assert!(labs.up("interrupted", &spec()).await.is_err());
    assert!(
        labs.inspect("interrupted", 0)
            .await
            .unwrap()
            .runtime
            .is_none()
    );
    let closed = labs.down("interrupted", 2).await.unwrap();
    assert_eq!(closed.lab.phase, LabHandlePhase::Closed);
    assert!(
        closed
            .runtime
            .unwrap()
            .teardown_receipt
            .unwrap()
            .verified_absent
    );
    assert!(cluster.lock().unwrap().objects.is_empty());
    assert_eq!(
        labs.down("interrupted", 2).await.unwrap().lab.phase,
        LabHandlePhase::Closed
    );
}

#[tokio::test]
async fn missing_runtime_with_a_remaining_namespace_does_not_claim_cleanup() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster {
        fail_materialize: true,
        ..Default::default()
    }));
    let labs = service(store, cluster.clone());
    assert!(labs.up("interrupted", &spec()).await.is_err());
    let handle = labs
        .store
        .lab_handle("local", "developer", "interrupted")
        .unwrap();
    let instance = labs
        .store
        .instance("local", "developer", &handle.instance_id)
        .unwrap();
    let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
    cluster.lock().unwrap().objects.insert(
        format!("/api/v1/namespaces/{namespace}"),
        json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace}}),
    );
    assert!(
        labs.down("interrupted", 2)
            .await
            .unwrap_err()
            .message
            .contains("still exists")
    );
    assert_eq!(
        labs.inspect("interrupted", 0).await.unwrap().lab.phase,
        LabHandlePhase::Closing
    );
}

#[tokio::test]
async fn ordinary_sessions_require_no_runtime_authority_annotation() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster {
        conflict_lease: true,
        ..Default::default()
    }));
    let labs = service(store, cluster.clone());
    let lab = labs.up("demo", &spec()).await.unwrap();
    assert_eq!(lab.lab.generation, 1);
    assert_eq!(lab.sessions.sessions.len(), 1);
    labs.exec("demo", "chain", command(), "first")
        .await
        .unwrap();
    labs.exec("demo", "chain", command(), "second")
        .await
        .unwrap();
    assert!(
        cluster.lock().unwrap().conflict_lease,
        "no session metadata patch was attempted"
    );
}

#[tokio::test]
async fn two_principals_share_a_named_lab_with_independent_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    store.put_principal("teammate").unwrap();
    for capability in store.capabilities("local", "developer").unwrap() {
        store.grant("local", "teammate", capability).unwrap();
    }
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let first = service(store, cluster.clone());
    let second = Labs::new(
        Store::open(&path).unwrap(),
        Runtime::new(client(cluster.clone()), "system".into()),
        "local".into(),
        "teammate".into(),
    );
    let a = first.up("demo", &spec()).await.unwrap();
    let b = second.up("demo", &spec()).await.unwrap();
    assert_eq!(a.lab.instance_id, b.lab.instance_id);
    let op_a = first
        .exec("demo", "chain", command(), "alice-work")
        .await
        .unwrap();
    let op_b = second
        .exec("demo", "chain", command(), "bob-work")
        .await
        .unwrap();
    assert_ne!(op_a.session_id, op_b.session_id);
    assert_eq!(op_a.principal_id, "developer");
    assert_eq!(op_b.principal_id, "teammate");
    assert_eq!(
        second
            .inspect("demo", 0)
            .await
            .unwrap()
            .sessions
            .sessions
            .len(),
        2
    );
}

#[path = "environment/mod.rs"]
mod environment_tests;
