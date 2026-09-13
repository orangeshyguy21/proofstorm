use http::{Request, Response};
use kube::client::Body;
use proofstorm_app::{Runtime, cell::Cells};
use proofstorm_core::{
    Capability, CellPolicy, CellSpec, ComponentKind, ComponentSpec, ControlClass, OperationPhase,
    native::{NativeCommand, NativeOutput},
};
use proofstorm_store::{CellHandlePhase, Store, Workspace};
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
    update_conflicts: usize,
    after_update_conflict: Option<Box<dyn FnOnce() + Send>>,
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
                "PATCH" | "POST" | "PUT" => {
                    let mut path=path.clone();
                    let mut value:Value=serde_json::from_slice(&bytes).unwrap();
                    if method == "POST" {path=format!("{}/{}",path,value["metadata"]["name"].as_str().unwrap());}
                    if method == "PUT" && path.contains("/proofstormcells/") && cluster.update_conflicts > 0 {
                        cluster.update_conflicts -= 1;
                        let revision = cluster.update_conflicts + 2;
                        cluster.objects.get_mut(&path).unwrap()["metadata"]["resourceVersion"] = json!(revision.to_string());
                        if let Some(hook) = cluster.after_update_conflict.take() { hook(); }
                        (409,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","message":"controller status changed resourceVersion","code":409}))
                    } else if value.get("spec").is_none() && value.get("data").is_none() && cluster.conflict_lease {
                        cluster.conflict_lease = false;
                        (409, json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","message":"controller updated metadata","code":409}))
                    } else if value.get("spec").is_some() && path.contains("/proofstormcells/") && cluster.fail_materialize {
                        cluster.fail_materialize=false;
                        (503,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"injected interruption","code":503}))
                    } else {
                        if value.get("spec").is_none() && value.get("data").is_none() {
                            let object=cluster.objects.get_mut(&path).unwrap();
                            for (key,v) in value["metadata"]["annotations"].as_object().unwrap() {if v.is_null() {object["metadata"]["annotations"].as_object_mut().unwrap().remove(key);} else {object["metadata"]["annotations"][key]=v.clone();}}
                            value=object.clone();
                        } else if path.contains("/proofstormcells/") {
                            if method == "PUT" {
                                assert_eq!(value["metadata"]["resourceVersion"], cluster.objects[&path]["metadata"]["resourceVersion"], "retry must reread resourceVersion");
                            }
                            if value["metadata"].get("annotations").is_none() {value["metadata"]["annotations"]=json!({});}
                            if value["metadata"].get("resourceVersion").is_none() {value["metadata"]["resourceVersion"]=json!("1");}
                            value["metadata"]["uid"]=json!(format!("uid-{}",value["metadata"]["name"].as_str().unwrap()));
                            value["status"]=json!({"phase":"Pending","observedRevisionDigest":value["spec"]["revisionDigest"],"instanceNamespace":format!("proofstorm-{}",value["spec"]["instanceKey"].as_str().unwrap()),"components":[],"inventory":[]});
                        }
                        if value["metadata"].get("uid").is_none() { value["metadata"]["uid"]=json!(format!("uid-{}",value["metadata"]["name"].as_str().unwrap())); }
                        cluster.objects.insert(path,value.clone());
                        (200,value)
                    }
                },
                "DELETE" if path.contains("/configmaps/")=> {
                    cluster.objects.remove(&path);
                    (200,json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200}))
                },
                "DELETE"=> {
                    let cell=cluster.objects.remove(&path).unwrap();
                    let key=cell["spec"]["instanceKey"].as_str().unwrap();
                    let name=format!("proofstorm-teardown-{key}");
                    cluster.objects.insert(format!("/api/v1/namespaces/system/configmaps/{name}"),json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"uid":format!("uid-{name}")},"data":{"instanceNamespace":format!("proofstorm-{key}"),"inventoryDigest":"digest","verifiedAbsent":"true"}}));
                    (200,json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200}))
                },
                "GET" if cluster.fail_reads => (503,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"PRIVATE-RUNTIME-DETAIL","code":503})),
                "GET" if path=="/api/v1/namespaces/kube-system" => (200,cluster.objects.get(&path).cloned().unwrap_or(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"test-cluster"}}))),
                "GET" if path.ends_with("/configmaps") => (200,json!({"apiVersion":"v1","kind":"ConfigMapList","metadata":{},"items":cluster.objects.iter().filter(|(p,_)|p.contains("/configmaps/")).map(|(_,v)|v.clone()).collect::<Vec<_>>()})),
                "GET" if path.ends_with("/proofstormcellactions") => (200,json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormCellActionList","metadata":{},"items":[]})),
                "GET" if path.ends_with("/proofstormcells")=> {
                    let items=cluster.objects.iter().filter(|(p,_)|p.contains("/proofstormcells/")).map(|(_,v)|v.clone()).collect::<Vec<_>>();
                    (200,json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormCellList","metadata":{},"items":items}))
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

fn spec() -> CellSpec {
    CellSpec {
        api_version: proofstorm_core::API_VERSION.into(),
        name: "demo".into(),
        components: vec![ComponentSpec {
            id: "chain".into(),
            kind: ComponentKind::Bitcoin,
            implementation: "bitcoin-core".into(),
            version: Some("31.1".into()),
            config_version: "bitcoin-core/31/v1".into(),
            control: ControlClass::Cell,
            config: BTreeMap::new(),
        }],
        links: vec![],
        policy: CellPolicy::default(),
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
        Capability::CellCreate,
        Capability::CellEdit,
        Capability::CellRead,
        Capability::CellPublish,
        Capability::CellMaterialize,
        Capability::CellStatus,
        Capability::CellClose,
        Capability::CellConnect,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::ExperimentClose,
        Capability::CellOperate,
        Capability::ExperimentRead,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
        Capability::ActionCancel,
    ] {
        store.grant("local", "developer", cap).unwrap();
    }
}
fn service(store: Store, cluster: Arc<Mutex<Cluster>>) -> Cells {
    Cells::new(
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
    let cells = service(store.clone(), cluster.clone());
    let first = cells.up("demo", &spec()).await.unwrap();
    let replay = cells.up("demo", &spec()).await.unwrap();
    assert_eq!(first.cell, replay.cell);
    assert_eq!(first.sessions.sessions, replay.sessions.sessions);
    // Pending aggregate readiness must not prevent dispatch to component admission.
    let op = cells
        .exec("demo", "chain", command(), "command-one")
        .await
        .unwrap();
    assert_eq!(op.phase, OperationPhase::Running);
    let action_path = cluster
        .lock()
        .unwrap()
        .objects
        .keys()
        .find(|p| p.contains("/proofstormcellactions/"))
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
    let view = cells.inspect("demo", 0).await.unwrap();
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
    drop(cells);
    drop(store);
    let reopened = Store::open(&path).unwrap();
    let cells = service(reopened, cluster.clone());
    cells.sync("demo").await.unwrap();
    assert_eq!(
        cells.inspect("demo", 0).await.unwrap().activity[0].phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        cells
            .exec("demo", "chain", command(), "command-one")
            .await
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    let progress = Mutex::new(Vec::new());
    let closed = cells
        .down_with_progress("demo", 1, &|label| {
            progress.lock().unwrap().push(label.to_owned());
        })
        .await
        .unwrap();
    assert_eq!(
        *progress.lock().unwrap(),
        [
            "Checking cell identity before cleanup",
            "Closing sessions and stopping cell actions",
            "Requesting workload and storage cleanup",
            "Waiting for workloads and storage to disappear",
            "Cell cleanup verified",
        ]
    );
    assert_eq!(closed.cell.phase, CellHandlePhase::Closed);
    assert!(
        closed
            .runtime
            .unwrap()
            .teardown_receipt
            .unwrap()
            .verified_absent
    );
    assert!(
        cells
            .store
            .cell_handle("local", "developer", "demo")
            .is_err()
    );
    let fresh = cells.up("demo", &spec()).await.unwrap();
    assert_ne!(fresh.cell.instance_id, first.cell.instance_id);
    assert_eq!(fresh.cell.generation, 1);
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
    let cells = service(store, cluster);
    let error = cells.up("demo", &spec()).await.unwrap_err();
    assert_eq!(error.details.unwrap()["stage"], "published");
    let ready = cells.up("demo", &spec()).await.unwrap();
    assert_eq!(ready.cell.generation, 1);
    let mut changed = spec();
    changed.name = "different".into();
    let edited = cells.up("demo", &changed).await.unwrap();
    assert_eq!(edited.runtime.as_ref().unwrap().instance.generation, 2);
    assert_eq!(edited.cell.instance_id, ready.cell.instance_id);
    cells
        .store
        .finish_session(
            "local",
            "developer",
            &ready.sessions.sessions[0].id,
            "manual-release",
        )
        .unwrap();
    let operation = cells
        .exec("demo", "chain", command(), "command-one")
        .await
        .unwrap();
    assert_ne!(operation.session_id, ready.sessions.sessions[0].id);
    assert_eq!(
        cells
            .store
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
    let cells = service(store, Arc::new(Mutex::new(Cluster::default())));
    let view = cells.up("demo", &spec()).await.unwrap();
    cells
        .store
        .set_cell_phase("local", "developer", &view.cell, CellHandlePhase::Closing)
        .unwrap();
    let result = cells.store.create_operation(
        "local",
        "developer",
        &view.cell.instance_id,
        &view.run.as_ref().unwrap().id,
        "",
        "race",
        proofstorm_core::OperationKind::ComponentExecLive,
        &json!({"component":"chain"}),
        "race",
        Capability::ComponentExecLive,
    );
    assert!(result.unwrap_err().to_string().contains("closing"));
    assert!(cells.inspect("demo", 0).await.unwrap().activity.is_empty());
}

#[tokio::test]
async fn external_configuration_is_private_and_status_has_no_credentials() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    let view = cells.up("demo", &spec()).await.unwrap();
    let instance = view.runtime.unwrap().instance;
    let ns = proofstorm_kube::instance_namespace(&instance.instance_key);
    {
        let mut api = cluster.lock().unwrap();
        api.objects.insert(format!("/api/v1/namespaces/{ns}/services/chain"),json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"chain","labels":{"proofstorm.dev/instance":instance.instance_key}},"spec":{"ports":[{"port":18443}]}}));
        api.objects.insert(format!("/api/v1/namespaces/{ns}/pods"),json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"chain-0"},"status":{"conditions":[{"type":"Ready","status":"True"}]}}]}));
    }
    let connection = cells.connect("demo", "chain", "rpc", 0).await.unwrap();
    let text = serde_json::to_string(&connection.descriptor).unwrap();
    assert!(text.contains("127.0.0.1"));
    assert!(text.contains("bypasses_cell_network_policies"));
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
    assert!(cells.connect("demo", "chain", "p2p", 0).await.is_err());
    cells
        .store
        .revoke("local", "developer", Capability::CellConnect)
        .unwrap();
    assert!(cells.connect("demo", "chain", "rpc", 0).await.is_err());
}

