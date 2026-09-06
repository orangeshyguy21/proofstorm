//! One-level recipient leases under an existing exclusive lab lease.
use super::{
    Capability, Connection, ExperimentLease, LabOperation, LeasePhase, Store, StoreError,
    TransactionBehavior, expire_leases, is_slug, now_unix, params, validate_lease_request,
};
use proofstorm_core::{ComponentKind, PrivateTransferLeaseScope};

pub(super) fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists = {
        let mut statement = tx.prepare("PRAGMA table_info(experiment_leases)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == "delegation_json")
    };
    if !exists {
        tx.execute_batch("ALTER TABLE experiment_leases ADD COLUMN delegation_json TEXT;")?;
    }
    tx.execute_batch(
        "DROP INDEX IF EXISTS one_active_lease_per_instance;
        CREATE UNIQUE INDEX one_active_lease_per_instance
        ON experiment_leases(workspace_id, instance_id)
        WHERE phase_json = '\"active\"' AND delegation_json IS NULL;",
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn parent_action_count(
    db: &Connection,
    workspace: &str,
    parent: &str,
) -> Result<u32, StoreError> {
    Ok(db.query_row(
        "SELECT COUNT(*) FROM actions WHERE workspace_id=?1 AND
        (lease_id=?2 OR lease_id IN (SELECT id FROM experiment_leases WHERE workspace_id=?1
        AND json_extract(delegation_json,'$.parent_lease_id')=?2))",
        params![workspace, parent],
        |r| r.get(0),
    )?)
}

impl Store {
    /// Resolve the immutable scope for an already-journaled operation before runtime dispatch.
    pub fn operation_lease_scope(
        &self,
        operation: &LabOperation,
    ) -> Result<Option<PrivateTransferLeaseScope>, StoreError> {
        let lease = self.lease_unchecked(&operation.workspace_id, &operation.lease_id)?;
        if lease.principal_id != operation.principal_id
            || lease.instance_id != operation.instance_id
            || lease.experiment_id != operation.experiment_id
        {
            return Err(StoreError::Validation(
                "operation lease identity mismatch".into(),
            ));
        }
        Ok(lease.delegation)
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "recipient identity, immutable scope and both budgets are checked at one authority boundary"
    )]
    pub fn delegate_private_transfer_lease(
        &self,
        workspace: &str,
        principal: &str,
        recipient: &str,
        lease_id: &str,
        scope: &PrivateTransferLeaseScope,
        duration_seconds: u32,
        max_actions: u32,
        idempotency_key: &str,
    ) -> Result<ExperimentLease, StoreError> {
        self.authorize(workspace, principal, Capability::LeaseAcquire)?;
        self.authorize(workspace, principal, Capability::ComponentExecLive)?;
        self.authorize(workspace, recipient, Capability::ComponentExecLive)?;
        validate_lease_request(lease_id, duration_seconds, max_actions)?;
        if !scope
            .receive_command_digest
            .strip_prefix("sha256:")
            .is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            return Err(StoreError::Validation(
                "recipient scope requires an approved receive command digest".into(),
            ));
        }
        if recipient == principal
            || recipient.is_empty()
            || recipient.len() > 128
            || !is_slug(&scope.component)
            || !is_slug(&scope.mint)
            || scope.reference.is_empty()
            || scope.reference.len() > 128
            || !scope
                .reference
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || max_actions > 32
            || duration_seconds > 900
        {
            return Err(StoreError::Validation("recipient scope must name another principal, one wallet/mint/reference, at most 32 actions and 900 seconds".into()));
        }
        let parent = self.refresh_lease(workspace, &scope.parent_lease_id)?;
        if parent.principal_id != principal
            || parent.delegation.is_some()
            || parent.phase != LeasePhase::Active
        {
            return Err(StoreError::Validation(
                "only the active root lease owner may delegate recipient access".into(),
            ));
        }
        let (_, revision) = self.operation_context(
            workspace,
            principal,
            &parent.instance_id,
            Capability::LeaseAcquire,
        )?;
        for (id, kind) in [
            (&scope.component, ComponentKind::Wallet),
            (&scope.mint, ComponentKind::Mint),
        ] {
            if !revision
                .lab
                .components
                .iter()
                .any(|c| &c.id == id && c.kind == kind)
            {
                return Err(StoreError::Validation(
                    "recipient scope endpoints must be a wallet and mint in this lab".into(),
                ));
            }
        }
        let request = serde_json::json!({"recipient":recipient,"lease_id":lease_id,"scope":scope,
            "duration_seconds":duration_seconds,"max_actions":max_actions});
        if let Some(previous) = self.idempotent_response::<ExperimentLease, _>(
            workspace,
            principal,
            idempotency_key,
            "lease.delegate",
            &request,
        )? {
            let current = self.refresh_lease(workspace, &previous.id)?;
            if current.phase != LeasePhase::Active {
                return Err(StoreError::LeaseInactive { lease: current.id });
            }
            return Ok(current);
        }
        let acquired_at = now_unix();
        if acquired_at + i64::from(duration_seconds) > parent.expires_at_unix {
            return Err(StoreError::Validation(
                "recipient deadline must not exceed the parent lease deadline".into(),
            ));
        }
        let lease = ExperimentLease {
            delegation: Some(scope.clone()),
            id: lease_id.into(),
            workspace_id: workspace.into(),
            experiment_id: parent.experiment_id.clone(),
            instance_id: parent.instance_id.clone(),
            principal_id: recipient.into(),
            phase: LeasePhase::Active,
            acquired_at_unix: acquired_at,
            expires_at_unix: acquired_at + i64::from(duration_seconds),
            max_actions,
            released_at_unix: None,
        };
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_leases(&tx, workspace, acquired_at)?;
        let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM experiment_leases WHERE workspace_id=?1 AND id=?2
            AND principal_id=?3 AND phase_json='\"active\"' AND delegation_json IS NULL AND expires_at>=?4)",
            params![workspace,parent.id,principal,lease.expires_at_unix], |r| r.get(0))?;
        if !active {
            return Err(StoreError::LeaseInactive { lease: parent.id });
        }
        let (total, active_children): (u32,u32) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(phase_json='\"active\"'),0)
            FROM experiment_leases WHERE workspace_id=?1 AND json_extract(delegation_json,'$.parent_lease_id')=?2",
            params![workspace,parent.id], |r| Ok((r.get(0)?,r.get(1)?)))?;
        if total >= 32 || active_children >= 8 {
            return Err(StoreError::Validation(
                "parent recipient lease limit reached".into(),
            ));
        }
        let used = parent_action_count(&tx, workspace, &parent.id)?;
        if max_actions > parent.max_actions.saturating_sub(used) {
            return Err(StoreError::ActionBudgetExceeded {
                lease: parent.id,
                maximum: parent.max_actions,
            });
        }
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM experiment_leases WHERE workspace_id=?1 AND id=?2)",
            params![workspace, lease_id],
            |r| r.get(0),
        )?;
        if existing {
            return Err(StoreError::Conflict {
                resource: "experiment lease",
                id: lease_id.into(),
            });
        }
        tx.execute("INSERT INTO experiment_leases(workspace_id,id,experiment_id,instance_id,principal_id,phase_json,
            acquired_at,expires_at,max_actions,delegation_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![workspace,lease_id,parent.experiment_id,parent.instance_id,recipient,
                serde_json::to_string(&LeasePhase::Active)?,acquired_at,lease.expires_at_unix,max_actions,serde_json::to_string(scope)?])?;
        tx.commit()?;
        drop(connection);
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lease.delegate",
            &request,
            &lease,
        )?;
        Ok(lease)
    }

    /// Check release authority before any runtime annotation is changed.
    pub fn lease_for_release(
        &self,
        workspace: &str,
        principal: &str,
        lease_id: &str,
    ) -> Result<ExperimentLease, StoreError> {
        self.authorize(workspace, principal, Capability::LeaseRelease)?;
        let lease = self.refresh_lease(workspace, lease_id)?;
        let owns_parent = if let Some(scope) = &lease.delegation {
            self.lease_unchecked(workspace, &scope.parent_lease_id)?
                .principal_id
                == principal
        } else {
            false
        };
        if lease.principal_id != principal && !owns_parent {
            return Err(StoreError::LeaseOwnerMismatch {
                lease: lease_id.into(),
                owner: lease.principal_id,
                principal: principal.into(),
            });
        }
        Ok(lease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use proofstorm_core::{LabSpec, OperationKind, PublishedRevision};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn scope() -> PrivateTransferLeaseScope {
        PrivateTransferLeaseScope {
            receive_command_digest: proofstorm_core::PrivateReceiveCommand {
                script: String::new(),
                argv: vec!["native-receive".into()],
                timeout_seconds: 30,
                input: proofstorm_core::private_io::InputBinding::Stdin,
            }
            .digest(),
            parent_lease_id: "parent".into(),
            component: "wallet-b".into(),
            mint: "mint".into(),
            reference: "payload-ref".into(),
        }
    }
    fn seed(store: &Store, budget: u32) {
        store
            .put_workspace(&Workspace {
                id: "workspace".into(),
                name: "workspace".into(),
            })
            .unwrap();
        for principal in ["sender", "receiver", "stranger"] {
            store.put_principal(principal).unwrap();
            for capability in [
                Capability::LeaseAcquire,
                Capability::LeaseRelease,
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
        store
            .acquire_lease(
                "workspace",
                "sender",
                "experiment",
                "parent",
                600,
                budget,
                "root-acquire",
            )
            .unwrap();
    }
    fn child(store: &Store, budget: u32) -> ExperimentLease {
        store
            .delegate_private_transfer_lease(
                "workspace",
                "sender",
                "receiver",
                "child",
                &scope(),
                300,
                budget,
                "delegate",
            )
            .unwrap()
    }
    fn submit(
        store: &Store,
        principal: &str,
        lease: &str,
        id: &str,
        kind: OperationKind,
        request: &Value,
    ) -> Result<LabOperation, StoreError> {
        let capability =
            if kind == OperationKind::ComponentExecLive || kind == OperationKind::PrivateTransfer {
                Capability::ComponentExecLive
            } else {
                Capability::WalletControl
            };
        store.create_operation(
            "workspace",
            principal,
            "instance",
            "experiment",
            lease,
            id,
            kind,
            request,
            id,
            capability,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "adversarial input matrix followed by accepted control requests"
    )]
    fn recipient_scope_blocks_binding_substitution_and_nested_authority() {
        let store = Store::memory().unwrap();
        seed(&store, 20);
        let child = child(&store, 10);
        assert_eq!(child.delegation, Some(scope()));
        assert!(
            store
                .acquire_lease(
                    "workspace",
                    "stranger",
                    "experiment",
                    "other-root",
                    100,
                    5,
                    "other-root"
                )
                .is_err()
        );
        let mut nested = scope();
        nested.parent_lease_id = "child".into();
        assert!(
            store
                .delegate_private_transfer_lease(
                    "workspace",
                    "receiver",
                    "stranger",
                    "nested",
                    &nested,
                    30,
                    2,
                    "nested"
                )
                .is_err()
        );
        assert!(
            store
                .delegate_private_transfer_lease(
                    "workspace",
                    "stranger",
                    "receiver",
                    "forged",
                    &scope(),
                    30,
                    2,
                    "forged"
                )
                .is_err()
        );
        assert!(
            store
                .delegate_private_transfer_lease(
                    "workspace",
                    "sender",
                    "missing",
                    "unknown",
                    &scope(),
                    30,
                    2,
                    "unknown"
                )
                .is_err()
        );
        assert!(
            store
                .delegate_private_transfer_lease(
                    "workspace",
                    "sender",
                    "receiver",
                    "late",
                    &scope(),
                    900,
                    2,
                    "late"
                )
                .is_err()
        );
        let native = json!({"component":"wallet-b","argv":["native-receive"],"timeout_seconds":30,"output":{"mode":"private"},"private_payload":{"kind":"consume","reference":"payload-ref","input":{"kind":"stdin"}}});
        for (pointer, value) in [
            ("/component", json!("wallet-a")),
            ("/argv", json!(["wallet-spend", "all"])),
            ("/timeout_seconds", json!(31)),
            ("/private_payload/input", json!({"kind":"argv","index":1})),
            ("/private_payload/reference", json!("other-ref")),
            ("/private_payload/kind", json!("capture")),
            ("/output/mode", json!("public")),
        ] {
            let mut request = native.clone();
            *request.pointer_mut(pointer).unwrap() = value;
            assert!(
                submit(
                    &store,
                    "receiver",
                    "child",
                    "refused",
                    OperationKind::ComponentExecLive,
                    &request
                )
                .is_err()
            );
        }
        let mut target = native.clone();
        target["target_component"] = json!("wallet-a");
        assert!(
            submit(
                &store,
                "receiver",
                "child",
                "target",
                OperationKind::ComponentExecLive,
                &target
            )
            .is_err()
        );
        for kind in [
            OperationKind::WalletInitialize,
            OperationKind::WalletPay,
            OperationKind::NodeRestart,
        ] {
            assert!(
                submit(
                    &store,
                    "receiver",
                    "child",
                    "wrong-kind",
                    kind,
                    &json!({"wallet":"wallet-b","mint":"mint"})
                )
                .is_err()
            );
        }
        for request in [
            json!({"wallet":"wallet-a","mint":"mint"}),
            json!({"wallet":"wallet-b","mint":"other"}),
        ] {
            assert!(
                submit(
                    &store,
                    "receiver",
                    "child",
                    "wrong-balance",
                    OperationKind::WalletBalance,
                    &request
                )
                .is_err()
            );
        }
        for method in ["prepare", "handoff", "release"] {
            assert!(submit(&store,"receiver","child","wrong-method",OperationKind::PrivateTransfer,&json!({"transfer":{"transferMethod":method,"component":"wallet-b","reference":"payload-ref"}})).is_err());
        }
        assert!(
            store
                .actions("workspace", "sender", "experiment", 0, 100)
                .unwrap()
                .is_empty()
        );
        let accepted = submit(
            &store,
            "receiver",
            "child",
            "receive",
            OperationKind::ComponentExecLive,
            &native,
        )
        .unwrap();
        assert_eq!(
            store.operation_lease_scope(&accepted).unwrap(),
            Some(scope())
        );
        assert_eq!(
            store
                .operation_lease_scope(
                    &store.operation("workspace", "receiver", "receive").unwrap()
                )
                .unwrap(),
            Some(scope())
        );
        assert!(
            store
                .operation_for_cancel("workspace", "sender", "receive")
                .is_err()
        );
        submit(
            &store,
            "receiver",
            "child",
            "balance",
            OperationKind::WalletBalance,
            &json!({"wallet":"wallet-b","mint":"mint"}),
        )
        .unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "revocation, idempotency and parent expiry share one authority lifecycle"
    )]
    fn recipient_revocation_expiry_and_idempotency_never_reactivate_authority() {
        let store = Store::memory().unwrap();
        seed(&store, 20);
        let original = child(&store, 10);
        assert_eq!(original, child(&store, 10));
        assert!(
            store
                .lease_for_release("workspace", "receiver", "parent")
                .is_err()
        );
        assert!(
            store
                .lease_for_release("workspace", "stranger", "child")
                .is_err()
        );
        store
            .release_lease("workspace", "sender", "child", "revoke-child")
            .unwrap();
        assert!(
            store
                .delegate_private_transfer_lease(
                    "workspace",
                    "sender",
                    "receiver",
                    "child",
                    &scope(),
                    300,
                    10,
                    "delegate"
                )
                .is_err()
        );
        assert!(
            submit(
                &store,
                "receiver",
                "child",
                "after-revoke",
                OperationKind::WalletBalance,
                &json!({"wallet":"wallet-b","mint":"mint"})
            )
            .is_err()
        );
        store
            .delegate_private_transfer_lease(
                "workspace",
                "sender",
                "receiver",
                "child-two",
                &scope(),
                200,
                5,
                "delegate-two",
            )
            .unwrap();
        store
            .release_lease("workspace", "receiver", "child-two", "self-release")
            .unwrap();
        store
            .delegate_private_transfer_lease(
                "workspace",
                "sender",
                "receiver",
                "child-three",
                &scope(),
                200,
                5,
                "delegate-three",
            )
            .unwrap();
        store
            .release_lease("workspace", "sender", "parent", "release-root")
            .unwrap();
        assert_eq!(
            store
                .lease("workspace", "receiver", "child-three")
                .unwrap()
                .phase,
            LeasePhase::Released
        );
        assert!(
            submit(
                &store,
                "receiver",
                "child-three",
                "after-root-release",
                OperationKind::WalletBalance,
                &json!({"wallet":"wallet-b","mint":"mint"})
            )
            .is_err()
        );
        let expired = Store::memory().unwrap();
        seed(&expired, 20);
        child(&expired, 5);
        expired
            .lock()
            .unwrap()
            .execute(
                "UPDATE experiment_leases SET expires_at=0 WHERE id='parent'",
                [],
            )
            .unwrap();
        assert_eq!(
            expired
                .lease("workspace", "receiver", "child")
                .unwrap()
                .phase,
            LeasePhase::Expired
        );
        assert!(
            submit(
                &expired,
                "receiver",
                "child",
                "after-expiry",
                OperationKind::WalletBalance,
                &json!({"wallet":"wallet-b","mint":"mint"})
            )
            .is_err()
        );
    }

    #[test]
    fn independent_connections_share_the_parent_action_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.db");
        let a = Store::open(&path).unwrap();
        seed(&a, 4);
        child(&a, 3);
        let b = Store::open(&path).unwrap();
        for store in [&a, &b] {
            store
                .lock()
                .unwrap()
                .busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers = [(a.clone(), "sender", "parent"), (b, "receiver", "child")]
            .into_iter()
            .map(|(store, principal, lease)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..2 {
                        submit(
                            &store,
                            principal,
                            lease,
                            &format!("{principal}-{i}"),
                            OperationKind::WalletBalance,
                            &json!({"wallet":"wallet-b","mint":"mint"}),
                        )
                        .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            a.actions("workspace", "sender", "experiment", 0, 100)
                .unwrap()
                .len(),
            4
        );
        assert!(matches!(
            submit(
                &a,
                "receiver",
                "child",
                "over-budget",
                OperationKind::WalletBalance,
                &json!({"wallet":"wallet-b","mint":"mint"})
            ),
            Err(StoreError::ActionBudgetExceeded { .. })
        ));
        // The accepted operation's original idempotency key does not spend again.
        submit(
            &a,
            "receiver",
            "child",
            "receiver-0",
            OperationKind::WalletBalance,
            &json!({"wallet":"wallet-b","mint":"mint"}),
        )
        .unwrap();
    }

    #[test]
    fn legacy_lease_schema_migrates_without_losing_exclusivity() {
        let store = Store::memory().unwrap();
        seed(&store, 10);
        {
            let db = store.lock().unwrap();
            db.execute_batch("DROP INDEX one_active_lease_per_instance;
            ALTER TABLE experiment_leases DROP COLUMN delegation_json;
            CREATE UNIQUE INDEX one_active_lease_per_instance ON experiment_leases(workspace_id,instance_id) WHERE phase_json='\"active\"';").unwrap();
        }
        migrate(&mut store.lock().unwrap()).unwrap();
        assert!(
            store
                .lease("workspace", "sender", "parent")
                .unwrap()
                .delegation
                .is_none()
        );
        child(&store, 5);
        assert!(
            store
                .acquire_lease(
                    "workspace",
                    "stranger",
                    "experiment",
                    "other",
                    60,
                    2,
                    "other"
                )
                .is_err()
        );
    }
}
