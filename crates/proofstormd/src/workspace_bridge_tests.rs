use super::*;
use proofstorm_core::workspace::{TaskRequest, WorkspaceRequest};
use proofstorm_kube::{
    ActionPhase, ProofstormCellActionSpec, ProofstormCellActionStatus, ProofstormCellSpec,
};
use std::sync::{Arc, Mutex};

struct Cluster {
    cell: ProofstormCell,
    child: Option<ProofstormCellAction>,
    creates: usize,
    cancellations: usize,
}

struct Mailbox {
    poll: Value,
    receipt: Option<Value>,
    lose_claim_reply: bool,
    unavailable: bool,
    closed: bool,
}

impl Mailbox {
    fn send(&mut self, request: BridgeRequest) -> Result<Value, Error> {
        if self.unavailable {
            return Err(Error::LiveExec("fixture transport unavailable".into()));
        }
        match request {
            BridgeRequest::Poll { .. } => Ok(self.poll.clone()),
            BridgeRequest::Claim { .. } => {
                assert_eq!(self.poll["pending"]["claimed"], false);
                self.poll["pending"]["claimed"] = json!(true);
                if self.lose_claim_reply {
                    return Err(Error::LiveExec("fixture lost claim response".into()));
                }
                Ok(json!({"fresh":true,"digest":self.poll["pending"]["digest"]}))
            }
            BridgeRequest::Complete { receipt, .. } => {
                self.receipt = Some(receipt);
                self.poll["pending"] = Value::Null;
                Ok(json!({"recorded":true}))
            }
            BridgeRequest::Close { .. } => {
                self.closed = true;
                Ok(json!({"closed":true}))
            }
            BridgeRequest::Cleanup { pending_faults, .. } => {
                self.poll["state"]["control_cleanup"] = json!({"pending_faults":pending_faults,"task_phase":self.poll["state"]["phase"]});
                Ok(json!({"recorded":true}))
            }
            _ => panic!("unexpected fixture request"),
        }
    }
}

fn client(cluster: Arc<Mutex<Cluster>>) -> kube::Client {
    kube::Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let cluster = cluster.clone();
            async move {
                let path = request.uri().path().to_owned();
                let method = request.method().clone();
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut cluster = cluster.lock().unwrap();
                let mut status = 200;
                let body = if path.ends_with("/proofstormcells/cell") {
                    json!(cluster.cell)
                } else if path.ends_with("/proofstormcellactions") && method == http::Method::GET {
                    json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormCellActionList","metadata":{},"items":cluster.child.iter().collect::<Vec<_>>()})
                } else if path.ends_with("/proofstormcellactions") && method == http::Method::POST {
                    assert!(cluster.child.is_none(), "child must never be recreated");
                    cluster.creates += 1;
                    cluster.child = Some(serde_json::from_slice(&bytes).unwrap());
                    json!(cluster.child)
                } else if method == http::Method::PATCH {
                    let patch: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(
                        patch["metadata"]["annotations"][ACTION_CANCEL_ANNOTATION],
                        "workspace-task-ended"
                    );
                    cluster.cancellations += 1;
                    let child = cluster.child.as_mut().unwrap();
                    child.annotations_mut().insert(
                        ACTION_CANCEL_ANNOTATION.into(),
                        "workspace-task-ended".into(),
                    );
                    json!(child)
                } else if let Some(child) = &cluster.child {
                    assert!(path.ends_with(&child.name_any()));
                    json!(child)
                } else {
                    status = 404;
                    json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"missing","code":404})
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
    )
}

