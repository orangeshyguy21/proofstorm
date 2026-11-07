//! An action that never started must fail at its start deadline instead of
//! starting hours late (PS-44), without ever claiming "nothing ran" for work
//! that may have started.
use super::*;
use proofstorm_kube::{
    ComponentExecLiveAction, ComponentForensicsAction, NetworkPartitionAction,
    ProofstormCellActionSpec,
};
use std::sync::Mutex;

const DEADLINE: i64 = 120;

fn exec_action(accepted_at_unix: i64) -> ProofstormCellAction {
    let mut action = ProofstormCellAction::new(
        "op-stale",
        ProofstormCellActionSpec {
            access_scope: None,
            cell_name: "cell".into(),
            workspace_id: "workspace".into(),
            instance_id: "instance".into(),
            instance_key: "i0123456789012345678".into(),
            experiment_id: "experiment".into(),
            session_id: "session".into(),
            principal_id: "principal".into(),
            sequence: 1,
            operation_id: "stale-pay".into(),
            request_digest: "sha256:request".into(),
            capability: proofstorm_core::Capability::ComponentExecLive,
            accepted_at_unix,
            action: CellAction::ComponentExecLive(ComponentExecLiveAction {
                private_payload: None,
                component: "cln-hub".into(),
                script: String::new(),
                argv: vec!["lightning-cli".into(), "pay".into(), "lnbcrt1".into()],
                timeout_seconds: 30,
                output: proofstorm_core::native::NativeOutput::default(),
            }),
        },
    );
    action.metadata.namespace = Some("proofstorm-system".into());
    action.metadata.resource_version = Some("1".into());
    action
}

fn forensics_action(accepted_at_unix: i64) -> ProofstormCellAction {
    let mut action = exec_action(accepted_at_unix);
    action.spec.capability = proofstorm_core::Capability::ComponentForensics;
    action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
        component: "wallet".into(),
        target_component: "mint".into(),
        script: "cashu send 10".into(),
        timeout_seconds: 30,
    });
    action
}

fn stale(action: fn(i64) -> ProofstormCellAction) -> ProofstormCellAction {
    action(now_unix() - DEADLINE - 1)
}

#[test]
fn only_never_started_one_off_actions_expire_before_starting() {
    let accepted = 1_000;
    let deadline = accepted + DEADLINE;
    let exec = exec_action(accepted);
    assert!(!start_deadline_passed(&exec, deadline, DEADLINE));
    assert!(start_deadline_passed(&exec, deadline + 1, DEADLINE));

    // A queued action has a Pending status but no start.
    let mut pending = exec.clone();
    pending.status = Some(ProofstormCellActionStatus::default());
    assert!(start_deadline_passed(&pending, deadline + 1, DEADLINE));

    // Started work runs to its own deadline instead.
    let mut started = exec.clone();
    started.status = Some(ProofstormCellActionStatus {
        phase: ActionPhase::Running,
        started_at_unix: Some(accepted + 5),
        ..ProofstormCellActionStatus::default()
    });
    assert!(!start_deadline_passed(&started, deadline + 3600, DEADLINE));

    // An action created long after acceptance (an upload, or an idempotent retry
    // of an old request) gets its full window from creation.
    let mut retried = exec.clone();
    retried.metadata.creation_timestamp =
        Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            k8s_openapi::jiff::Timestamp::from_second(deadline + 600).unwrap(),
        ));
    assert!(!start_deadline_passed(&retried, deadline + 601, DEADLINE));
    assert!(start_deadline_passed(
        &retried,
        deadline + 601 + DEADLINE,
        DEADLINE
    ));

    // Network faults are replayed from the action list; they keep their lifecycle.
    let mut partition = exec;
    partition.spec.action = CellAction::NetworkPartition(NetworkPartitionAction {
        from_component: "a".into(),
        to_component: "b".into(),
    });
    assert!(!start_deadline_passed(
        &partition,
        deadline + 3600,
        DEADLINE
    ));
}

/// What the fake API server holds: the action as stored (with its own
/// resource version), whether its Job exists, and every request it saw.
struct Server {
    action: ProofstormCellAction,
    job_exists: bool,
    requests: Vec<String>,
    status_patches: Vec<serde_json::Value>,
}

fn not_found() -> (u16, serde_json::Value) {
    (
        404,
        serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"missing","code":404}),
    )
}

fn respond(server: &mut Server, route: &str, bytes: &[u8]) -> (u16, serde_json::Value) {
    if route.starts_with("GET ") && route.ends_with("/jobs/op-stale") {
        if !server.job_exists {
            return not_found();
        }
        return (
            200,
            serde_json::json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":"op-stale","namespace":"proofstorm-i0123456789012345678"}}),
        );
    }
    if route.starts_with("PATCH ") && route.ends_with("/op-stale/status") {
        let patch: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        server.status_patches.push(patch.clone());
        let stored = server.action.metadata.resource_version.clone();
        if let Some(fence) = patch["metadata"]["resourceVersion"].as_str()
            && Some(fence) != stored.as_deref()
        {
            return (
                409,
                serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","message":"the object has been modified","code":409}),
            );
        }
        server.action.status = Some(serde_json::from_value(patch["status"].clone()).unwrap());
        let next = stored.map_or(1, |v| v.parse::<u64>().unwrap() + 1);
        server.action.metadata.resource_version = Some(next.to_string());
        return (200, serde_json::json!(server.action));
    }
    not_found()
}

