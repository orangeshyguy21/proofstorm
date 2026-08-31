use std::collections::BTreeMap;

use proofstorm_core::{
    API_VERSION, Capability, ComponentKind, ComponentSpec, ControlClass, DraftMutation, LabPolicy,
    LabSpec, OperationKind, OperationPhase, WalletQuoteDirection, WalletQuotePhase,
};
use proofstorm_store::{Store, StoreError, Workspace};

fn empty_lab(name: &str) -> LabSpec {
    LabSpec {
        api_version: API_VERSION.into(),
        name: name.into(),
        components: vec![],
        links: vec![],
        policy: LabPolicy::default(),
    }
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
        Capability::LabRead,
        Capability::LabCreate,
        Capability::LabEdit,
        Capability::LabClone,
        Capability::LabValidate,
        Capability::LabPublish,
        Capability::LabMaterialize,
        Capability::LabStatus,
        Capability::LabClose,
    ] {
        store
            .grant("alpha", "designer", capability)
            .expect("designer grant");
    }
    store
        .grant("alpha", "reader", Capability::LabRead)
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
            "lab-a",
            &empty_lab("lab-a"),
            "create-1",
        )
        .expect("create");
    assert_eq!(created.version, 1);
    assert_eq!(
        store
            .create_draft(
                "alpha",
                "designer",
                "lab-a",
                &empty_lab("lab-a"),
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
            &empty_lab("different"),
            "create-1"
        ),
        Err(StoreError::IdempotencyConflict { .. })
    ));
    assert!(matches!(
        store.create_draft(
            "alpha",
            "reader",
            "forbidden",
            &empty_lab("forbidden"),
            "reader-create"
        ),
        Err(StoreError::AccessDenied { .. })
    ));
    assert!(matches!(
        store.read_draft("beta", "reader", "lab-a"),
        Err(StoreError::AccessDenied { .. })
    ));

    let edited = store
        .edit_draft(
            "alpha",
            "designer",
            "lab-a",
            1,
            &empty_lab("lab-a-edited"),
            "edit-1",
        )
        .expect("edit");
    assert_eq!(edited.version, 2);
    assert_eq!(
        store
            .edit_draft(
                "alpha",
                "designer",
                "lab-a",
                1,
                &empty_lab("lab-a-edited"),
                "edit-1"
            )
            .expect("idempotent edit replay"),
        edited
    );
    assert!(matches!(
        store.edit_draft(
            "alpha",
            "designer",
            "lab-a",
            1,
            &empty_lab("stale"),
            "edit-stale"
        ),
        Err(StoreError::StaleDraft { actual: 2, .. })
    ));
}

#[test]
fn composer_mutations_are_idempotent_and_optimistic() {
    let store = Store::memory().expect("store");
    seed(&store);
    store
        .create_draft(
            "alpha",
            "designer",
            "composed",
            &empty_lab("composed"),
            "create-composed",
        )
        .expect("create");

    let mutation = DraftMutation::AddComponent {
        component: ComponentSpec {
            id: "chain".into(),
            kind: ComponentKind::Bitcoin,
            implementation: "bitcoin-core".into(),
            version: Some("30.0".into()),
            config_version: "v1alpha1".into(),
            control: ControlClass::Laboratory,
            config: BTreeMap::new(),
        },
    };
    let composed = store
        .mutate_draft("alpha", "designer", "composed", 1, &mutation, "add-chain")
        .expect("component mutation");
    assert_eq!(composed.version, 2);
    assert_eq!(
        store
            .mutate_draft("alpha", "designer", "composed", 1, &mutation, "add-chain",)
            .expect("idempotent mutation replay"),
        composed
    );
    assert!(matches!(
        store.mutate_draft(
            "alpha",
            "designer",
            "composed",
            1,
            &DraftMutation::RemoveComponent {
                component_id: "chain".into(),
            },
            "stale-remove-chain",
        ),
        Err(StoreError::StaleDraft { actual: 2, .. })
    ));
}

