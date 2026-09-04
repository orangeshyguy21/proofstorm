use std::collections::BTreeMap;

use proofstorm_core::{
    API_VERSION, Capability, ComponentKind, ComponentSpec, ControlClass, DraftMutation,
    LOCK_API_VERSION, LabPolicy, LabSpec, OperationKind, OperationPhase, WalletQuoteDirection,
    WalletQuoteObservationInput, WalletQuoteObservationRole,
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
            config_version: "bitcoin-core/30/v1".into(),
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
fn publication_keeps_requested_draft_and_persists_effective_configuration() {
    let store = Store::memory().expect("store");
    seed(&store);
    let mut requested = empty_lab("effective-publication");
    requested.components.push(ComponentSpec {
        id: "chain".into(),
        kind: ComponentKind::Bitcoin,
        implementation: "bitcoin-core".into(),
        version: Some("30.0".into()),
        config_version: "bitcoin-core/30/v1".into(),
        control: ControlClass::Laboratory,
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

    assert!(draft.lab.components[0].config.is_empty());
    assert_eq!(revision.lab.components[0].config["txindex"], true);
    assert_eq!(revision.lab.components[0].config["fallback_fee"], 0.0002);
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
        (revision.digest, instance)
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
    for index in 0..8 {
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
        vec![1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert!(matches!(
        store.create_operation(
            "alpha",
            "designer",
            "operations-instance",
            "operations-experiment",
            "operations-lease",
            "operation-nine",
            OperationKind::BootstrapLiquidity,
            &serde_json::json!({"index": 9}),
            "create-operation-nine",
            Capability::WalletFund,
        ),
        Err(StoreError::OperationLimit { maximum: 8, .. })
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
            "operation-7"
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
        Capability::LeaseAcquire,
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
            &empty_lab("quote-observations"),
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
        .acquire_lease(
            "alpha",
            "designer",
            "quote-observation-experiment",
            "quote-observation-lease",
            300,
            10,
            "acquire-quote-observation-lease",
        )
        .expect("lease");
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
            "quote-observation-lease",
            id,
            kind,
            &serde_json::json!({"fixture": id}),
            key,
            capability,
        )
        .expect("quote operation");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the atomic terminalization, immutable replay, and latest-observation assertions form one store contract"
)]
fn quote_observations_are_atomic_immutable_and_latest_by_sequence() {
    let store = quote_observation_store();
    quote_operation(
        &store,
        "invoice-observation",
        OperationKind::WalletInvoice,
        "create-invoice-observation",
    );
    let unpaid = WalletQuoteObservationInput {
        role: WalletQuoteObservationRole::InvoiceReceive,
        wallet_id: "recipient-wallet".into(),
        mint_id: "recipient-mint".into(),
        direction: WalletQuoteDirection::Receive,
        quote_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
        amount_sat: 100,
        state: "UNPAID".into(),
        wallet_created_at_unix: Some(1),
        wallet_paid_at_unix: None,
        wallet_expires_at_unix: Some(301),
        fee_reserve_sat: None,
        fee_paid_sat: None,
    };
    store
        .record_operation_result_with_quote_observations(
            "alpha",
            "invoice-observation",
            OperationPhase::Succeeded,
            serde_json::json!({"state": "UNPAID"}),
            std::slice::from_ref(&unpaid),
        )
        .expect("terminal result and observation");
    let first = store
        .wallet_quote_observation(
            "alpha",
            "designer",
            "quote-observation-instance",
            "recipient-wallet",
            "recipient-mint",
            WalletQuoteDirection::Receive,
            &unpaid.quote_id,
        )
        .expect("first observation");
    assert_eq!(first.state, "UNPAID");
    assert_eq!(first.observed_by_operation, "invoice-observation");
    assert!(matches!(
        store.wallet_quote_observation(
            "alpha",
            "reader",
            "quote-observation-instance",
            "recipient-wallet",
            "recipient-mint",
            WalletQuoteDirection::Receive,
            &unpaid.quote_id,
        ),
        Err(StoreError::AccessDenied { .. })
    ));

    let replay = WalletQuoteObservationInput {
        state: "ISSUED".into(),
        ..unpaid.clone()
    };
    store
        .record_operation_result_with_quote_observations(
            "alpha",
            "invoice-observation",
            OperationPhase::Failed,
            serde_json::json!({"state": "ISSUED"}),
            &[replay],
        )
        .expect("terminal replay is immutable");
    assert_eq!(
        store
            .wallet_quote_observation(
                "alpha",
                "designer",
                "quote-observation-instance",
                "recipient-wallet",
                "recipient-mint",
                WalletQuoteDirection::Receive,
                &unpaid.quote_id,
            )
            .expect("unchanged observation"),
        first
    );

    quote_operation(
        &store,
        "claim-observation",
        OperationKind::WalletQuoteClaim,
        "create-claim-observation",
    );
    let issued = WalletQuoteObservationInput {
        role: WalletQuoteObservationRole::ClaimReceive,
        state: "ISSUED".into(),
        wallet_paid_at_unix: Some(2),
        ..unpaid
    };
    store
        .record_operation_result_with_quote_observations(
            "alpha",
            "claim-observation",
            OperationPhase::Succeeded,
            serde_json::json!({"state": "ISSUED"}),
            &[issued],
        )
        .expect("new observation");
    let latest = store
        .wallet_quote_observation(
            "alpha",
            "designer",
            "quote-observation-instance",
            "recipient-wallet",
            "recipient-mint",
            WalletQuoteDirection::Receive,
            "01234567-89ab-cdef-0123-456789abcdef",
        )
        .expect("latest observation");
    assert_eq!(latest.state, "ISSUED");
    assert!(latest.observation_sequence > first.observation_sequence);
    let snapshot = store
        .wallet_quote_observation_max_sequence("alpha", "designer", "quote-observation-experiment")
        .expect("observation snapshot");
    let listed = store
        .wallet_quote_observations(
            "alpha",
            "designer",
            "quote-observation-experiment",
            0,
            snapshot,
            10,
        )
        .expect("latest observations");
    assert_eq!(listed, vec![latest]);
}

#[test]
fn invalid_observation_cannot_terminalize_its_operation() {
    let store = quote_observation_store();
    quote_operation(
        &store,
        "invalid-observation",
        OperationKind::WalletInvoice,
        "create-invalid-observation",
    );
    let invalid = WalletQuoteObservationInput {
        role: WalletQuoteObservationRole::PaymentMelt,
        wallet_id: "recipient-wallet".into(),
        mint_id: "recipient-mint".into(),
        direction: WalletQuoteDirection::Receive,
        quote_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
        amount_sat: 100,
        state: "UNPAID".into(),
        wallet_created_at_unix: None,
        wallet_paid_at_unix: None,
        wallet_expires_at_unix: None,
        fee_reserve_sat: None,
        fee_paid_sat: None,
    };
    assert!(matches!(
        store.record_operation_result_with_quote_observations(
            "alpha",
            "invalid-observation",
            OperationPhase::Succeeded,
            serde_json::json!({"state": "UNPAID"}),
            &[invalid],
        ),
        Err(StoreError::Validation(_))
    ));
    assert_eq!(
        store
            .operation("alpha", "designer", "invalid-observation")
            .expect("operation remains readable")
            .phase,
        OperationPhase::Pending
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "atomic identity rollback and concurrent single-flight admission are one payment-claim contract"
)]
fn payment_claims_are_idempotent_and_single_flight() {
    let store = quote_observation_store();
    let admit = |store: &Store, operation: &str, quote: &str| {
        store.create_wallet_pay_operation(
            "alpha",
            "designer",
            "quote-observation-instance",
            "quote-observation-experiment",
            "quote-observation-lease",
            operation,
            &serde_json::json!({"mint_quote_id": quote}),
            &format!("create-{operation}"),
            "recipient-wallet",
            "recipient-mint",
            quote,
            "payer-wallet",
            "payer-mint",
        )
    };
    let first = admit(
        &store,
        "payment-one",
        "01234567-89ab-cdef-0123-456789abcdef",
    )
    .expect("first atomic payment admission");
    assert_eq!(
        admit(
            &store,
            "payment-one",
            "01234567-89ab-cdef-0123-456789abcdef"
        )
        .expect("idempotent payment admission"),
        first
    );
    assert!(matches!(
        admit(&store, "payment-two", "01234567-89ab-cdef-0123-456789abcdef"),
        Err(StoreError::QuotePaymentAlreadyClaimed { operation, .. })
            if operation == "payment-one"
    ));
    assert!(matches!(
        store.operation("alpha", "designer", "payment-two"),
        Err(StoreError::NotFound { .. })
    ));
    quote_operation(
        &store,
        "identity-conflict",
        OperationKind::WalletInvoice,
        "seed-identity-conflict",
    );
    assert!(matches!(
        admit(
            &store,
            "identity-conflict",
            "bbbbbbbb-cccc-dddd-eeee-ffffffffffff"
        ),
        Err(StoreError::Conflict { .. })
    ));
    admit(
        &store,
        "payment-five",
        "bbbbbbbb-cccc-dddd-eeee-ffffffffffff",
    )
    .expect("conflicting operation identity did not retain a payment claim");
    for operation in ["payment-one", "identity-conflict", "payment-five"] {
        store
            .record_operation_result(
                "alpha",
                operation,
                OperationPhase::Succeeded,
                serde_json::json!({"fixture": true}),
            )
            .expect("finish setup operation");
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = ["payment-three", "payment-four"].map(|operation| {
        let store = store.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.create_wallet_pay_operation(
                "alpha",
                "designer",
                "quote-observation-instance",
                "quote-observation-experiment",
                "quote-observation-lease",
                operation,
                &serde_json::json!({"mint_quote_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}),
                &format!("create-{operation}"),
                "recipient-wallet",
                "recipient-mint",
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "payer-wallet",
                "payer-mint",
            )
        })
    });
    let results = handles.map(|handle| handle.join().expect("payment claim thread"));
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::QuotePaymentAlreadyClaimed { .. })))
            .count(),
        1,
        "{results:?}"
    );
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
