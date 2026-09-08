use super::*;
use proofstorm_core::{LabSpec, PrivateTransferScope, PublishedRevision, SessionPhase};
use serde_json::json;
fn seed(store: &Store) {
    store
        .put_workspace(&Workspace {
            id: "workspace".into(),
            name: "workspace".into(),
        })
        .unwrap();
    for principal in ["sender", "receiver", "stranger"] {
        store.put_principal(principal).unwrap();
        for capability in [
            Capability::LabOperate,
            Capability::LabClose,
            Capability::ComponentExecLive,
            Capability::WalletControl,
            Capability::ArtifactRead,
            Capability::ExperimentRead,
            Capability::ExperimentCreate,
            Capability::ActionCancel,
        ] {
            store.grant("workspace", principal, capability).unwrap();
        }
    }
    let lab: LabSpec = serde_json::from_value(json!({"api_version":proofstorm_core::API_VERSION,"name":"lab","links":[],
            "components":[
                {"id":"wallet-a","kind":"wallet","implementation":"cocod-wallet","config_version":"test","control":"laboratory","config":{}},
                {"id":"wallet-b","kind":"wallet","implementation":"cdk-cli-wallet","config_version":"test","control":"laboratory","config":{}},
                {"id":"mint","kind":"mint","implementation":"cdk-mint","config_version":"test","control":"laboratory","config":{}}
            ]})).unwrap();
    let revision = PublishedRevision {
        workspace_id: "workspace".into(),
        digest: "revision".into(),
        lab,
        lock: proofstorm_core::ResolvedLock {
            api_version: proofstorm_core::API_VERSION.into(),
            digest: "lock".into(),
            entries: vec![],
        },
    };
    let db = store.lock().unwrap();
    db.execute(
        "INSERT INTO revisions VALUES('revision','workspace','draft',1,?1)",
        [serde_json::to_string(&revision).unwrap()],
    )
    .unwrap();
    db.execute("INSERT INTO instances VALUES('workspace','instance','revision','lock','instance-key','lab')",[]).unwrap();
    drop(db);
    store
        .create_experiment(
            "workspace",
            "sender",
            "experiment",
            "instance",
            "experiment-create",
        )
        .unwrap();
}

