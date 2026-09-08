use super::*;
use proofstorm_app::lab::ComponentControlRequest;
use proofstorm_core::OperationKind;

fn request(instance: &str, id: &str) -> ComponentControlRequest {
    ComponentControlRequest {
        instance_id: instance.into(),
        experiment_id: String::new(),
        session_id: String::new(),
        operation_id: id.into(),
        component: "chain".into(),
        idempotency_key: id.into(),
    }
}

#[tokio::test]
async fn component_controls_share_admission_replay_and_cross_actor_ordering() {
    let store = Store::memory().unwrap();
    seed(&store);
    store
        .grant("local", "developer", Capability::ComponentControl)
        .unwrap();
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let applied = native(&labs, "component-lab").await;
    let instance = &applied.instance.id;
    let first = labs
        .control_component(request(instance, "stop-one"), OperationKind::ComponentStop)
        .await
        .unwrap();
    assert_eq!(first.phase, OperationPhase::Running);
    assert!(!first.experiment_id.is_empty());
    let requests_before = cluster.lock().unwrap().requests.len();
    let replay = labs
        .control_component(request(instance, "stop-one"), OperationKind::ComponentStop)
        .await
        .unwrap();
    assert_eq!(first, replay);
    let mut new_key = request(instance, "stop-one");
    new_key.idempotency_key = "stop-one-retry".into();
    let retried = labs
        .control_component(new_key, OperationKind::ComponentStop)
        .await
        .unwrap();
    assert_eq!(
        first, retried,
        "transport retry keys must not change the operation's payload"
    );
    assert_eq!(
        cluster.lock().unwrap().requests.len(),
        requests_before,
        "replay must not contact the workload or recreate the action"
    );
    store.put_principal("second").unwrap();
    for capability in [
        Capability::ComponentControl,
        Capability::LabOperate,
        Capability::ExperimentCreate,
    ] {
        store.grant("local", "second", capability).unwrap();
    }
    let mut second = labs.clone();
    second.principal = "second".into();
    let newer = second
        .control_component(
            request(instance, "start-two"),
            OperationKind::ComponentStart,
        )
        .await
        .unwrap();
    assert_ne!(newer.experiment_id, first.experiment_id);
    assert!(
        newer.sequence > first.sequence,
        "different actors must have one comparable lifecycle order"
    );
    let third = labs
        .control_component(
            request(instance, "restart-three"),
            OperationKind::ComponentRestart,
        )
        .await
        .unwrap();
    assert!(third.sequence > newer.sequence);
    let wrong = labs
        .control_component(request(instance, "stop-one"), OperationKind::ComponentStart)
        .await;
    assert!(
        wrong.is_err(),
        "an operation id cannot be reused with different intent"
    );
}

#[tokio::test]
async fn component_controls_reject_missing_components_and_wrong_capabilities_before_submission() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let applied = native(&labs, "component-invalid").await;
    assert!(
        labs.control_component(
            request(&applied.instance.id, "not-authorized"),
            OperationKind::ComponentStop
        )
        .await
        .is_err()
    );
    store
        .grant("local", "developer", Capability::ComponentControl)
        .unwrap();
    let mut missing = request(&applied.instance.id, "missing");
    missing.component = "absent".into();
    assert_eq!(
        labs.control_component(missing, OperationKind::ComponentStop)
            .await
            .unwrap_err()
            .details
            .unwrap()["code"],
        "component_not_found"
    );
    assert!(
        cluster
            .lock()
            .unwrap()
            .objects
            .values()
            .all(|o| o["kind"] != "ProofstormLabAction")
    );
}
