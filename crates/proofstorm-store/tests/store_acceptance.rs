use std::collections::BTreeMap;

use proofstorm_core::{
    API_VERSION, CANDIDATE_BUILD_API_VERSION, CandidateBuild, CandidateBuildPhase, Capability,
    CellPolicy, CellSpec, ComponentKind, ComponentSpec, ControlClass, LOCK_API_VERSION,
    OperationKind, OperationPhase,
};
use proofstorm_store::{Store, StoreError, Workspace};

fn empty_cell(name: &str) -> CellSpec {
    CellSpec {
        api_version: API_VERSION.into(),
        name: name.into(),
        components: vec![],
        links: vec![],
        policy: CellPolicy::default(),
    }
}

#[test]
fn successful_candidate_becomes_a_workspace_catalog_version() {
    let store = Store::memory().expect("store");
    seed(&store);
    for capability in [Capability::CandidateBuild, Capability::CandidateRead] {
        store
            .grant("alpha", "designer", capability)
            .expect("candidate grant");
    }
    let candidate = CandidateBuild {
        diagnostics: None,
        api_version: CANDIDATE_BUILD_API_VERSION.into(),
        id: "nutshell-pr-1095".into(),
        workspace_id: "alpha".into(),
        principal_id: "designer".into(),
        implementation: "nutshell".into(),
        base_version: "0.20.3".into(),
        pull_request_url: "https://github.com/cashubtc/nutshell/pull/1095".into(),
        resource_name: "candidate-aabbccdd".into(),
        request_digest: "sha256:request".into(),
        provenance: None,
        build_features: [
            proofstorm_core::CatalogFeature::MintManagementRpc,
            proofstorm_core::CatalogFeature::NativeCliEntrypoints,
        ]
        .into_iter()
        .collect(),
        phase: CandidateBuildPhase::Pending,
        accepted_at_unix: 1,
        started_at_unix: None,
        completed_at_unix: None,
        repository: Some("https://github.com/cashubtc/nutshell.git".into()),
        commit_sha: Some("aabbccddaabbccddaabbccddaabbccddaabbccdd".into()),
        version: Some("candidate-pr1095-aabbccdd".into()),
        image: None,
        error_code: None,
        error_message: None,
    };
    let created = store
        .create_candidate_build("alpha", "designer", &candidate, "candidate-1")
        .expect("create candidate");
    assert_eq!(created, candidate);
    let mut succeeded = candidate;
    succeeded.phase = CandidateBuildPhase::Succeeded;
    succeeded.started_at_unix = Some(2);
    succeeded.completed_at_unix = Some(3);
    succeeded.image = Some(format!(
        "proofstorm-registry.localhost:5000/proofstorm-candidates/nutshell@sha256:{}",
        "1".repeat(64)
    ));
    store
        .update_candidate_build("alpha", &succeeded)
        .expect("record success");
    let catalog = store
        .effective_catalog("alpha", "designer")
        .expect("effective catalog");
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.version == "candidate-pr1095-aabbccdd")
        .expect("candidate version");
    assert_eq!(
        entry
            .source
            .as_ref()
            .map(|source| source.candidate_id.as_str()),
        Some("nutshell-pr-1095")
    );
}

fn seed(store: &Store) {
    for workspace in ["alpha", "beta"] {
        store
            .put_workspace(&Workspace {
                id: workspace.into(),
                name: workspace.into(),
            })
            .expect("workspace");
    }
    for principal in ["designer", "reader"] {
        store.put_principal(principal).expect("principal");
    }
    for capability in [
        Capability::CatalogRead,
        Capability::CellRead,
        Capability::CellCreate,
        Capability::CellEdit,
        Capability::CellClone,
        Capability::CellValidate,
        Capability::CellPublish,
        Capability::CellMaterialize,
        Capability::CellStatus,
        Capability::CellClose,
    ] {
        store
            .grant("alpha", "designer", capability)
            .expect("designer grant");
    }
    store
        .grant("alpha", "reader", Capability::CellRead)
        .expect("reader grant");
}