fn submit(store: &Store, actor: &str, session: &str, id: &str) -> LabOperation {
    store
        .create_operation(
            "workspace",
            actor,
            "instance",
            "experiment",
            session,
            id,
            OperationKind::WalletBalance,
            &json!({"wallet":"wallet-b","mint":"mint"}),
            id,
            Capability::WalletControl,
        )
        .unwrap()
}
#[test]
fn overlapping_agents_and_finished_sessions_never_gate_actions_or_retries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    let first = Store::open(&path).unwrap();
    seed(&first);
    let second = Store::open(&path).unwrap();
    let a = submit(&first, "sender", "", "one");
    let b = submit(&second, "sender", "", "two");
    let c = submit(&second, "receiver", "", "three");
    assert_ne!(a.session_id, b.session_id);
    assert_ne!(b.session_id, c.session_id);
    let overlaps = first
        .overlapping_sessions("workspace", "sender", &a.session_id, "", 1)
        .unwrap();
    assert_eq!(overlaps.sessions.len(), 1);
    assert!(overlaps.next_cursor.is_some());
    let next = first
        .overlapping_sessions(
            "workspace",
            "sender",
            &a.session_id,
            overlaps.next_cursor.as_deref().unwrap(),
            1,
        )
        .unwrap();
    assert_eq!(next.sessions.len(), 1);
    assert_ne!(next.sessions[0].id, overlaps.sessions[0].id);
    first
        .finish_session("workspace", "sender", &a.session_id, "finish")
        .unwrap();
    assert_eq!(
        first.operation("workspace", "sender", "one").unwrap().phase,
        OperationPhase::Pending
    );
    let later = submit(&first, "sender", &a.session_id, "four");
    assert_ne!(later.session_id, a.session_id);
    let replay = submit(&first, "sender", "", "one");
    assert_eq!(replay.session_id, a.session_id);
    assert_eq!(
        first
            .session("workspace", "sender", &a.session_id)
            .unwrap()
            .phase,
        SessionPhase::Finished
    );
    first
        .record_operation_result(
            "workspace",
            "one",
            OperationPhase::Succeeded,
            json!({"complete":true}),
        )
        .unwrap();
    assert!(
        first
            .instance_for_close("workspace", "sender", "instance")
            .is_ok()
    );
    drop(second);
    assert_eq!(
        first
            .session("workspace", "sender", &b.session_id)
            .unwrap()
            .phase,
        SessionPhase::Finished
    );
}
#[test]
fn observation_is_pure_and_completion_advances_last_activity() {
    let store = Store::memory().unwrap();
    seed(&store);
    let op = submit(&store, "sender", "", "work");
    store
        .lock()
        .unwrap()
        .execute("UPDATE sessions SET last_activity_at=0,started_at=0", [])
        .unwrap();
    assert_eq!(
        store
            .sessions("workspace", "sender", "instance", "", 20)
            .unwrap()
            .sessions[0]
            .last_activity_at_unix,
        0
    );
    assert_eq!(
        store
            .session("workspace", "sender", &op.session_id)
            .unwrap()
            .last_activity_at_unix,
        0
    );
    store
        .record_operation_result("workspace", "work", OperationPhase::Succeeded, json!({}))
        .unwrap();
    assert!(
        store
            .session("workspace", "sender", &op.session_id)
            .unwrap()
            .last_activity_at_unix
            > 0
    );
}
#[test]
fn private_permissions_survive_session_finish_but_explicit_revocation_still_works() {
    let store = Store::memory().unwrap();
    seed(&store);
    store
        .revoke("workspace", "receiver", Capability::LabOperate)
        .unwrap();
    let scope = PrivateTransferScope {
        issuer_principal_id: "sender".into(),
        component: "wallet-b".into(),
        mint: "mint".into(),
        reference: "payload-one".into(),
        receive_command_digest: format!("sha256:{}", "a".repeat(64)),
    };
    let session = store
        .start_session(
            "workspace",
            "sender",
            "experiment",
            "sender-session",
            "start",
        )
        .unwrap();
    store
        .issue_private_access(
            "workspace",
            "sender",
            "receiver",
            "access-one",
            "instance",
            &scope,
            "issue",
        )
        .unwrap();
    store
        .finish_session("workspace", "sender", &session.id, "finish")
        .unwrap();
    let accepted = submit(&store, "receiver", "", "received-balance");
    assert!(store.operation_access_scope(&accepted).unwrap().is_some());
    assert!(
        store
            .create_operation(
                "workspace",
                "receiver",
                "instance",
                "experiment",
                "",
                "unbound",
                OperationKind::ComponentExecLive,
                &json!({"component":"wallet-b","argv":["arbitrary"]}),
                "unbound",
                Capability::ComponentExecLive
            )
            .is_err()
    );
    store
        .revoke_private_access("workspace", "sender", "access-one")
        .unwrap();
    assert!(
        store
            .create_operation(
                "workspace",
                "receiver",
                "instance",
                "experiment",
                "",
                "after-revoke",
                OperationKind::WalletBalance,
                &json!({"wallet":"wallet-b","mint":"mint"}),
                "after-revoke",
                Capability::WalletControl
            )
            .is_err()
    );
    assert_eq!(
        store
            .operation("workspace", "receiver", "received-balance")
            .unwrap()
            .phase,
        OperationPhase::Pending
    );
}

fn implicit_submit(store: &Store, actor: &str, id: &str) -> Result<LabOperation, StoreError> {
    store.create_operation(
        "workspace",
        actor,
        "instance",
        "",
        "",
        id,
        OperationKind::WalletBalance,
        &json!({"wallet":"wallet-b","mint":"mint","experiment_id":""}),
        id,
        Capability::WalletControl,
    )
}