#[test]
fn revisions_and_grants_survive_reopen() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("proofstorm.sqlite3");
    let (digest, instance, quote) = {
        let store = Store::open(&path).expect("store");
        seed(&store);
        for capability in [
            Capability::ExperimentCreate,
            Capability::LeaseAcquire,
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
                &empty_lab("durable"),
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
            .acquire_lease(
                "alpha",
                "designer",
                "durable-experiment",
                "durable-lease",
                300,
                1,
                "acquire-durable-lease",
            )
            .expect("lease");
        let quote = store
            .create_wallet_quote(
                "alpha",
                "designer",
                "durable-instance",
                "durable-experiment",
                "durable-lease",
                "durable-quote",
                "wallet",
                "mint",
                WalletQuoteDirection::Receive,
                100,
                300,
                "create-durable-quote",
            )
            .expect("quote");
        (revision.digest, instance, quote)
    };
    let reopened = Store::open(&path).expect("reopen");
    assert!(
        reopened
            .capabilities("alpha", "designer")
            .expect("capabilities")
            .contains(&Capability::LabPublish)
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
    assert_eq!(
        reopened
            .wallet_quote("alpha", "designer", "durable-quote")
            .expect("durable quote"),
        quote
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one acceptance scenario keeps lease admission, sequencing, quota, and artifact bounds visible"
)]
fn operations_are_idempotent_bounded_and_artifacts_are_capped() {
    let store = Store::memory().expect("store");
    seed(&store);
    for capability in [
        Capability::WalletFund,
        Capability::WalletControl,
        Capability::ArtifactRead,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::LeaseAcquire,
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
            &empty_lab("operations"),
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
        .acquire_lease(
            "alpha",
            "designer",
            "operations-experiment",
            "operations-lease",
            300,
            10,
            "acquire-operations-lease",
        )
        .expect("lease");
    let quote = store
        .create_wallet_quote(
            "alpha",
            "designer",
            "operations-instance",
            "operations-experiment",
            "operations-lease",
            "receive-quote",
            "wallet",
            "mint",
            WalletQuoteDirection::Receive,
            1_000,
            300,
            "create-receive-quote",
        )
        .expect("quote");
    assert_eq!(quote.phase, WalletQuotePhase::Requested);
    assert_eq!(
        store
            .create_wallet_quote(
                "alpha",
                "designer",
                "operations-instance",
                "operations-experiment",
                "operations-lease",
                "receive-quote",
                "wallet",
                "mint",
                WalletQuoteDirection::Receive,
                1_000,
                300,
                "create-receive-quote",
            )
            .expect("idempotent quote"),
        quote
    );
    let public_quote = serde_json::to_value(&quote).expect("serialize quote");
    for forbidden in ["invoice", "adapter_quote", "payment_request", "secret"] {
        assert!(
            !public_quote
                .as_object()
                .expect("quote object")
                .contains_key(forbidden)
        );
    }
    let ready = store
        .transition_wallet_quote(
            "alpha",
            "receive-quote",
            WalletQuotePhase::Requested,
            WalletQuotePhase::Ready,
            Some("invoice-operation"),
            None,
        )
        .expect("ready quote");
    assert_eq!(ready.operation_id.as_deref(), Some("invoice-operation"));
    store
        .transition_wallet_quote(
            "alpha",
            "receive-quote",
            WalletQuotePhase::Ready,
            WalletQuotePhase::Pending,
            Some("invoice-operation"),
            None,
        )
        .expect("pending quote");
    store
        .transition_wallet_quote(
            "alpha",
            "receive-quote",
            WalletQuotePhase::Pending,
            WalletQuotePhase::Paid,
            Some("invoice-operation"),
            None,
        )
        .expect("paid quote");
    let settled = store
        .transition_wallet_quote(
            "alpha",
            "receive-quote",
            WalletQuotePhase::Paid,
            WalletQuotePhase::Settled,
            Some("invoice-operation"),
            None,
        )
        .expect("settled quote");
    assert!(settled.settled_at_unix.is_some());
    assert!(matches!(
        store.transition_wallet_quote(
            "alpha",
            "receive-quote",
            WalletQuotePhase::Settled,
            WalletQuotePhase::Ready,
            Some("invoice-operation"),
            None,
        ),
        Err(StoreError::InvalidQuoteTransition { .. })
    ));
    assert_eq!(
        store
            .wallet_quote("alpha", "designer", "receive-quote")
            .expect("read quote")
            .phase,
        WalletQuotePhase::Settled
    );
    assert_eq!(
        store
            .wallet_quotes("alpha", "designer", "operations-experiment", None, 10)
            .expect("list quotes")
            .len(),
        1
    );
    assert!(matches!(
        store.wallet_quote("alpha", "reader", "receive-quote"),
        Err(StoreError::QuoteOwnerMismatch { .. })
    ));
    store
        .create_wallet_quote(
            "alpha",
            "designer",
            "operations-instance",
            "operations-experiment",
            "operations-lease",
            "recovery-quote",
            "wallet",
            "mint",
            WalletQuoteDirection::Receive,
            100,
            300,
            "create-recovery-quote",
        )
        .expect("recovery quote");
    let inconclusive = store
        .transition_wallet_quote(
            "alpha",
            "recovery-quote",
            WalletQuotePhase::Requested,
            WalletQuotePhase::Inconclusive,
            Some("recovery-invoice"),
            Some("terminal_artifact_missing"),
        )
        .expect("quarantine ambiguous quote");
    assert_eq!(
        inconclusive.terminal_code.as_deref(),
        Some("terminal_artifact_missing")
    );
    let recovered = store
        .transition_wallet_quote(
            "alpha",
            "recovery-quote",
            WalletQuotePhase::Inconclusive,
            WalletQuotePhase::Settled,
            Some("recovery-invoice"),
            None,
        )
        .expect("authoritative settlement repairs ambiguous quote");
    assert!(recovered.settled_at_unix.is_some());
    assert!(recovered.terminal_code.is_none());
    for index in 0..4 {
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "operations-instance",
                "operations-experiment",
                "operations-lease",
                &format!("operation-{index}"),
                OperationKind::BootstrapLiquidity,
                &serde_json::json!({"index": index}),
                &format!("create-operation-{index}"),
                Capability::WalletFund,
            )
            .expect("bounded operation");
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
        vec![1, 2, 3, 4]
    );
    assert!(matches!(
        store.create_operation(
            "alpha",
            "designer",
            "operations-instance",
            "operations-experiment",
            "operations-lease",
            "operation-five",
            OperationKind::BootstrapLiquidity,
            &serde_json::json!({"index": 5}),
            "create-operation-five",
            Capability::WalletFund,
        ),
        Err(StoreError::OperationLimit { maximum: 4, .. })
    ));
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one lifecycle acceptance test keeps the conflicting lease and close sequence visible"
)]
fn exclusive_leases_block_conflicting_control_and_close() {
    let store = Store::memory().expect("store");
    seed(&store);
    for principal in ["designer", "reader"] {
        for capability in [
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::ExperimentClose,
            Capability::LeaseAcquire,
            Capability::LeaseRelease,
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
            &empty_lab("leased"),
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
    let lease = store
        .acquire_lease(
            "alpha",
            "designer",
            "experiment-a",
            "lease-a",
            300,
            10,
            "acquire-lease-a",
        )
        .expect("lease");
    assert_eq!(lease.phase, proofstorm_core::LeasePhase::Active);
    assert!(matches!(
        store.instance_for_close("alpha", "designer", "leased-instance"),
        Err(StoreError::InstanceLeased { .. })
    ));
    assert!(matches!(
        store.close_experiment("alpha", "designer", "experiment-a", "close-leased"),
        Err(StoreError::ExperimentLeased { .. })
    ));
    store
        .create_experiment(
            "alpha",
            "reader",
            "experiment-b",
            "leased-instance",
            "create-experiment-b",
        )
        .expect("second experiment");
    assert!(matches!(
        store.acquire_lease(
            "alpha",
            "reader",
            "experiment-b",
            "lease-b",
            300,
            10,
            "acquire-lease-b",
        ),
        Err(StoreError::InstanceLeased { .. })
    ));
    let released = store
        .release_lease("alpha", "designer", "lease-a", "release-lease-a")
        .expect("release");
    assert_eq!(released.phase, proofstorm_core::LeasePhase::Released);
    let closed = store
        .close_experiment("alpha", "designer", "experiment-a", "close-experiment-a")
        .expect("close experiment");
    assert_eq!(closed.phase, proofstorm_core::ExperimentPhase::Closed);
    store
        .instance_for_close("alpha", "designer", "leased-instance")
        .expect("unleased instance closes");
}