fn fixture() -> (ProofstormCellAction, Arc<Mutex<Cluster>>, Context, Mailbox) {
    let spec: proofstorm_core::CellSpec =
        serde_json::from_str(include_str!("../../../examples/workspace/cell.json")).unwrap();
    let lock = proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).unwrap();
    let cell = ProofstormCell::new(
        "cell",
        ProofstormCellSpec {
            workspace_id: "local".into(),
            instance_id: "instance".into(),
            instance_key: "i0123456789012345678".into(),
            revision_digest: "revision".into(),
            lock,
            cell: spec,
        },
    );
    let start: TaskStart = serde_json::from_value(
        json!({"task_id":"miner","script":"sleep 60","control":{"components":["chain"]}}),
    )
    .unwrap();
    let request = WorkspaceRequest::Task(TaskRequest::Start(start.clone()));
    let mut parent = ProofstormCellAction::new(
        "start-miner",
        ProofstormCellActionSpec {
            access_scope: None,
            cell_name: "cell".into(),
            workspace_id: cell.spec.workspace_id.clone(),
            instance_id: cell.spec.instance_id.clone(),
            instance_key: cell.spec.instance_key.clone(),
            experiment_id: "finite-run".into(),
            session_id: "session".into(),
            principal_id: "initiator".into(),
            sequence: 1,
            operation_id: "start-operation".into(),
            request_digest: proofstorm_core::digest_json(&request),
            capability: Capability::ComponentExecLive,
            accepted_at_unix: 1,
            action: CellAction::ComponentExecLive(ComponentExecLiveAction {
                component: "scripts".into(),
                argv: vec![
                    WORKSPACE_RUNNER.into(),
                    "workspace".into(),
                    "request".into(),
                    serde_json::to_string(&request).unwrap(),
                ],
                script: String::new(),
                timeout_seconds: 25,
                output: proofstorm_core::native::NativeOutput::default(),
                private_payload: None,
            }),
        },
    );
    parent.metadata.namespace = Some("system".into());
    parent.metadata.uid = Some("start-action-uid".into());
    parent
        .annotations_mut()
        .insert("proofstorm.dev/action-revision".into(), "revision".into());
    let call: ControlCall = serde_json::from_value(json!({"call_id":"mine-001","component":"chain","command":{"argv":["bitcoin-cli","-regtest","getblockcount"],"timeout_seconds":10,"output":{"mode":"public"}}})).unwrap();
    let mailbox = Mailbox {
        poll: json!({"state":{"phase":"running","control_owner":parent.spec.operation_id,"request_digest":proofstorm_core::digest_json(&start)},"call_count":1,"pending":{"call":call,"digest":proofstorm_core::digest_json(&call),"claimed":false}}),
        receipt: None,
        lose_claim_reply: false,
        unavailable: false,
        closed: false,
    };
    let cluster = Arc::new(Mutex::new(Cluster {
        cell,
        child: None,
        creates: 0,
        cancellations: 0,
    }));
    let client = client(cluster.clone());
    let context = Context {
        probes: crate::probes::Manager::new(client.clone(), "fixture".into()).0,
        client,
    };
    (parent, cluster, context, mailbox)
}

async fn tick(
    parent: &ProofstormCellAction,
    context: &Context,
    mailbox: &mut Mailbox,
) -> Result<Action, Error> {
    reconcile_with(parent, context, |request| {
        std::future::ready(mailbox.send(request))
    })
    .await
}

#[tokio::test]
async fn calls_preserve_authority_and_receipts_without_replaying_after_controller_reconnect() {
    let (parent, cluster, context, mut mailbox) = fixture();
    tick(&parent, &context, &mut mailbox).await.unwrap();
    tick(&parent, &context, &mut mailbox).await.unwrap();
    {
        let mut cluster = cluster.lock().unwrap();
        assert_eq!(cluster.creates, 1);
        let child = cluster.child.as_mut().unwrap();
        assert_eq!(child.spec.principal_id, "initiator");
        assert_eq!(child.spec.instance_key, parent.spec.instance_key);
        assert_eq!(
            child.annotations()["proofstorm.dev/action-revision"],
            "revision"
        );
        assert!(child.spec.experiment_id.is_empty());
        assert!(!is_controlled_start(child));
        child.status = Some(ProofstormCellActionStatus {
            phase: ActionPhase::Succeeded,
            artifact: Some(super::super::status_object(
                json!({"exit_code":0,"stdout":"101"}),
            )),
            ..Default::default()
        });
    }
    tick(&parent, &context, &mut mailbox).await.unwrap();
    assert_eq!(mailbox.receipt.unwrap()["artifact"]["stdout"], "101");
    assert_eq!(cluster.lock().unwrap().creates, 1);
}

