use super::*;
use http::{Request, Response};
use kube::client::Body;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Mutex};

struct Cluster {
    build: ProofstormCandidateBuild,
    events: Vec<String>,
    fail_status: bool,
    fail_delete: bool,
    job: bool,
}

fn fixture(cancel: bool) -> (Arc<Mutex<Cluster>>, Arc<Context>) {
    let mut build: ProofstormCandidateBuild = serde_json::from_value(json!({
        "apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormCandidateBuild",
        "metadata":{"name":"candidate-test","namespace":"system","uid":"uid-1"},
        "spec":{"workspaceId":"workspace","candidateId":"test","principalId":"agent","implementation":"cdk","baseVersion":"0.18.1",
            "pullRequestUrl":"https://github.com/cashubtc/cdk/pull/123","repository":"https://github.com/cashubtc/cdk.git","commitSha":"a".repeat(40),
            "version":"candidate-test","requestDigest":"digest","acceptedAtUnix":1,"imageRepository":"registry:5000/candidates/cdk","dockerfile":"Dockerfile"},
        "status":{"phase":"building","startedAtUnix":2}
    })).unwrap();
    if cancel {
        build
            .annotations_mut()
            .insert(CANDIDATE_CANCEL_ANNOTATION.into(), "cancel".into());
    }
    let cluster = Arc::new(Mutex::new(Cluster {
        build,
        events: vec![],
        fail_status: false,
        fail_delete: false,
        job: true,
    }));
    let shared = cluster.clone();
    let client = Client::new(
        tower::service_fn(move |request: Request<Body>| {
            let shared = shared.clone();
            async move {
                let route = request.uri().path().to_owned();
                let query = request.uri().query().unwrap_or_default().to_owned();
                let method = request.method().clone();
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut cluster = shared.lock().unwrap();
                let mut status = 200;
                let body = if route.ends_with("/log") {
                    cluster.events.push(format!("log:{query}"));
                    // Exceeds the requested limit to exercise our own UTF-8 bound.
                    format!(
                        "{}FINAL BUILD FAILURE 🦀\n",
                        "download progress 🦀\n".repeat(3000)
                    )
                    .into_bytes()
                } else {
                    let value = if route.ends_with("/status") {
                        let patch: Value = serde_json::from_slice(&bytes).unwrap();
                        if std::mem::take(&mut cluster.fail_status) {
                            status = 503;
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"retry status","code":503})
                        } else {
                            cluster.events.push(format!(
                                "status:{}",
                                patch["status"]["phase"].as_str().unwrap()
                            ));
                            cluster.build.status =
                                Some(serde_json::from_value(patch["status"].clone()).unwrap());
                            json!(cluster.build)
                        }
                    } else if route.ends_with("/pods") {
                        json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"builder","namespace":"system"},"status":{"phase":"Failed"}}]})
                    } else if method == http::Method::DELETE {
                        cluster.events.push("delete".into());
                        if std::mem::take(&mut cluster.fail_delete) {
                            status = 503;
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"retry delete","code":503})
                        } else {
                            cluster.job = false;
                            json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200})
                        }
                    } else if method == http::Method::PATCH {
                        let patch: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(patch["spec"]["ttlSecondsAfterFinished"], 600);
                        assert!(cluster.build.status.as_ref().unwrap().diagnostics.is_some());
                        cluster.events.push("ttl".into());
                        json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":"candidate-test-build"}})
                    } else if !cluster.job {
                        status = 404;
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"absent","code":404})
                    } else {
                        json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":"candidate-test-build"},"status":{"failed":1,"conditions":[{"type":"Failed","status":"True","reason":"DeadlineExceeded","message":"deadline reached"}]}})
                    };
                    serde_json::to_vec(&value).unwrap()
                };
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(status)
                        .body(Body::from(body))
                        .unwrap(),
                )
            }
        }),
        "system",
    );
    let context = Arc::new(Context {
        probes: probes::Manager::new(client.clone(), "candidate-fixture".into()).0,
        client,
    });
    (cluster, context)
}

#[tokio::test]
async fn candidate_failure_archives_bounded_diagnostics_before_cleanup_and_retries_failed_status() {
    let (cluster, context) = fixture(false);
    cluster.lock().unwrap().fail_status = true;
    let build = Arc::new(cluster.lock().unwrap().build.clone());
    assert!(
        reconcile_candidate_build(build.clone(), context.clone())
            .await
            .is_err()
    );
    assert!(!cluster.lock().unwrap().events.contains(&"ttl".into()));
    reconcile_candidate_build(build, context.clone())
        .await
        .unwrap();
    let terminal = cluster.lock().unwrap().build.clone();
    let status = terminal.status.as_ref().unwrap();
    assert_eq!(status.phase, CandidateBuildPhase::Failed);
    for log in status.diagnostics.as_ref().unwrap().logs.values() {
        assert!(log.truncated);
        assert!(log.text.len() <= 32_768);
        assert!(log.text.ends_with("FINAL BUILD FAILURE 🦀\n"));
    }
    let events = cluster.lock().unwrap().events.clone();
    for container in ["source", "buildkit"] {
        assert!(events.iter().any(|event| {
            event.starts_with("log:")
                && event.contains(&format!("container={container}"))
                && event.contains("tailLines=128")
                && event.contains("limitBytes=32769")
        }));
    }
    assert!(
        events.iter().position(|e| e == "status:failed").unwrap()
            < events.iter().position(|e| e == "ttl").unwrap()
    );
    cluster.lock().unwrap().job = false;
    reconcile_candidate_build(Arc::new(terminal.clone()), context)
        .await
        .unwrap();
    assert_eq!(cluster.lock().unwrap().build.status, terminal.status);
}

#[tokio::test]
async fn candidate_cancellation_preserves_archive_across_delete_failure_and_controller_restart() {
    let (cluster, context) = fixture(true);
    cluster.lock().unwrap().fail_delete = true;
    let build = Arc::new(cluster.lock().unwrap().build.clone());
    assert!(
        reconcile_candidate_build(build, context.clone())
            .await
            .is_err()
    );
    let restart = cluster.lock().unwrap().build.clone();
    assert_eq!(
        restart.status.as_ref().unwrap().phase,
        CandidateBuildPhase::Building
    );
    let saved = restart.status.as_ref().unwrap().diagnostics.clone();
    let logs_before = cluster
        .lock()
        .unwrap()
        .events
        .iter()
        .filter(|e| e.starts_with("log:"))
        .count();
    reconcile_candidate_build(Arc::new(restart), context)
        .await
        .unwrap();
    let cluster = cluster.lock().unwrap();
    assert!(!cluster.job);
    let status = cluster.build.status.as_ref().unwrap();
    assert_eq!(status.phase, CandidateBuildPhase::Cancelled);
    assert_eq!(status.diagnostics, saved);
    assert_eq!(
        cluster
            .events
            .iter()
            .filter(|e| e.starts_with("log:"))
            .count(),
        logs_before
    );
}