#[test]
fn implicit_runs_need_no_experiment_grant_and_retries_keep_original_attribution() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runs.db");
    let first = Store::open(&path).unwrap();
    seed(&first);
    first
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM grants WHERE capability='experiment.create'",
            [],
        )
        .unwrap();
    let op = implicit_submit(&first, "sender", "implicit-one").unwrap();
    assert_eq!(op.request["experiment_id"], op.experiment_id);
    first
        .finish_session("workspace", "sender", &op.session_id, "finish")
        .unwrap();
    let second = Store::open(&path).unwrap();
    assert_eq!(
        implicit_submit(&second, "sender", "implicit-one").unwrap(),
        op
    );
    let next = implicit_submit(&second, "sender", "implicit-two").unwrap();
    assert_eq!(next.experiment_id, op.experiment_id);
    assert_ne!(next.session_id, op.session_id);
    let other = implicit_submit(&second, "receiver", "implicit-three").unwrap();
    assert_ne!(other.experiment_id, op.experiment_id);
    assert_eq!(
        second
            .experiment_unchecked("workspace", &other.experiment_id)
            .unwrap()
            .owner_principal_id,
        "receiver"
    );
    assert_eq!(
        second
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='operations'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn concurrent_default_run_creation_converges_without_ownership_collisions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runs.db");
    let first = Store::open(&path).unwrap();
    seed(&first);
    let second = Store::open(&path).unwrap();
    let worker =
        std::thread::spawn(move || implicit_submit(&second, "sender", "concurrent-b").unwrap());
    let a = implicit_submit(&first, "sender", "concurrent-a").unwrap();
    let b = worker.join().unwrap();
    assert_eq!(a.experiment_id, b.experiment_id);
    assert_ne!(a.session_id, b.session_id);
    assert_ne!(a.sequence, b.sequence);
}

#[test]
fn denied_and_closed_default_runs_do_not_silently_create_a_new_group() {
    let store = Store::memory().unwrap();
    seed(&store);
    store.replace_grants("workspace", "stranger", []).unwrap();
    assert!(implicit_submit(&store, "stranger", "denied").is_err());
    assert_eq!(
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM experiments WHERE owner_principal_id='stranger'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let op = implicit_submit(&store, "sender", "before-close").unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE experiments SET phase_json='\"closed\"' WHERE id=?1",
            [&op.experiment_id],
        )
        .unwrap();
    assert!(
        implicit_submit(&store, "sender", "after-close")
            .unwrap_err()
            .to_string()
            .contains("closed")
    );
    assert_eq!(
        implicit_submit(&store, "sender", "before-close").unwrap(),
        op
    );
    let wrong = store.create_operation(
        "workspace",
        "sender",
        "instance",
        "missing-run",
        "",
        "wrong-run",
        OperationKind::WalletBalance,
        &json!({"wallet":"wallet-b","mint":"mint"}),
        "wrong-run",
        Capability::WalletControl,
    );
    assert!(wrong.is_err());
}

#[test]
fn closing_and_recreation_preserve_the_lab_incarnation_boundary() {
    let store = Store::memory().unwrap();
    seed(&store);
    let op = implicit_submit(&store, "sender", "first-incarnation").unwrap();
    store
        .begin_instance_close("workspace", "sender", "instance")
        .unwrap();
    assert!(
        implicit_submit(&store, "receiver", "while-closing")
            .unwrap_err()
            .to_string()
            .contains("closing")
    );
    let instance = store.instance_unchecked("workspace", "instance").unwrap();
    let _guard = store.try_lifecycle_guard().unwrap().unwrap();
    store.purge_lab(&instance).unwrap();
    assert!(store.operation_unchecked("workspace", &op.id).is_err());
    assert!(
        store
            .experiment_unchecked("workspace", &op.experiment_id)
            .is_err()
    );
    seed(&store);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE instances SET instance_key='replacement-key' WHERE id='instance'",
            [],
        )
        .unwrap();
    let replacement = implicit_submit(&store, "sender", "replacement").unwrap();
    assert_ne!(op.experiment_id, replacement.experiment_id);
    assert_ne!(op.session_id, replacement.session_id);
}