#[tokio::test]
async fn lost_dispatch_acknowledgement_and_deleted_child_never_create_again() {
    let (parent, cluster, context, mut mailbox) = fixture();
    mailbox.lose_claim_reply = true;
    assert!(tick(&parent, &context, &mut mailbox).await.is_err());
    tick(&parent, &context, &mut mailbox).await.unwrap();
    assert_eq!(cluster.lock().unwrap().creates, 0);
    assert_eq!(
        mailbox.receipt.unwrap()["error"]["code"],
        "dispatch_outcome_unknown"
    );
    let (parent, cluster, context, mut mailbox) = fixture();
    tick(&parent, &context, &mut mailbox).await.unwrap();
    cluster.lock().unwrap().child = None;
    tick(&parent, &context, &mut mailbox).await.unwrap();
    assert_eq!(cluster.lock().unwrap().creates, 1);
    assert_eq!(
        mailbox.receipt.unwrap()["error"]["code"],
        "dispatch_outcome_unknown"
    );
}

#[tokio::test]
async fn stop_crash_or_transport_loss_cancels_owned_commands_and_preserves_cleanup_receipt() {
    for phase in ["stopping", "interrupted", "unreachable"] {
        let (parent, cluster, context, mut mailbox) = fixture();
        tick(&parent, &context, &mut mailbox).await.unwrap();
        mailbox.poll["state"]["phase"] = json!(phase);
        mailbox.unavailable = phase == "unreachable";
        tick(&parent, &context, &mut mailbox).await.unwrap();
        assert_eq!(cluster.lock().unwrap().cancellations, 1);
        cluster.lock().unwrap().child.as_mut().unwrap().status = Some(ProofstormCellActionStatus {
            phase: ActionPhase::Cancelled,
            artifact: Some(super::super::status_object(
                json!({"cleanup_verified":true,"cancelled":true}),
            )),
            ..Default::default()
        });
        mailbox.unavailable = false;
        tick(&parent, &context, &mut mailbox).await.unwrap();
        let receipt = mailbox.receipt.unwrap();
        assert_eq!(receipt["phase"], "Cancelled");
        assert_eq!(receipt["artifact"]["cleanup_verified"], true);
    }
}

#[tokio::test]
async fn changed_revision_duplicate_owner_and_out_of_scope_calls_cannot_start_commands() {
    for reason in ["revision", "removed-target", "replaced-workspace"] {
        let (parent, cluster, context, mut mailbox) = fixture();
        if reason == "replaced-workspace" {
            mailbox.poll["workspace_replaced"] = json!(true);
        } else {
            cluster.lock().unwrap().cell.spec.revision_digest = "changed".into();
            if reason == "removed-target" {
                cluster
                    .lock()
                    .unwrap()
                    .cell
                    .spec
                    .cell
                    .components
                    .retain(|component| component.id != "chain");
            }
        }
        tick(&parent, &context, &mut mailbox).await.unwrap();
        assert!(mailbox.closed);
        assert_eq!(cluster.lock().unwrap().creates, 0);
    }
    let (parent, cluster, context, mut mailbox) = fixture();
    mailbox.poll["state"]["control_owner"] = json!("original-start");
    tick(&parent, &context, &mut mailbox).await.unwrap();
    assert_eq!(cluster.lock().unwrap().creates, 0);
    let (parent, cluster, context, mut mailbox) = fixture();
    mailbox.poll["pending"]["call"]["component"] = json!("scripts");
    assert!(tick(&parent, &context, &mut mailbox).await.is_err());
    assert_eq!(cluster.lock().unwrap().creates, 0);
    let mut other_cell = cluster.lock().unwrap().cell.clone();
    other_cell.spec.instance_id = "recreated".into();
    assert!(controlled_start(&parent, &other_cell).is_none());
}

