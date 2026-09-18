use super::*;
use proofstorm_core::workspace::evidence::CapturedFile;
use proofstorm_store::{Store, Workspace};
use std::sync::{Arc, Mutex};

struct Cluster {
    cell: ProofstormCell,
    pod: Pod,
    unavailable: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "one self-contained store and Kubernetes fixture"
)]
fn fixture() -> (
    Cells,
    WorkspaceCaptureRequest,
    TaskCapture,
    Arc<Mutex<Cluster>>,
) {
    let store = Store::memory().unwrap();
    store
        .put_workspace(&Workspace {
            id: "local".into(),
            name: "local".into(),
        })
        .unwrap();
    store.put_principal("actor").unwrap();
    for capability in [
        Capability::CellCreate,
        Capability::CellRead,
        Capability::CellPublish,
        Capability::CellMaterialize,
        Capability::CellStatus,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::ExperimentClose,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
    ] {
        store.grant("local", "actor", capability).unwrap();
    }
    let spec: proofstorm_core::CellSpec =
        serde_json::from_str(include_str!("../../../../examples/workspace/cell.json")).unwrap();
    store
        .create_draft("local", "actor", "draft", &spec, "draft")
        .unwrap();
    let revision = store
        .publish("local", "actor", "draft", 1, "publish")
        .unwrap();
    let instance = store
        .materialize(
            "local",
            "actor",
            "instance",
            &revision.digest,
            "materialize",
        )
        .unwrap();
    store
        .create_experiment("local", "actor", "run", "instance", "run")
        .unwrap();
    let mut cell = ProofstormCell::new(
        &instance.resource_name,
        proofstorm_kube::ProofstormCellSpec {
            workspace_id: "local".into(),
            instance_id: instance.id,
            instance_key: instance.instance_key.clone(),
            revision_digest: revision.digest,
            cell: spec,
            lock: revision.lock,
        },
    );
    cell.metadata.namespace = Some("system".into());
    cell.metadata.uid = Some("cell-uid".into());
    let pod: Pod=serde_json::from_value(json!({"metadata":{"name":"scripts-pod","namespace":instance_namespace(&instance.instance_key),"uid":"pod-uid"},"status":{"phase":"Running"}})).unwrap();
    let cluster = Arc::new(Mutex::new(Cluster {
        cell,
        pod,
        unavailable: false,
    }));
    let shared = cluster.clone();
    let client = kube::Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let shared = shared.clone();
            async move {
                let cluster = shared.lock().unwrap();
                let route = request.uri().path();
                let (status, body) = if cluster.unavailable {
                    (
                        503,
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"fixture offline","code":503}),
                    )
                } else if route.ends_with("/pods") {
                    (
                        200,
                        json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[cluster.pod]}),
                    )
                } else if route.ends_with("/scripts-pod") {
                    (200, json!(cluster.pod))
                } else if route.contains("/proofstormcells/") {
                    (200, json!(cluster.cell))
                } else {
                    panic!("unexpected capture API call {route}")
                };
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(kube::client::Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "system",
    );
    let service = Cells::new(
        store,
        crate::Runtime::new(client, "system".into()),
        "local".into(),
        "actor".into(),
    );
    let request = WorkspaceCaptureRequest {
        name: "instance".into(),
        component: "scripts".into(),
        run_id: "run".into(),
        request_id: "capture".into(),
        selection: CaptureSelection {
            task_id: "miner".into(),
            output_paths: vec!["result.bin".into()],
            include_logs: false,
        },
    };
    let start = serde_json::from_value(json!({"task_id":"miner","script":"sleep 60"})).unwrap();
    let snapshot = TaskCapture {
        capture_id: request.capture_id("local", "actor"),
        capture_request_digest: digest_json(&request),
        selection: request.selection.clone(),
        observed_at_unix: 1,
        task: json!({"task_id":"miner","phase":"running","request_digest":digest_json(&start),"source_digest":digest_json(&json!({}))}),
        request: start,
        files: vec![CapturedFile::from_bytes(
            "output/result.bin".into(),
            0,
            &[0, 255, 1],
        )],
    };
    (service, request, snapshot, cluster)
}

