use super::*;
use crate::cell::workspace_fixture;
use proofstorm_core::OperationPhase;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Cluster {
    cell: ProofstormCell,
    pod: Pod,
    actions: Vec<Value>,
}

fn fixture() -> (
    Cells,
    WorkspaceUploadRequest,
    tempfile::TempDir,
    Arc<Mutex<Cluster>>,
) {
    let store = workspace_fixture::store();
    crate::developer::configure(&store, "local", "actor").unwrap();
    let (cell, pod) = workspace_fixture::materialize(&store);
    let cluster = Arc::new(Mutex::new(Cluster {
        cell,
        pod,
        actions: vec![],
    }));
    let shared = cluster.clone();
    let client = kube::Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let shared = shared.clone();
            async move {
                let method = request.method().clone();
                let path = request.uri().path().to_owned();
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut cluster = shared.lock().unwrap();
                let (status, body) = if method == http::Method::PATCH {
                    let action: Value = serde_json::from_slice(&bytes).unwrap();
                    cluster.actions.push(action.clone());
                    (200, action)
                } else if path.ends_with("/pods") {
                    (
                        200,
                        json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[cluster.pod]}),
                    )
                } else if path.ends_with("/scripts-pod") {
                    (200, json!(cluster.pod))
                } else if path.contains("/proofstormcells/") {
                    (200, json!(cluster.cell))
                } else {
                    (
                        404,
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404}),
                    )
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
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.bin");
    fs::write(&source, [0, 255, 128, 10, 34]).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let request = WorkspaceUploadRequest {
        name: "instance".into(),
        component: "scripts".into(),
        source_path: source.to_str().unwrap().into(),
        path: "src/upload.bin".into(),
        request_id: "upload".into(),
    };
    (service, request, directory, cluster)
}

#[tokio::test]
async fn upload_stages_binary_bytes_once_records_metadata_and_rejects_changed_retries() {
    let (service, request, _directory, cluster) = fixture();
    let operation = service
        .upload_with(&request, |_, manifest, bytes| async move {
            assert_eq!(bytes, [0, 255, 128, 10, 34]);
            assert_eq!(manifest.bytes, 5);
            assert!(manifest.executable);
            assert_eq!(manifest.sha256, format!("{:x}", Sha256::digest(&bytes)));
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(operation.phase, OperationPhase::Running);
    assert_eq!(cluster.lock().unwrap().actions.len(), 1);
    let payload: Value =
        serde_json::from_str(operation.request["argv"][3].as_str().unwrap()).unwrap();
    assert_eq!(payload["kind"], "upload");
    assert!(payload["request"].get("content").is_none());
    assert!(
        !serde_json::to_string(&operation)
            .unwrap()
            .contains(&request.source_path)
    );
    let retry = service
        .upload_with(&request, |_, _, _| async {
            panic!("must not restage accepted upload")
        })
        .await
        .unwrap();
    assert_eq!(operation.id, retry.id);
    fs::write(&request.source_path, b"changed").unwrap();
    assert!(
        service
            .upload_with(&request, |_, _, _| async {
                panic!("must not stage conflicting bytes")
            })
            .await
            .is_err()
    );
    assert_eq!(cluster.lock().unwrap().actions.len(), 1);
}

#[tokio::test]
async fn interrupted_staging_and_replaced_pods_never_submit_a_commit() {
    let (service, request, _directory, cluster) = fixture();
    assert!(
        service
            .upload_with(&request, |_, _, _| async { Err(invalid("interrupted")) })
            .await
            .is_err()
    );
    assert!(cluster.lock().unwrap().actions.is_empty());
    assert_eq!(
        service
            .store
            .operation("local", "actor", "upload")
            .unwrap()
            .phase,
        OperationPhase::Pending
    );
    assert!(
        service
            .upload_with(&request, |_, _, _| {
                cluster.lock().unwrap().pod.metadata.uid = Some("replacement".into());
                std::future::ready(Ok(()))
            })
            .await
            .is_err()
    );
    assert!(cluster.lock().unwrap().actions.is_empty());
    let retried = service
        .upload_with(&request, |_, _, _| async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(retried.phase, OperationPhase::Running);
    assert_eq!(cluster.lock().unwrap().actions.len(), 1);
}

#[tokio::test]
async fn upload_refuses_unsafe_paths_wrong_components_and_missing_authority_before_transfer() {
    let (service, mut request, _directory, cluster) = fixture();
    request.path = "../escape".into();
    assert!(
        service
            .upload_with(&request, |_, _, _| async { panic!("unsafe path") })
            .await
            .is_err()
    );
    request.path = "src/file".into();
    request.component = "chain".into();
    assert!(
        service
            .upload_with(&request, |_, _, _| async { panic!("wrong component") })
            .await
            .is_err()
    );
    request.component = "scripts".into();
    service
        .store
        .revoke("local", "actor", Capability::ComponentExecLive)
        .unwrap();
    assert!(
        service
            .upload_with(&request, |_, _, _| async { panic!("denied") })
            .await
            .is_err()
    );
    assert!(cluster.lock().unwrap().actions.is_empty());
}

#[test]
fn upload_source_is_bounded_and_special_files_cannot_block_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_UPLOAD_BYTES).unwrap();
    assert_eq!(read_source(&path).unwrap().0.len() as u64, MAX_UPLOAD_BYTES);
    file.set_len(MAX_UPLOAD_BYTES + 1).unwrap();
    assert!(read_source(&path).is_err());
    assert!(read_source(directory.path()).is_err());
    assert!(read_source(&directory.path().join("missing")).is_err());
    let fifo = directory.path().join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(read_source(&fifo).is_err());
}

#[tokio::test]
async fn cancelled_or_revoked_uploads_do_not_commit_after_staging() {
    let (service, request, _directory, cluster) = fixture();
    let operation = service
        .upload_with(&request, |_, _, _| {
            service
                .store
                .record_operation_result(
                    "local",
                    "upload",
                    OperationPhase::Cancelled,
                    json!({"cancelled":true}),
                )
                .unwrap();
            std::future::ready(Ok(()))
        })
        .await
        .unwrap();
    assert_eq!(operation.phase, OperationPhase::Cancelled);
    assert!(cluster.lock().unwrap().actions.is_empty());
    for capability in [
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
        Capability::CellOperate,
    ] {
        let (service, request, _directory, cluster) = fixture();
        let result = service
            .upload_with(&request, |_, _, _| {
                service.store.revoke("local", "actor", capability).unwrap();
                std::future::ready(Ok(()))
            })
            .await;
        assert!(result.is_err(), "revoked {capability:?}");
        assert!(cluster.lock().unwrap().actions.is_empty());
    }
}