fn fake_api(server: Server) -> (kube::Client, Arc<Mutex<Server>>) {
    let server = Arc::new(Mutex::new(server));
    let shared = server.clone();
    let client = kube::Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let server = shared.clone();
            async move {
                let route = format!("{} {}", request.method(), request.uri().path());
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let (code, body) = {
                    let mut server = server.lock().unwrap();
                    server.requests.push(route.clone());
                    respond(&mut server, &route, &bytes)
                };
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(kube::client::Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "proofstorm-system",
    );
    (client, server)
}

fn server_holding(action: &ProofstormCellAction) -> Server {
    Server {
        action: action.clone(),
        job_exists: false,
        requests: Vec::new(),
        status_patches: Vec::new(),
    }
}

fn context(client: kube::Client) -> Arc<Context> {
    Arc::new(Context::for_tests(
        client.clone(),
        probes::Manager::new(client, "fixture".into()).0,
    ))
}

async fn reconcile(
    action: ProofstormCellAction,
    server: Server,
) -> (Result<Action, Error>, Arc<Mutex<Server>>) {
    let (client, server) = fake_api(server);
    let result = Box::pin(reconcile_action(Arc::new(action), context(client))).await;
    (result, server)
}

#[tokio::test]
async fn a_stale_command_fails_without_touching_its_cell_or_pod() {
    let action = stale(exec_action);
    let (result, server) = reconcile(action.clone(), server_holding(&action)).await;
    assert_eq!(result.unwrap(), Action::await_change());
    let server = server.lock().unwrap();
    // A Job check and one fenced status write; no cell read, pod lookup or upload.
    assert_eq!(server.requests.len(), 2, "{:?}", server.requests);
    assert!(
        server.requests[0].starts_with("GET ") && server.requests[0].ends_with("/jobs/op-stale")
    );
    assert_eq!(server.status_patches[0]["metadata"]["resourceVersion"], "1");
    let status = server.action.status.clone().unwrap();
    assert_eq!(status.phase, ActionPhase::Failed);
    assert_eq!(status.started_at_unix, None);
    let error = serde_json::to_value(status.error.unwrap()).unwrap();
    assert_eq!(error["code"], "action_not_started");
}

#[tokio::test]
async fn a_job_that_exists_is_observed_not_expired() {
    // The Job was created but the Running status write was lost.
    let action = stale(forensics_action);
    let mut held = server_holding(&action);
    held.job_exists = true;
    let (result, server) = reconcile(action, held).await;
    // The fixture has no cell, so the normal path stops at the cell read.
    assert!(result.is_err());
    let server = server.lock().unwrap();
    assert!(
        server.status_patches.is_empty(),
        "must not report that nothing ran"
    );
    assert!(
        server.requests[1].starts_with("GET ")
            && server.requests[1].ends_with("/proofstormcells/cell")
    );
}

#[tokio::test]
async fn a_stale_cache_cannot_overwrite_a_start_it_has_not_seen() {
    // This reconcile reads version 1 with no status, but an earlier reconcile
    // already claimed the start: the stored action is version 2 and Running.
    let cached = stale(exec_action);
    let mut stored = cached.clone();
    stored.metadata.resource_version = Some("2".into());
    stored.status = Some(ProofstormCellActionStatus {
        phase: ActionPhase::Running,
        started_at_unix: Some(now_unix() - 1),
        job_name: Some("live-exec-started".into()),
        ..ProofstormCellActionStatus::default()
    });
    let (result, server) = reconcile(cached, server_holding(&stored)).await;
    assert_eq!(result.unwrap(), Action::requeue(Duration::from_secs(1)));
    let server = server.lock().unwrap();
    assert_eq!(
        server.action.status, stored.status,
        "the running start survives"
    );
}

#[tokio::test]
async fn a_fresh_command_goes_on_to_read_its_cell() {
    let action = exec_action(now_unix());
    let (result, server) = reconcile(action.clone(), server_holding(&action)).await;
    assert!(result.is_err());
    let server = server.lock().unwrap();
    assert_eq!(server.requests.len(), 1, "{:?}", server.requests);
    assert!(
        server.requests[0].starts_with("GET ")
            && server.requests[0].ends_with("/proofstormcells/cell")
    );
    assert!(server.action.status.is_none());
}

#[tokio::test]
async fn a_cancel_still_wins_over_the_start_deadline() {
    let mut action = stale(exec_action);
    action
        .annotations_mut()
        .insert(ACTION_CANCEL_ANNOTATION.into(), "token".into());
    // Cancellation looks for a Job (absent here) and then records the cancel.
    let (result, server) = reconcile(action.clone(), server_holding(&action)).await;
    result.unwrap();
    let status = server.lock().unwrap().action.status.clone().unwrap();
    assert_eq!(status.phase, ActionPhase::Cancelled);
}