#[test]
fn optimistic_idempotent_and_workspace_policy_is_enforced() {
    let store = Store::memory().expect("store");
    seed(&store);
    let created = store
        .create_draft(
            "alpha",
            "designer",
            "cell-a",
            &empty_cell("cell-a"),
            "create-1",
        )
        .expect("create");
    assert_eq!(created.version, 1);
    assert_eq!(
        store
            .create_draft(
                "alpha",
                "designer",
                "cell-a",
                &empty_cell("cell-a"),
                "create-1"
            )
            .expect("idempotent replay"),
        created
    );
    assert!(matches!(
        store.create_draft(
            "alpha",
            "designer",
            "different",
            &empty_cell("different"),
            "create-1"
        ),
        Err(StoreError::IdempotencyConflict { .. })
    ));
    assert!(matches!(
        store.create_draft(
            "alpha",
            "reader",
            "forbidden",
            &empty_cell("forbidden"),
            "reader-create"
        ),
        Err(StoreError::AccessDenied { .. })
    ));
    assert!(matches!(
        store.read_draft("beta", "reader", "cell-a"),
        Err(StoreError::AccessDenied { .. })
    ));

    let edited = store
        .edit_draft(
            "alpha",
            "designer",
            "cell-a",
            1,
            &empty_cell("cell-a-edited"),
            "edit-1",
        )
        .expect("edit");
    assert_eq!(edited.version, 2);
    assert_eq!(
        store
            .edit_draft(
                "alpha",
                "designer",
                "cell-a",
                1,
                &empty_cell("cell-a-edited"),
                "edit-1"
            )
            .expect("idempotent edit replay"),
        edited
    );
    assert!(matches!(
        store.edit_draft(
            "alpha",
            "designer",
            "cell-a",
            1,
            &empty_cell("stale"),
            "edit-stale"
        ),
        Err(StoreError::StaleDraft { actual: 2, .. })
    ));
}

#[test]
fn publication_keeps_requested_draft_and_persists_effective_configuration() {
    let store = Store::memory().expect("store");
    seed(&store);
    let mut requested = empty_cell("effective-publication");
    requested.components.push(ComponentSpec {
        id: "chain".into(),
        kind: ComponentKind::Bitcoin,
        implementation: "bitcoin-core".into(),
        version: Some("31.1".into()),
        config_version: "bitcoin-core/31/v1".into(),
        control: ControlClass::Cell,
        config: BTreeMap::new(),
    });
    store
        .create_draft(
            "alpha",
            "designer",
            "effective-publication",
            &requested,
            "create-effective-publication",
        )
        .expect("create draft");
    let revision = store
        .publish(
            "alpha",
            "designer",
            "effective-publication",
            1,
            "publish-effective-publication",
        )
        .expect("publish effective revision");
    let draft = store
        .read_draft("alpha", "designer", "effective-publication")
        .expect("read requested draft");

    assert!(draft.cell.components[0].config.is_empty());
    assert_eq!(revision.cell.components[0].config["txindex"], true);
    assert_eq!(revision.cell.components[0].config["fallback_fee"], 0.0002);
    assert_eq!(revision.lock.api_version, LOCK_API_VERSION);
    assert!(
        revision.lock.entries[0]
            .effective_config_digest
            .starts_with("sha256:")
    );
    assert!(
        revision.lock.entries[0]
            .rollout_digest
            .starts_with("sha256:")
    );
}