#[tokio::test]
async fn controller_capture_preserves_original_receipts_and_records_later_cleanup_or_missing_actions()
 {
    use proofstorm_kube::{
        ActionPhase, ComponentExecLiveAction, NetworkPartitionAction, ProofstormCellActionSpec,
        ProofstormCellActionStatus,
    };
    let (service, _, mut snapshot, _) = fixture();
    let (instance, _) = service
        .store
        .operation_context("local", "actor", "instance", Capability::ComponentExecLive)
        .unwrap();
    snapshot.request.control = Some(
        serde_json::from_value(
            json!({"network":[{"from_component":"chain","to_component":"scripts"}]}),
        )
        .unwrap(),
    );
    snapshot.task["request_digest"] = json!(digest_json(&snapshot.request));
    snapshot.task["control_owner"] = json!("owner");
    let call:ControlCall=serde_json::from_value(json!({"call_id":"outage","operation":{"kind":"network_partition","from_component":"chain","to_component":"scripts","duration_seconds":30}})).unwrap();
    let mut parent = ProofstormCellAction::new(
        "owner-action",
        ProofstormCellActionSpec {
            workspace_id: instance.workspace_id.clone(),
            instance_id: instance.id.clone(),
            instance_key: instance.instance_key.clone(),
            cell_name: instance.resource_name.clone(),
            experiment_id: String::new(),
            session_id: String::new(),
            principal_id: "actor".into(),
            sequence: 1,
            operation_id: "owner".into(),
            request_digest: "start-digest".into(),
            capability: Capability::ComponentExecLive,
            accepted_at_unix: 1,
            access_scope: None,
            action: CellAction::ComponentExecLive(ComponentExecLiveAction {
                component: "scripts".into(),
                script: String::new(),
                argv: vec![
                    WORKSPACE_RUNNER.into(),
                    "workspace".into(),
                    "request".into(),
                    serde_json::to_string(&WorkspaceRequest::Task(
                        proofstorm_core::workspace::TaskRequest::Start(snapshot.request.clone()),
                    ))
                    .unwrap(),
                ],
                timeout_seconds: 25,
                output: proofstorm_core::native::NativeOutput::default(),
                private_payload: None,
            }),
        },
    );
    parent.metadata.uid = Some("owner-uid".into());
    let label =
        digest_json(&(parent.metadata.uid.as_deref(), &parent.spec.operation_id))[7..47].to_owned();
    let name = format!("ws-call-{}", &digest_json(&(&label, &call.call_id))[7..47]);
    let initial = json!({"call":call,"digest":digest_json(&call),"claimed":true,"receipt":{"action_id":name,"phase":"Succeeded","artifact":{"cleanup_verified":false}}});
    snapshot.files.push(CapturedFile::from_bytes(
        "control/outage.json".into(),
        0,
        &serde_json::to_vec(&initial).unwrap(),
    ));
    let mut child = parent.clone();
    child.metadata.name = Some(name.clone());
    child.spec.operation_id = name.clone();
    child.spec.request_digest = digest_json(&call);
    child.spec.capability = Capability::NetworkPartition;
    child.spec.action = CellAction::NetworkPartition(NetworkPartitionAction {
        from_component: "chain".into(),
        to_component: "scripts".into(),
    });
    child
        .annotations_mut()
        .insert("proofstorm.dev/workspace-parent".into(), parent.name_any());
    child.status = Some(ProofstormCellActionStatus {
        phase: ActionPhase::Succeeded,
        artifact: Some(BTreeMap::from([("cleanup_verified".into(), json!(true))])),
        ..Default::default()
    });
    let mut children = BTreeMap::from([(name.clone(), child)]);
    let latest = controller_records(&instance, &snapshot, &parent, &label, &children).unwrap();
    assert_eq!(latest[1]["status"]["artifact"]["cleanup_verified"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&snapshot.files.last().unwrap().bytes().unwrap()).unwrap(),
        initial
    );
    children.get_mut(&name).unwrap().spec.instance_key = "other-cell".into();
    assert!(controller_records(&instance, &snapshot, &parent, &label, &children).is_err());
    let missing =
        controller_records(&instance, &snapshot, &parent, &label, &BTreeMap::new()).unwrap();
    assert_eq!(missing[1]["observation"], "missing");
    assert_eq!(missing[1]["outcome"], "unknown");
}

#[tokio::test]
async fn capture_attaches_before_release_and_exact_retry_works_after_run_closure_offline() {
    let (service, request, snapshot, cluster) = fixture();
    let mut downloads = 0;
    let receipt = service
        .capture_with(&request, |_, args| {
            downloads += 1;
            let bytes = if args[2] == "capture" {
                serde_json::to_vec(&snapshot).unwrap()
            } else {
                assert!(
                    service
                        .store
                        .workspace_capture(
                            "local",
                            "actor",
                            &snapshot.capture_id,
                            &digest_json(&request)
                        )
                        .unwrap()
                        .is_some()
                );
                b"{}".to_vec()
            };
            std::future::ready(Ok(bytes))
        })
        .await
        .unwrap();
    assert_eq!(downloads, 2);
    assert_eq!(receipt.task_phase, "running");
    service
        .store
        .close_experiment("local", "actor", "run", "finish")
        .unwrap();
    cluster.lock().unwrap().unavailable = true;
    let replay = service
        .capture_with(&request, |_, _| async {
            panic!("exact retry must not contact the workspace")
        })
        .await
        .unwrap();
    assert_eq!(replay.digest, receipt.digest);
    let mut changed = request.clone();
    changed.selection.include_logs = true;
    assert!(
        service
            .capture_with(&changed, |_, _| async {
                panic!("conflicting retry must not download")
            })
            .await
            .is_err()
    );
    assert_eq!(
        service
            .store
            .workspace_captures("local", "actor", "run")
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn capture_racing_run_closure_cannot_attach_or_release_the_transfer() {
    let (service, request, snapshot, _) = fixture();
    let mut downloads = 0;
    let outcome = service
        .capture_with(&request, |_, _| {
            downloads += 1;
            service
                .store
                .close_experiment("local", "actor", "run", "finish")
                .unwrap();
            std::future::ready(Ok(serde_json::to_vec(&snapshot).unwrap()))
        })
        .await;
    assert!(outcome.is_err());
    assert_eq!(downloads, 1);
    assert!(
        service
            .store
            .workspace_captures("local", "actor", "run")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn capture_rejects_replaced_pods_and_corrupt_file_bodies_without_attaching() {
    let (service, request, mut snapshot, cluster) = fixture();
    let outcome = service
        .capture_with(&request, |_, _| {
            cluster.lock().unwrap().pod.metadata.uid = Some("replacement".into());
            std::future::ready(Ok(serde_json::to_vec(&snapshot).unwrap()))
        })
        .await;
    assert!(outcome.is_err());
    snapshot.files[0].sha256 = "corrupt".into();
    assert!(
        service
            .capture_with(&request, |_, _| std::future::ready(Ok(serde_json::to_vec(
                &snapshot
            )
            .unwrap())))
            .await
            .is_err()
    );
    assert!(
        service
            .store
            .workspace_captures("local", "actor", "run")
            .unwrap()
            .is_empty()
    );
}