#[tokio::test]
async fn partial_startup_can_be_inspected_and_closed_without_reprovisioning() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster {
        fail_materialize: true,
        ..Default::default()
    }));
    let cells = service(store, cluster.clone());
    assert!(cells.up("interrupted", &spec()).await.is_err());
    assert!(
        cells
            .inspect("interrupted", 0)
            .await
            .unwrap()
            .runtime
            .is_none()
    );
    assert!(
        cells
            .inspect("interrupted", 0)
            .await
            .unwrap()
            .instance_key
            .is_some(),
        "the close fence remains available even before a runtime resource exists"
    );
    let closed = cells.down("interrupted", 2).await.unwrap();
    assert_eq!(closed.cell.phase, CellHandlePhase::Closed);
    assert!(
        closed
            .runtime
            .unwrap()
            .teardown_receipt
            .unwrap()
            .verified_absent
    );
    assert!(cluster.lock().unwrap().objects.is_empty());
    assert!(
        cells
            .store
            .cell_handle("local", "developer", "interrupted")
            .is_err()
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
    let cells = service(store, cluster.clone());
    assert!(cells.up("interrupted", &spec()).await.is_err());
    let handle = cells
        .store
        .cell_handle("local", "developer", "interrupted")
        .unwrap();
    let instance = cells
        .store
        .instance("local", "developer", &handle.instance_id)
        .unwrap();
    let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
    cluster.lock().unwrap().objects.insert(
        format!("/api/v1/namespaces/{namespace}"),
        json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace}}),
    );
    assert!(
        cells
            .down("interrupted", 2)
            .await
            .unwrap_err()
            .message
            .contains("still exists")
    );
    assert_eq!(
        cells.inspect("interrupted", 0).await.unwrap().cell.phase,
        CellHandlePhase::Closing
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
    let cells = service(store, cluster.clone());
    let cell = cells.up("demo", &spec()).await.unwrap();
    assert_eq!(cell.cell.generation, 1);
    assert_eq!(cell.sessions.sessions.len(), 1);
    cells
        .exec("demo", "chain", command(), "first")
        .await
        .unwrap();
    cells
        .exec("demo", "chain", command(), "second")
        .await
        .unwrap();
    assert!(
        cluster.lock().unwrap().conflict_lease,
        "no session metadata patch was attempted"
    );
}

