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
#[test]
fn legacy_history_migrates_without_exclusivity_or_lost_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("old.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let original = store
        .create_operation(
            "workspace",
            "sender",
            "instance",
            "experiment",
            "old-session",
            "original",
            OperationKind::WalletBalance,
            &json!({"wallet":"wallet-b","mint":"mint","lease_id":"old-session"}),
            "original",
            Capability::WalletControl,
        )
        .unwrap();
    store
        .record_operation_result(
            "workspace",
            "original",
            OperationPhase::Succeeded,
            json!({"retained":true}),
        )
        .unwrap();
    {
        let db = store.lock().unwrap();
        db.execute_batch("ALTER TABLE sessions RENAME TO experiment_leases;
            ALTER TABLE experiment_leases RENAME COLUMN started_at TO acquired_at;
            ALTER TABLE experiment_leases RENAME COLUMN finished_at TO released_at;
            ALTER TABLE experiment_leases DROP COLUMN last_activity_at;
            ALTER TABLE experiment_leases ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE experiment_leases ADD COLUMN max_actions INTEGER NOT NULL DEFAULT 1;
            ALTER TABLE experiment_leases ADD COLUMN delegation_json TEXT;
            CREATE UNIQUE INDEX one_active_lease_per_instance ON experiment_leases(workspace_id,instance_id) WHERE phase_json='\"active\"';
            ALTER TABLE actions RENAME COLUMN session_id TO lease_id;
            ALTER TABLE wallet_payment_claims RENAME COLUMN session_id TO lease_id;
            ALTER TABLE wallet_quote_observations RENAME COLUMN session_id TO lease_id;
            UPDATE grants SET capability='lease.acquire' WHERE capability='lab.operate';
            UPDATE idempotency SET response_json=json_remove(json_set(response_json,'$.lease_id',json_extract(response_json,'$.session_id')),'$.session_id') WHERE operation='lab.operation.create';").unwrap();
    }
    drop(store);
    let store = Store::open(&path).unwrap();
    let replay = store
        .create_operation(
            "workspace",
            "sender",
            "instance",
            "experiment",
            "old-session",
            "original",
            OperationKind::WalletBalance,
            &json!({"wallet":"wallet-b","mint":"mint","session_id":"old-session"}),
            "original",
            Capability::WalletControl,
        )
        .unwrap();
    assert_eq!(replay.request, original.request);
    assert_eq!(replay.request_digest, original.request_digest);
    assert_eq!(replay.artifact.unwrap().content, json!({"retained":true}));
    let concurrent = submit(&store, "receiver", "", "concurrent");
    assert_ne!(concurrent.session_id, "old-session");
    assert_eq!(
        store
            .sessions("workspace", "sender", "instance", "", 100)
            .unwrap()
            .sessions
            .len(),
        2
    );
    assert_eq!(store.lock().unwrap().query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='experiment_leases' OR name='one_active_lease_per_instance'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
