use super::*;
use proofstorm_core::{CellSpec, PrivateTransferScope, PublishedRevision, SessionPhase};
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
            Capability::CellOperate,
            Capability::CellClose,
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
    let cell: CellSpec = serde_json::from_value(json!({"api_version":proofstorm_core::API_VERSION,"name":"cell","links":[],
            "components":[
                {"id":"wallet-a","kind":"wallet","implementation":"cocod-wallet","config_version":"test","control":"cell","config":{}},
                {"id":"wallet-b","kind":"wallet","implementation":"cdk-cli-wallet","config_version":"test","control":"cell","config":{}},
                {"id":"mint","kind":"mint","implementation":"cdk-mint","config_version":"test","control":"cell","config":{}}
            ]})).unwrap();
    let revision = PublishedRevision {
        workspace_id: "workspace".into(),
        digest: "revision".into(),
        cell,
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
    db.execute("INSERT INTO instances VALUES('workspace','instance','revision','lock','instance-key','cell')",[]).unwrap();
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

#[test]
fn directory_filters_before_limit_and_does_not_decode_unrelated_sessions_or_write() {
    let store = Store::memory().unwrap();
    seed(&store);
    {
        let db = store.lock().unwrap();
        for index in 0..2000 {
            db.execute("INSERT INTO sessions(workspace_id,id,experiment_id,instance_id,principal_id,phase_json,started_at,last_activity_at,finished_at) VALUES('workspace',?1,'experiment','instance',?2,?3,?4,?4,?5)",
                params![format!("session-{index:04}"),if index<1990 {"sender"} else {"receiver"},if index<1990 {"malformed unrelated record"} else {"\"finished\""},index,if index<1990 {None} else {Some(3000)}]).unwrap();
        }
    }
    let before = store.observation_token("workspace", "sender").unwrap();
    let digest = store
        .session_observation_digest("workspace", "sender", "instance")
        .unwrap();
    let filter = SessionFilters {
        principal_id: Some("receiver".into()),
        run_id: Some("experiment".into()),
        phase: Some(SessionPhase::Finished),
        started_after_unix: Some(1992),
        started_before_unix: Some(1998),
        ..SessionFilters::default()
    };
    let page = store
        .session_candidates(
            "workspace",
            "sender",
            "instance",
            &filter,
            SessionWindow {
                after_id: "",
                limit: 3,
                observed_at: 4000,
            },
        )
        .unwrap();
    assert_eq!(
        page.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["session-1992", "session-1993", "session-1994"]
    );
    let next = store
        .session_candidates(
            "workspace",
            "sender",
            "instance",
            &filter,
            SessionWindow {
                after_id: &page.last().unwrap().id,
                limit: 3,
                observed_at: 4000,
            },
        )
        .unwrap();
    assert_eq!(
        next.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["session-1995", "session-1996", "session-1997"]
    );
    assert_eq!(
        store.observation_token("workspace", "sender").unwrap(),
        before
    );
    assert_eq!(
        store
            .session_observation_digest("workspace", "sender", "instance")
            .unwrap(),
        digest
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at=last_activity_at+1 WHERE id='session-1992'",
            [],
        )
        .unwrap();
    assert_ne!(
        store
            .session_observation_digest("workspace", "sender", "instance")
            .unwrap(),
        digest
    );
}

#[test]
fn directory_overlap_uses_a_fixed_observation_time_and_cell_scope() {
    let store = Store::memory().unwrap();
    seed(&store);
    let db = store.lock().unwrap();
    for (id, start, end) in [
        ("anchor", 10, None),
        ("earlier", 1, Some(9)),
        ("intersects", 5, Some(12)),
        ("future", 30, None),
    ] {
        db.execute("INSERT INTO sessions(workspace_id,id,experiment_id,instance_id,principal_id,phase_json,started_at,last_activity_at,finished_at) VALUES('workspace',?1,'experiment','instance','sender','\"active\"',?2,?2,?3)",params![id,start,end]).unwrap();
    }
    drop(db);
    let filter = SessionFilters {
        overlaps_with: Some("anchor".into()),
        ..SessionFilters::default()
    };
    let matches = store
        .session_candidates(
            "workspace",
            "sender",
            "instance",
            &filter,
            SessionWindow {
                after_id: "",
                limit: 20,
                observed_at: 20,
            },
        )
        .unwrap();
    assert_eq!(
        matches.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["intersects"]
    );
    assert!(
        store
            .session_candidates(
                "workspace",
                "sender",
                "another-cell",
                &filter,
                SessionWindow {
                    after_id: "",
                    limit: 20,
                    observed_at: 20
                }
            )
            .is_err()
    );
}

fn submit(store: &Store, actor: &str, session: &str, id: &str) -> CellOperation {
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
        .revoke("workspace", "receiver", Capability::CellOperate)
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

fn implicit_submit(store: &Store, actor: &str, id: &str) -> Result<CellOperation, StoreError> {
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
fn denied_calls_do_not_create_runs_and_closed_defaults_roll_forward_explicitly() {
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
    let continued = implicit_submit(&store, "sender", "after-close").unwrap();
    assert_ne!(continued.experiment_id, op.experiment_id);
    assert_eq!(
        implicit_submit(&store, "sender", "after-close").unwrap(),
        continued
    );
    let sealed = store.create_operation(
        "workspace",
        "sender",
        "instance",
        &op.experiment_id,
        "",
        "explicit-closed-run",
        OperationKind::WalletBalance,
        &json!({"wallet":"wallet-b","mint":"mint"}),
        "explicit-closed-run",
        Capability::WalletControl,
    );
    assert!(
        sealed
            .unwrap_err()
            .to_string()
            .contains("action run must be open")
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
fn closing_and_recreation_preserve_the_cell_incarnation_boundary() {
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
    store.purge_cell(&instance).unwrap();
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