#[tokio::test]
async fn two_principals_share_a_named_cell_with_independent_sessions() {
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
    let second = Cells::new(
        Store::open(&path).unwrap(),
        Runtime::new(client(cluster.clone()), "system".into()),
        "local".into(),
        "teammate".into(),
    );
    let a = first.up("demo", &spec()).await.unwrap();
    let b = second.up("demo", &spec()).await.unwrap();
    assert_eq!(a.cell.instance_id, b.cell.instance_id);
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

#[tokio::test]
async fn retained_data_edits_cannot_converge_using_the_previous_configuration_status() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    let handle = cells.up("generation-check", &spec()).await.unwrap().cell;
    environment_tests::ready(&cluster);
    let mut instance = cells
        .store
        .instance("local", "developer", &handle.instance_id)
        .unwrap();
    instance.generation = 2;
    let value = cluster
        .lock()
        .unwrap()
        .objects
        .iter()
        .find(|(path, _)| path.contains("/proofstormcells/"))
        .unwrap()
        .1
        .clone();
    let mut resource: proofstorm_kube::ProofstormCell = serde_json::from_value(value).unwrap();
    resource
        .status
        .as_mut()
        .unwrap()
        .observed_desired_generation = 1;
    let pending = proofstorm_app::runtime::status_from_resource(instance.clone(), &resource);
    assert_eq!(pending.phase, proofstorm_core::InstancePhase::Pending);
    assert_eq!(pending.observed_generation, 1);
    resource
        .status
        .as_mut()
        .unwrap()
        .observed_desired_generation = 2;
    assert_eq!(
        proofstorm_app::runtime::status_from_resource(instance, &resource).phase,
        proofstorm_core::InstancePhase::Ready
    );
}