#[tokio::test]
async fn typed_calls_need_an_authorized_grant_and_succeeded_faults_are_cancelled_on_exit() {
    let (mut parent, cluster, context, mut mailbox) = fixture();
    let mut start = requested_start(&parent).unwrap();
    start.control = Some(serde_json::from_value(json!({"lifecycle":["chain"],"network":[{"from_component":"chain","to_component":"scripts"}],"max_fault_seconds":45})).unwrap());
    if let CellAction::ComponentExecLive(request) = &mut parent.spec.action {
        request.argv[3] =
            serde_json::to_string(&WorkspaceRequest::Task(TaskRequest::Start(start.clone())))
                .unwrap();
    }
    assert!(!is_controlled_start(&parent));
    parent.annotations_mut().insert(
        GRANT_ANNOTATION.into(),
        proofstorm_core::digest_json(start.control.as_ref().unwrap()),
    );
    assert!(is_controlled_start(&parent));
    let restart: ControlCall = serde_json::from_value(
        json!({"call_id":"restart","operation":{"kind":"component_restart","component":"chain"}}),
    )
    .unwrap();
    let child = child_action(&parent, &restart);
    assert!(matches!(child.spec.action, CellAction::ComponentRestart(_)));
    assert_eq!(child.spec.capability, Capability::ComponentControl);
    assert!(child_matches(&parent, &restart, &child));
    let partition: ControlCall = serde_json::from_value(json!({"call_id":"outage","operation":{"kind":"network_partition","from_component":"chain","to_component":"scripts","duration_seconds":30}})).unwrap();
    mailbox.poll["state"]["request_digest"] = json!(proofstorm_core::digest_json(&start));
    mailbox.poll["pending"] =
        json!({"call":partition,"digest":proofstorm_core::digest_json(&partition),"claimed":false});
    tick(&parent, &context, &mut mailbox).await.unwrap();
    {
        let mut cluster = cluster.lock().unwrap();
        let child = cluster.child.as_mut().unwrap();
        assert_eq!(child.spec.capability, Capability::NetworkPartition);
        assert_eq!(
            child.annotations()[super::super::workspace_faults::EXPIRES_ANNOTATION]
                .parse::<i64>()
                .unwrap(),
            child.spec.accepted_at_unix + 30
        );
        child.status = Some(ProofstormCellActionStatus {
            phase: ActionPhase::Succeeded,
            artifact: Some(super::super::status_object(
                json!({"cleanup_verified":false}),
            )),
            ..Default::default()
        });
    }
    tick(&parent, &context, &mut mailbox).await.unwrap();
    assert!(mailbox.poll["pending"].is_null());
    mailbox.poll["state"]["phase"] = json!("stopping");
    tick(&parent, &context, &mut mailbox).await.unwrap();
    assert_eq!(cluster.lock().unwrap().cancellations, 1);
    assert_eq!(
        mailbox.poll["state"]["control_cleanup"]["pending_faults"],
        1
    );
    cluster
        .lock()
        .unwrap()
        .child
        .as_mut()
        .unwrap()
        .status
        .as_mut()
        .unwrap()
        .artifact
        .as_mut()
        .unwrap()
        .insert("cleanup_verified".into(), json!(true));
    assert_eq!(
        tick(&parent, &context, &mut mailbox).await.unwrap(),
        Action::requeue(Duration::from_secs(POLL_SECONDS))
    );
    assert_eq!(
        mailbox.poll["state"]["control_cleanup"],
        json!({"pending_faults":0,"task_phase":"stopping"})
    );
    mailbox.poll["state"]["phase"] = json!("cancelled");
    assert_eq!(
        tick(&parent, &context, &mut mailbox).await.unwrap(),
        Action::await_change()
    );
    assert_eq!(
        mailbox.poll["state"]["control_cleanup"],
        json!({"pending_faults":0,"task_phase":"cancelled"})
    );
}