#[test]
fn revisions_and_grants_survive_reopen() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("proofstorm.sqlite3");
    let (digest, instance) = {
        let store = Store::open(&path).expect("store");
        seed(&store);
        for capability in [
            Capability::ExperimentCreate,
            Capability::CellOperate,
            Capability::ExperimentRead,
            Capability::WalletFund,
            Capability::ArtifactRead,
        ] {
            store
                .grant("alpha", "designer", capability)
                .expect("durable quote grant");
        }
        store
            .create_draft(
                "alpha",
                "designer",
                "durable",
                &empty_cell("durable"),
                "create-durable",
            )
            .expect("create");
        let revision = store
            .publish("alpha", "designer", "durable", 1, "publish-durable")
            .expect("publish");
        let instance = store
            .materialize(
                "alpha",
                "designer",
                "durable-instance",
                &revision.digest,
                "materialize-durable",
            )
            .expect("materialize");
        store
            .create_experiment(
                "alpha",
                "designer",
                "durable-experiment",
                "durable-instance",
                "create-durable-experiment",
            )
            .expect("experiment");
        store
            .start_session(
                "alpha",
                "designer",
                "durable-experiment",
                "durable-session",
                "acquire-durable-session",
            )
            .expect("session");
        (revision.digest, instance)
    };
    let reopened = Store::open(&path).expect("reopen");
    assert!(
        reopened
            .capabilities("alpha", "designer")
            .expect("capabilities")
            .contains(&Capability::CellPublish)
    );
    assert_eq!(
        reopened
            .revision("alpha", "designer", &digest)
            .expect("revision")
            .digest,
        digest
    );
    assert_eq!(
        reopened
            .instance("alpha", "designer", "durable-instance")
            .expect("instance"),
        instance
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one acceptance scenario keeps session admission, sequencing, quota, and artifact bounds visible"
)]
fn operations_admit_large_batches_and_preserve_idempotency_and_terminal_results() {
    let store = Store::memory().expect("store");
    seed(&store);
    for capability in [
        Capability::WalletFund,
        Capability::WalletControl,
        Capability::ArtifactRead,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::CellOperate,
        Capability::ActionCancel,
    ] {
        store
            .grant("alpha", "designer", capability)
            .expect("operation grant");
    }
    store
        .grant("alpha", "reader", Capability::ActionCancel)
        .expect("reader cancellation grant");
    store
        .grant("alpha", "reader", Capability::ArtifactRead)
        .expect("reader artifact grant");
    store
        .create_draft(
            "alpha",
            "designer",
            "operations",
            &empty_cell("operations"),
            "create-operations",
        )
        .expect("create");
    let revision = store
        .publish("alpha", "designer", "operations", 1, "publish-operations")
        .expect("publish");
    store
        .materialize(
            "alpha",
            "designer",
            "operations-instance",
            &revision.digest,
            "materialize-operations",
        )
        .expect("materialize");
    store
        .create_experiment(
            "alpha",
            "designer",
            "operations-experiment",
            "operations-instance",
            "create-operations-experiment",
        )
        .expect("experiment");
    store
        .start_session(
            "alpha",
            "designer",
            "operations-experiment",
            "operations-session",
            "acquire-operations-session",
        )
        .expect("session");
    for index in 0..12 {
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "operations-instance",
                "operations-experiment",
                "operations-session",
                &format!("operation-{index}"),
                OperationKind::BootstrapLiquidity,
                &serde_json::json!({"index": index}),
                &format!("create-operation-{index}"),
                Capability::WalletFund,
            )
            .expect("operation admitted without a per-cell count cap");
        assert_eq!(operation.phase, OperationPhase::Pending);
        assert_eq!(operation.sequence, index + 1);
    }
    let journal = store
        .actions("alpha", "designer", "operations-experiment", 0, 100)
        .expect("journal");
    assert_eq!(
        journal
            .iter()
            .map(|action| action.sequence)
            .collect::<Vec<_>>(),
        (1..=12).collect::<Vec<_>>()
    );
    let replay = store
        .create_operation(
            "alpha",
            "designer",
            "operations-instance",
            "operations-experiment",
            "operations-session",
            "operation-9",
            OperationKind::BootstrapLiquidity,
            &serde_json::json!({"index": 9}),
            "create-operation-9",
            Capability::WalletFund,
        )
        .expect("exact retry with twelve active operations");
    assert_eq!(replay.sequence, 10);
    let completed = store
        .record_operation_result(
            "alpha",
            "operation-0",
            OperationPhase::Succeeded,
            serde_json::json!({"ready": true}),
        )
        .expect("record result");
    assert!(completed.artifact.is_some());
    assert_eq!(
        store
            .active_operations("alpha", "operations-instance")
            .expect("active operations")
            .iter()
            .map(|operation| operation.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "operation-1",
            "operation-2",
            "operation-3",
            "operation-4",
            "operation-5",
            "operation-6",
            "operation-7",
            "operation-8",
            "operation-9",
            "operation-10",
            "operation-11"
        ],
        "a terminal result leaves the ledger's active set"
    );
    assert!(
        store
            .active_operations("alpha", "no-such-instance")
            .expect("unknown instance has no active operations")
            .is_empty()
    );
    assert_eq!(
        store
            .record_operation_result(
                "alpha",
                "operation-0",
                OperationPhase::Cancelled,
                serde_json::json!({"code": "action_cancelled"}),
            )
            .expect("terminal result is monotonic")
            .phase,
        OperationPhase::Succeeded
    );
    assert!(matches!(
        store.operation_for_cancel("alpha", "reader", "operation-1"),
        Err(StoreError::OperationOwnerMismatch { .. })
    ));
    let cancelled = store
        .record_operation_result(
            "alpha",
            "operation-2",
            OperationPhase::Cancelled,
            serde_json::json!({"code": "action_cancelled"}),
        )
        .expect("cancel result");
    assert_eq!(cancelled.phase, OperationPhase::Cancelled);
    assert_eq!(
        store
            .update_operation_phase("alpha", "operation-2", OperationPhase::Running)
            .expect("late running update is ignored")
            .phase,
        OperationPhase::Cancelled
    );
    assert!(matches!(
        store.record_operation_result(
            "alpha",
            "operation-1",
            OperationPhase::Failed,
            serde_json::json!({"oversized": "x".repeat(33 * 1024)}),
        ),
        Err(StoreError::ArtifactTooLarge { .. })
    ));
}