#[tokio::test]
async fn edit_conflict_retry_rereads_the_latest_durable_generation() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    cells.up("retry-edit", &spec()).await.unwrap();
    let mut expanded = spec();
    let mut second = expanded.components[0].clone();
    second.id = "second".into();
    expanded.components.push(second);
    let first_plan = cells
        .plan_edit("retry-edit", &expanded, false, &[])
        .unwrap();
    store
        .accept_update("local", "developer", &first_plan, "first-edit")
        .unwrap();
    let mut third = expanded.components[0].clone();
    third.id = "third".into();
    expanded.components.push(third);
    let next_plan = cells
        .plan_edit("retry-edit", &expanded, false, &[])
        .unwrap();
    let next_revision = next_plan.target_revision.clone();
    let newer_store = store.clone();
    {
        let mut cluster = cluster.lock().unwrap();
        cluster.update_conflicts = 1;
        cluster.after_update_conflict = Some(Box::new(move || {
            newer_store
                .accept_update("local", "developer", &next_plan, "newer-edit")
                .unwrap();
        }));
    }
    let status = proofstorm_app::updates::reconcile(
        &cells.runtime,
        &store,
        "local",
        "developer",
        &first_plan.target.instance_id,
    )
    .await
    .unwrap();
    assert_eq!(status.instance.generation, 3);
    let cluster = cluster.lock().unwrap();
    let cell = cluster
        .objects
        .values()
        .find(|value| value["kind"] == "ProofstormCell")
        .unwrap();
    assert_eq!(cell["spec"]["revisionDigest"], next_revision);
    assert_eq!(
        cell["spec"]["cell"]["components"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        cluster
            .requests
            .iter()
            .filter(|(method, path)| method == "PUT" && path.contains("/proofstormcells/"))
            .count(),
        2
    );
}

