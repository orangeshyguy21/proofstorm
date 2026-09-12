use proofstorm_core::{
    Capability, CellSpec, CellUpdateTarget, OperationKind, OperationPhase, PublishedRevision,
};
use proofstorm_store::{Store, Workspace};
use serde_json::json;

fn seed(store: &Store) {
    store
        .put_workspace(&Workspace {
            id: "w".into(),
            name: "w".into(),
        })
        .unwrap();
    store.put_principal("agent").unwrap();
    for cap in [
        Capability::CellCreate,
        Capability::CellRead,
        Capability::CellEdit,
        Capability::CellPublish,
        Capability::CellMaterialize,
        Capability::CellStatus,
        Capability::CellClose,
        Capability::CatalogRead,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::CellOperate,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
    ] {
        store.grant("w", "agent", cap).unwrap();
    }
}
fn cell(ids: &[&str]) -> CellSpec {
    serde_json::from_value(json!({"api_version":"proofstorm/v1alpha1","name":"edit-test","components":ids.iter().map(|id|json!({"id":id,"kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}})).collect::<Vec<_>>(),"links":[]})).unwrap()
}
fn publish(store: &Store, id: &str, spec: &CellSpec) -> PublishedRevision {
    store
        .create_draft("w", "agent", id, spec, &format!("{id}-draft"))
        .unwrap();
    store
        .publish("w", "agent", id, 1, &format!("{id}-publish"))
        .unwrap()
}
#[test]
fn edits_preserve_identity_reject_stale_plans_and_never_replay_old_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let first = publish(&store, "first", &cell(&["chain"]));
    let original = store
        .materialize("w", "agent", "demo", &first.digest, "start")
        .unwrap();
    let second = publish(&store, "second", &cell(&["chain", "other"]));
    let target = CellUpdateTarget {
        delete_retained: vec![],
        instance_id: "demo".into(),
        expected_generation: 1,
        delete_data: false,
    };
    let plan = store
        .plan_update("w", "agent", target.clone(), &second)
        .unwrap();
    assert_eq!(plan.changes.unchanged, vec!["chain"]);
    assert_eq!(plan.changes.added, vec!["other"]);
    let updated = store
        .accept_update("w", "agent", &plan, "apply-two")
        .unwrap();
    assert_eq!(updated.instance_key, original.instance_key);
    assert_eq!(updated.resource_name, original.resource_name);
    assert_eq!(updated.generation, 2);
    assert!(
        store
            .accept_update("w", "agent", &plan, "different-key")
            .unwrap_err()
            .to_string()
            .contains("cell_update_conflict")
    );
    assert_eq!(
        store
            .accept_update("w", "agent", &plan, "apply-two")
            .unwrap(),
        updated
    );
    let third = publish(&store, "third", &cell(&["chain", "other", "third"]));
    let plan3 = store
        .plan_update(
            "w",
            "agent",
            CellUpdateTarget {
                delete_retained: vec![],
                expected_generation: 2,
                ..target
            },
            &third,
        )
        .unwrap();
    let latest = store
        .accept_update("w", "agent", &plan3, "apply-three")
        .unwrap();
    assert_eq!(
        store
            .accept_update("w", "agent", &plan, "apply-two")
            .unwrap(),
        latest
    );
    assert_eq!(
        store
            .materialize("w", "agent", "demo", &first.digest, "start")
            .unwrap(),
        latest
    );
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.instance("w", "agent", "demo").unwrap(), latest);
    assert_eq!(
        reopened.pending_updates("w", "agent").unwrap(),
        vec!["demo"]
    );
    reopened.begin_instance_close("w", "agent", "demo").unwrap();
    assert_eq!(
        reopened
            .materialize("w", "agent", "demo", &first.digest, "start")
            .unwrap_err()
            .code(),
        "cell_closing"
    );
    assert!(
        reopened
            .plan_update(
                "w",
                "agent",
                CellUpdateTarget {
                    delete_retained: vec![],
                    instance_id: "demo".into(),
                    expected_generation: 3,
                    delete_data: false
                },
                &first
            )
            .unwrap_err()
            .to_string()
            .contains("cell_closing")
    );
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the test follows one operation across addition, removal and retained-state reuse"
)]
fn operation_snapshots_and_removal_conflicts_are_component_scoped() {
    let store = Store::memory().unwrap();
    seed(&store);
    let first = publish(&store, "first", &cell(&["chain"]));
    store
        .materialize("w", "agent", "demo", &first.digest, "start")
        .unwrap();
    store
        .create_experiment("w", "agent", "run", "demo", "run")
        .unwrap();
    let request = json!({"component":"chain","argv":["bitcoin-cli","getblockcount"]});
    let operation = store
        .create_operation_at_revision(
            &first.digest,
            "w",
            "agent",
            "demo",
            "run",
            "",
            "op",
            OperationKind::ComponentExecLive,
            &request,
            "op",
            Capability::ComponentExecLive,
        )
        .unwrap();
    let next = publish(&store, "next", &cell(&["chain", "new"]));
    let plan = store
        .plan_update(
            "w",
            "agent",
            CellUpdateTarget {
                delete_retained: vec![],
                instance_id: "demo".into(),
                expected_generation: 1,
                delete_data: false,
            },
            &next,
        )
        .unwrap();
    store
        .accept_update("w", "agent", &plan, "addition")
        .unwrap();
    assert_eq!(
        store.operation("w", "agent", "op").unwrap().revision_digest,
        first.digest
    );
    assert_eq!(operation.revision_digest, first.digest);
    assert!(
        store
            .create_operation_at_revision(
                &first.digest,
                "w",
                "agent",
                "demo",
                "run",
                "",
                "stale-op",
                OperationKind::ComponentExecLive,
                &request,
                "stale-op",
                Capability::ComponentExecLive
            )
            .unwrap_err()
            .to_string()
            .contains("cell_update_conflict")
    );
    let removed = publish(&store, "removed", &cell(&["new"]));
    let removal = store
        .plan_update(
            "w",
            "agent",
            CellUpdateTarget {
                delete_retained: vec![],
                instance_id: "demo".into(),
                expected_generation: 2,
                delete_data: false,
            },
            &removed,
        )
        .unwrap();
    assert!(
        store
            .accept_update("w", "agent", &removal, "remove")
            .unwrap_err()
            .to_string()
            .contains("op")
    );
    store
        .record_operation_result("w", "op", OperationPhase::Succeeded, json!({"ok":true}))
        .unwrap();
    store
        .accept_update("w", "agent", &removal, "remove")
        .unwrap();
    assert!(
        store
            .plan_update(
                "w",
                "agent",
                CellUpdateTarget {
                    delete_retained: vec![],
                    instance_id: "demo".into(),
                    expected_generation: 3,
                    delete_data: false
                },
                &next
            )
            .unwrap_err()
            .to_string()
            .contains("retained_component_conflict")
    );
}

#[test]
fn no_op_receipts_do_not_advance_and_cannot_roll_back_later_edits() {
    let store = Store::memory().unwrap();
    seed(&store);
    let first = publish(&store, "first", &cell(&["chain"]));
    store
        .materialize("w", "agent", "demo", &first.digest, "start")
        .unwrap();
    let target = CellUpdateTarget {
        instance_id: "demo".into(),
        expected_generation: 1,
        delete_data: false,
        delete_retained: vec![],
    };
    let noop = store
        .plan_update("w", "agent", target.clone(), &first)
        .unwrap();
    assert!(noop.is_noop());
    assert_eq!(
        store
            .accept_update("w", "agent", &noop, "noop")
            .unwrap()
            .generation,
        1
    );
    assert!(store.pending_updates("w", "agent").unwrap().is_empty());
    let second = publish(&store, "second", &cell(&["chain", "other"]));
    let change = store.plan_update("w", "agent", target, &second).unwrap();
    store.accept_update("w", "agent", &change, "edit").unwrap();
    let replay = store.accept_update("w", "agent", &noop, "noop").unwrap();
    assert_eq!(replay.generation, 2);
    assert_eq!(replay.revision_digest, second.digest);
    assert_eq!(
        store
            .accept_update("w", "agent", &change, "start")
            .unwrap_err()
            .code(),
        "idempotency_conflict"
    );
}

#[test]
fn independent_database_connections_cannot_both_accept_the_same_base_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let first = publish(&store, "first", &cell(&["chain"]));
    store
        .materialize("w", "agent", "demo", &first.digest, "start")
        .unwrap();
    let target = CellUpdateTarget {
        instance_id: "demo".into(),
        expected_generation: 1,
        delete_data: false,
        delete_retained: vec![],
    };
    let plans = ["one", "two"].map(|id| {
        let revision = publish(&store, id, &cell(&["chain", id]));
        store
            .plan_update("w", "agent", target.clone(), &revision)
            .unwrap()
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = plans.map(|plan| {
        let store = Store::open(&path).unwrap();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.accept_update("w", "agent", &plan, &plan.digest)
        })
    });
    let results = handles.map(|h| h.join().unwrap());
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .code(),
        "cell_update_conflict"
    );
    assert_eq!(store.instance("w", "agent", "demo").unwrap().generation, 2);
}