fn quote_observation_store() -> Store {
    let store = Store::memory().expect("store");
    seed(&store);
    for capability in [
        Capability::WalletFund,
        Capability::WalletControl,
        Capability::ArtifactRead,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::CellOperate,
    ] {
        store
            .grant("alpha", "designer", capability)
            .expect("quote observation grant");
    }
    store
        .create_draft(
            "alpha",
            "designer",
            "quote-observations",
            &empty_cell("quote-observations"),
            "create-quote-observations",
        )
        .expect("draft");
    let revision = store
        .publish(
            "alpha",
            "designer",
            "quote-observations",
            1,
            "publish-quote-observations",
        )
        .expect("publish");
    store
        .materialize(
            "alpha",
            "designer",
            "quote-observation-instance",
            &revision.digest,
            "materialize-quote-observations",
        )
        .expect("materialize");
    store
        .create_experiment(
            "alpha",
            "designer",
            "quote-observation-experiment",
            "quote-observation-instance",
            "create-quote-observation-experiment",
        )
        .expect("experiment");
    store
        .start_session(
            "alpha",
            "designer",
            "quote-observation-experiment",
            "quote-observation-session",
            "acquire-quote-observation-session",
        )
        .expect("session");
    store
}

fn quote_operation(store: &Store, id: &str, kind: OperationKind, key: &str) {
    let capability = match kind {
        OperationKind::WalletInvoice => Capability::WalletFund,
        OperationKind::WalletPay | OperationKind::WalletQuoteClaim => Capability::WalletControl,
        _ => panic!("quote fixture received a non-quote operation"),
    };
    store
        .create_operation(
            "alpha",
            "designer",
            "quote-observation-instance",
            "quote-observation-experiment",
            "quote-observation-session",
            id,
            kind,
            &serde_json::json!({"fixture": id}),
            key,
            capability,
        )
        .expect("quote operation");
}

#[test]
fn historical_wallet_receipts_remain_readable_and_immutable() {
    let store = quote_observation_store();
    for (id, kind) in [
        ("old-invoice", OperationKind::WalletInvoice),
        ("old-pay", OperationKind::WalletPay),
        ("old-claim", OperationKind::WalletQuoteClaim),
    ] {
        quote_operation(&store, id, kind, id);
        let artifact = serde_json::json!({"quote_observations":[{"role":"payment_melt","state":"PAID","quote_id":"historical"}]});
        let recorded = store
            .record_operation_result("alpha", id, OperationPhase::Succeeded, artifact.clone())
            .unwrap();
        assert_eq!(recorded.artifact.as_ref().unwrap().content, artifact);
        assert_eq!(store.operation("alpha", "designer", id).unwrap(), recorded);
        assert_eq!(
            store
                .record_operation_result(
                    "alpha",
                    id,
                    OperationPhase::Failed,
                    serde_json::json!({"changed":true})
                )
                .unwrap(),
            recorded
        );
    }
}