#[tokio::test]
async fn edit_conflict_retry_respects_a_durable_close() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    cells.up("retry-close", &spec()).await.unwrap();
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    let plan = cells
        .plan_edit("retry-close", &changed, false, &[])
        .unwrap();
    store
        .accept_update("local", "developer", &plan, "edit-before-close")
        .unwrap();
    let closing_store = store.clone();
    let closing_instance = plan.target.instance_id.clone();
    {
        let mut cluster = cluster.lock().unwrap();
        cluster.update_conflicts = 1;
        cluster.after_update_conflict = Some(Box::new(move || {
            closing_store
                .begin_instance_close("local", "developer", &closing_instance)
                .unwrap();
        }));
    }
    proofstorm_app::updates::reconcile(
        &cells.runtime,
        &store,
        "local",
        "developer",
        &plan.target.instance_id,
    )
    .await
    .unwrap();
    let cluster = cluster.lock().unwrap();
    assert_eq!(
        cluster
            .requests
            .iter()
            .filter(|(method, path)| method == "PUT" && path.contains("/proofstormcells/"))
            .count(),
        1
    );
    assert!(
        cluster
            .objects
            .values()
            .filter(|value| value["kind"] == "ProofstormCell")
            .all(|value| value["spec"]["revisionDigest"] != plan.target_revision)
    );
}

#[tokio::test]
async fn edit_conflict_retry_is_bounded_and_keeps_the_accepted_update() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    cells.up("retry-bound", &spec()).await.unwrap();
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    let plan = cells
        .plan_edit("retry-bound", &changed, false, &[])
        .unwrap();
    store
        .accept_update("local", "developer", &plan, "accepted-edit")
        .unwrap();
    cluster.lock().unwrap().update_conflicts = 10;
    let error = proofstorm_app::updates::reconcile(
        &cells.runtime,
        &store,
        "local",
        "developer",
        &plan.target.instance_id,
    )
    .await
    .unwrap_err();
    assert_eq!(error.details.unwrap()["http_status"], 409);
    assert_eq!(
        store
            .instance("local", "developer", &plan.target.instance_id)
            .unwrap()
            .generation,
        2
    );
    assert_eq!(
        cluster
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(method, path)| method == "PUT" && path.contains("/proofstormcells/"))
            .count(),
        4
    );
}

#[tokio::test]
async fn accepted_edits_resume_from_the_journal_after_the_control_client_restarts() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let handle = cells.up("resume-edit", &spec()).await.unwrap().cell;
    let mut expanded = spec();
    let mut second = expanded.components[0].clone();
    second.id = "second-chain".into();
    expanded.components.push(second);
    let plan = cells
        .plan_edit("resume-edit", &expanded, false, &[])
        .unwrap();
    let accepted = store
        .accept_update("local", "developer", &plan, "accept-without-apply")
        .unwrap();
    assert_eq!(accepted.generation, 2);
    assert!(
        cluster
            .lock()
            .unwrap()
            .objects
            .values()
            .filter(|v| v["kind"] == "ProofstormCell")
            .all(|v| v["spec"]["revisionDigest"] != plan.target_revision)
    );
    let restarted = service(store.clone(), cluster.clone());
    proofstorm_app::updates::reconcile(
        &restarted.runtime,
        &store,
        "local",
        "developer",
        &handle.instance_id,
    )
    .await
    .unwrap();
    let desired = cluster
        .lock()
        .unwrap()
        .objects
        .values()
        .find(|v| v["kind"] == "ProofstormCell")
        .unwrap()
        .clone();
    assert_eq!(desired["spec"]["revisionDigest"], plan.target_revision);
    assert_eq!(
        desired["metadata"]["annotations"]["proofstorm.dev/desired-generation"],
        "2"
    );
    assert_eq!(
        store.pending_updates("local", "developer").unwrap(),
        vec![handle.instance_id]
    );
}

#[tokio::test]
async fn external_deletion_reclaims_name_and_purges_agent_history() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let first = cells.up("reusable", &spec()).await.unwrap();
    let old = first.runtime.unwrap().instance;
    cluster
        .lock()
        .unwrap()
        .objects
        .retain(|path, _| !path.contains("/proofstormcells/"));
    let second = cells.up("reusable", &spec()).await.unwrap();
    assert_ne!(
        old.instance_key,
        second.runtime.unwrap().instance.instance_key
    );
    assert!(store.instance("local", "developer", &old.id).is_err());
    assert!(
        store
            .experiment("local", "developer", &first.run.as_ref().unwrap().id)
            .is_err()
    );
}

#[tokio::test]
async fn cleanup_retains_records_on_failed_reads_and_remaining_namespace() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let old = cells
        .up("keep", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    cluster.lock().unwrap().fail_reads = true;
    assert!(
        proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
            .await
            .is_err()
    );
    assert!(store.instance("local", "developer", &old.id).is_ok());
    {
        let mut c = cluster.lock().unwrap();
        c.fail_reads = false;
        c.objects
            .retain(|path, _| !path.contains("/proofstormcells/"));
        let ns = proofstorm_kube::instance_namespace(&old.instance_key);
        c.objects.insert(format!("/api/v1/namespaces/{ns}"),json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":ns,"uid":"remaining","deletionTimestamp":"2026-09-07T00:00:00Z"}}));
    }
    assert!(
        proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
            .await
            .is_err()
    );
    assert!(store.instance("local", "developer", &old.id).is_ok());
    cluster
        .lock()
        .unwrap()
        .objects
        .retain(|path, _| !path.starts_with("/api/v1/namespaces/proofstorm-"));
    proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
        .await
        .unwrap();
    assert!(store.instance("local", "developer", &old.id).is_err());
}

#[tokio::test]
async fn rebuilt_cluster_reclaims_old_incarnation_but_switching_context_does_not() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let old = cells
        .up("rebuild", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    cluster.lock().unwrap().objects.clear();
    let mut elsewhere = cells.runtime.clone();
    elsewhere.cluster_source = "other-context".into();
    proofstorm_app::lifecycle::sweep(&elsewhere, &store, "local", "developer", "")
        .await
        .unwrap();
    assert!(store.instance("local", "developer", &old.id).is_ok());
    cluster.lock().unwrap().objects.insert("/api/v1/namespaces/kube-system".into(),json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"replacement-cluster"}}));
    proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
        .await
        .unwrap();
    assert!(store.instance("local", "developer", &old.id).is_err());
}

#[tokio::test]
async fn concurrent_same_name_creation_converges_to_one_incarnation() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let first = service(store.clone(), cluster.clone());
    let second = first.clone();
    let spec = spec();
    let (a, b) = tokio::join!(
        first.up("concurrent", &spec),
        second.up("concurrent", &spec)
    );
    assert_eq!(
        a.unwrap().runtime.unwrap().instance,
        b.unwrap().runtime.unwrap().instance
    );
    assert_eq!(
        cluster
            .lock()
            .unwrap()
            .objects
            .keys()
            .filter(|p| p.contains("/proofstormcells/"))
            .count(),
        1
    );
}

#[tokio::test]
async fn one_pending_namespace_does_not_starve_cleanup_of_other_cells() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let blocked = cells
        .up("blocked-cleanup", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    let gone = cells
        .up("gone", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    {
        let mut c = cluster.lock().unwrap();
        c.objects
            .retain(|path, _| !path.contains("/proofstormcells/"));
        let namespace = proofstorm_kube::instance_namespace(&blocked.instance_key);
        c.objects.insert(format!("/api/v1/namespaces/{namespace}"),json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace,"uid":"still-deleting"}}));
    }
    assert!(
        proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
            .await
            .is_err()
    );
    assert!(store.instance("local", "developer", &blocked.id).is_ok());
    assert!(store.instance("local", "developer", &gone.id).is_err());
}

#[tokio::test]
async fn named_cell_commands_do_not_require_experiment_creation_authority() {
    let store = Store::memory().unwrap();
    seed(&store);
    store
        .revoke("local", "developer", Capability::ExperimentCreate)
        .unwrap();
    let cells = service(store, Arc::new(Mutex::new(Cluster::default())));
    let view = cells.up("automatic", &spec()).await.unwrap();
    assert!(view.run.is_some());
    assert_eq!(view.run.unwrap().owner_principal_id, "developer");
}

#[tokio::test]
async fn inspecting_unmaterialized_intent_does_not_require_or_create_a_run() {
    let store = Store::memory().unwrap();
    seed(&store);
    store
        .reserve_cell("local", "developer", "reserved", "pending-config")
        .unwrap();
    let cells = service(store, Arc::new(Mutex::new(Cluster::default())));
    let view = cells.inspect("reserved", 0).await.unwrap();
    assert!(view.runtime.is_none());
    assert!(view.run.is_none());
    assert!(view.activity.is_empty());
    assert!(cells.sync("reserved").await.unwrap().is_empty());
}

#[path = "shared_lifecycle/mod.rs"]
mod shared_lifecycle;