#[test]
fn opening_store_does_not_create_or_rewrite_retired_wallet_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite3");
    drop(Store::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    for table in ["wallet_payment_claims", "wallet_quote_observations"] {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists);
        db.execute_batch(&format!("CREATE TABLE {table}(workspace_id TEXT, instance_id TEXT, legacy TEXT); INSERT INTO {table} VALUES ('workspace','instance','preserve');")).unwrap();
    }
    drop(db);
    drop(Store::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    for table in ["wallet_payment_claims", "wallet_quote_observations"] {
        assert_eq!(
            db.query_row(&format!("SELECT legacy FROM {table}"), [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "preserve"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one lifecycle acceptance test keeps the conflicting session and close sequence visible"
)]
fn overlapping_sessions_do_not_block_control_or_close() {
    let store = Store::memory().expect("store");
    seed(&store);
    for principal in ["designer", "reader"] {
        for capability in [
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::ExperimentClose,
            Capability::CellOperate,
            Capability::ExperimentRead,
        ] {
            store
                .grant("alpha", principal, capability)
                .expect("experiment grant");
        }
    }
    store
        .create_draft(
            "alpha",
            "designer",
            "leased",
            &empty_cell("leased"),
            "create-leased",
        )
        .expect("create");
    let revision = store
        .publish("alpha", "designer", "leased", 1, "publish-leased")
        .expect("publish");
    store
        .materialize(
            "alpha",
            "designer",
            "leased-instance",
            &revision.digest,
            "materialize-leased",
        )
        .expect("materialize");
    let experiment = store
        .create_experiment(
            "alpha",
            "designer",
            "experiment-a",
            "leased-instance",
            "create-experiment-a",
        )
        .expect("experiment");
    assert_eq!(
        store
            .create_experiment(
                "alpha",
                "designer",
                "experiment-a",
                "leased-instance",
                "create-experiment-a",
            )
            .expect("idempotent experiment"),
        experiment
    );
    let session = store
        .start_session(
            "alpha",
            "designer",
            "experiment-a",
            "session-a",
            "acquire-session-a",
        )
        .expect("session");
    assert_eq!(session.phase, proofstorm_core::SessionPhase::Active);
    store
        .instance_for_close("alpha", "designer", "leased-instance")
        .unwrap();
    store
        .create_experiment(
            "alpha",
            "reader",
            "experiment-b",
            "leased-instance",
            "create-experiment-b",
        )
        .expect("second experiment");
    store
        .start_session("alpha", "reader", "experiment-b", "session-b", "start-b")
        .unwrap();
    let overlap = store
        .overlapping_sessions("alpha", "reader", "session-a", "", 20)
        .unwrap();
    assert_eq!(overlap.sessions.len(), 1);
    assert_eq!(overlap.sessions[0].id, "session-b");
    let released = store
        .finish_session("alpha", "designer", "session-a", "release-session-a")
        .expect("release");
    assert_eq!(released.phase, proofstorm_core::SessionPhase::Finished);
    let closed = store
        .close_experiment("alpha", "designer", "experiment-a", "close-experiment-a")
        .expect("close experiment");
    assert_eq!(closed.phase, proofstorm_core::ExperimentPhase::Closed);
    store
        .instance_for_close("alpha", "designer", "leased-instance")
        .expect("unleased instance closes");
}

#[test]
fn simultaneous_identical_admission_across_connections_keeps_one_operation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.sqlite3");
    let store = Store::open(&path).unwrap();
    seed(&store);
    for capability in [
        Capability::CellOperate,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
        Capability::ExperimentRead,
    ] {
        store.grant("alpha", "designer", capability).unwrap();
    }
    let spec = serde_json::from_value(serde_json::json!({
        "api_version":"proofstorm/v1alpha1","name":"race","links":[],
        "components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]
    })).unwrap();
    store
        .create_draft("alpha", "designer", "race", &spec, "draft")
        .unwrap();
    let revision = store
        .publish("alpha", "designer", "race", 1, "publish")
        .unwrap();
    store
        .materialize("alpha", "designer", "race", &revision.digest, "apply")
        .unwrap();
    let request = serde_json::json!({"component":"chain","argv":["true"],"script":"","timeout_seconds":25,"output":{"mode":"public","fields":[]}});
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers = (0..8)
        .map(|_| {
            let store = Store::open(&path).unwrap();
            let barrier = barrier.clone();
            let request = request.clone();
            std::thread::spawn(move || {
                (0..8)
                    .map(|round| {
                        let id = format!("race-{round}");
                        barrier.wait();
                        store.create_operation(
                            "alpha",
                            "designer",
                            "race",
                            "",
                            "",
                            &id,
                            OperationKind::ComponentExecLive,
                            &request,
                            &id,
                            Capability::ComponentExecLive,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    for round in 0..8 {
        let first = results[0][round].as_ref().unwrap();
        for results in &results {
            assert_eq!(results[round].as_ref().unwrap(), first);
        }
    }
    let changed = serde_json::json!({"component":"chain","argv":["false"]});
    assert!(matches!(
        store.create_operation(
            "alpha",
            "designer",
            "race",
            "",
            "",
            "race-0",
            OperationKind::ComponentExecLive,
            &changed,
            "race-0",
            Capability::ComponentExecLive
        ),
        Err(StoreError::IdempotencyConflict { .. })
    ));
}
