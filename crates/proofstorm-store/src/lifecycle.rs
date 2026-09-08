//! Lab-owned data lifetime and a crash-releasing, cross-process lifecycle guard.
use super::{
    Arc, BTreeSet, Connection, LabInstance, OptionalExtension, Store, StoreError,
    TransactionBehavior, params,
};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct LifecycleGuard {
    connection: Option<Connection>,
    busy: Arc<AtomicBool>,
}
impl Drop for LifecycleGuard {
    fn drop(&mut self) {
        if let Some(db) = &self.connection {
            let _ = db.execute_batch("ROLLBACK");
        }
        self.busy.store(false, Ordering::Release);
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBinding {
    pub source: String,
    pub cluster_uid: String,
    pub resource_uid: Option<String>,
}

pub(super) fn initialize_schema(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS lab_runtime_bindings(
        workspace_id TEXT NOT NULL, instance_id TEXT NOT NULL, instance_key TEXT NOT NULL,
        source TEXT NOT NULL, cluster_uid TEXT NOT NULL, resource_uid TEXT,
        PRIMARY KEY(workspace_id,instance_id));",
    )?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS lab_plan_uses(workspace_id TEXT NOT NULL,instance_id TEXT NOT NULL,instance_key TEXT NOT NULL,plan_id TEXT NOT NULL,PRIMARY KEY(workspace_id,instance_key,plan_id));")?;
    Ok(())
}

impl Store {
    /// Serializes lifecycle transitions across processes sharing this database.
    /// The sidecar contains no lab records. `SQLite` releases the guard on process death.
    pub fn try_lifecycle_guard(&self) -> Result<Option<LifecycleGuard>, StoreError> {
        if self
            .lifecycle_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Ok(None);
        }
        let mut guard = LifecycleGuard {
            connection: None,
            busy: self.lifecycle_busy.clone(),
        };
        let path = self
            .lock()?
            .path()
            .filter(|p| !p.is_empty())
            .map(str::to_owned);
        if let Some(path) = path {
            let db = Connection::open(format!("{path}.lifecycle-lock"))?;
            db.busy_timeout(std::time::Duration::ZERO)?;
            match db.execute_batch("BEGIN IMMEDIATE") {
                Ok(()) => guard.connection = Some(db),
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(guard))
    }

    pub fn runtime_binding(
        &self,
        instance: &LabInstance,
    ) -> Result<Option<RuntimeBinding>, StoreError> {
        Ok(self.lock()?.query_row("SELECT source,cluster_uid,resource_uid FROM lab_runtime_bindings WHERE workspace_id=?1 AND instance_id=?2 AND instance_key=?3",params![instance.workspace_id,instance.id,instance.instance_key],|r|Ok(RuntimeBinding {source:r.get(0)?,cluster_uid:r.get(1)?,resource_uid:r.get(2)?})).optional()?)
    }
    pub fn bind_runtime(
        &self,
        instance: &LabInstance,
        binding: &RuntimeBinding,
    ) -> Result<(), StoreError> {
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_incarnation(&tx, instance)?;
        tx.execute("INSERT INTO lab_runtime_bindings VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(workspace_id,instance_id) DO UPDATE SET resource_uid=COALESCE(excluded.resource_uid,resource_uid) WHERE instance_key=excluded.instance_key AND source=excluded.source AND cluster_uid=excluded.cluster_uid AND (resource_uid IS NULL OR resource_uid=excluded.resource_uid)",params![instance.workspace_id,instance.id,instance.instance_key,binding.source,binding.cluster_uid,binding.resource_uid])?;
        let actual=tx.query_row("SELECT source,cluster_uid,resource_uid FROM lab_runtime_bindings WHERE workspace_id=?1 AND instance_id=?2",params![instance.workspace_id,instance.id],|r|Ok(RuntimeBinding {source:r.get(0)?,cluster_uid:r.get(1)?,resource_uid:r.get(2)?}))?;
        if actual.source != binding.source
            || actual.cluster_uid != binding.cluster_uid
            || binding
                .resource_uid
                .as_ref()
                .is_some_and(|uid| actual.resource_uid.as_ref() != Some(uid))
        {
            return Err(stale());
        }
        tx.commit()?;
        Ok(())
    }

    /// Recover a crash after reserving a developer name but before creating its instance.
    pub fn purge_unmaterialized_handle(&self, workspace: &str, id: &str) -> Result<(), StoreError> {
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM instances WHERE workspace_id=?1 AND id=?2)",
            params![workspace, id],
            |r| r.get(0),
        )?;
        if exists {
            return Err(stale());
        }
        let removed = tx.execute(
            "DELETE FROM lab_handles WHERE workspace_id=?1 AND instance_id=?2",
            params![workspace, id],
        )?;
        if removed > 0 {
            let draft = format!("draft-{id}");
            tx.execute("DELETE FROM idempotency WHERE workspace_id=?1 AND json_valid(response_json) AND (json_extract(response_json,'$.id')=?2 OR json_extract(response_json,'$.digest') IN (SELECT digest FROM revisions WHERE workspace_id=?1 AND draft_id=?2))",params![workspace,draft])?;
            tx.execute("DELETE FROM revisions WHERE workspace_id=?1 AND draft_id=?2 AND digest NOT IN (SELECT revision_digest FROM instances)",params![workspace,draft])?;
            tx.execute(
                "DELETE FROM drafts WHERE workspace_id=?1 AND id=?2",
                params![workspace, draft],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn record_plan_use(
        &self,
        instance: &LabInstance,
        plan: Option<&str>,
    ) -> Result<(), StoreError> {
        let db = self.lock()?;
        require_incarnation(&db, instance)?;
        let plan = match plan {
            Some(p) => p.to_owned(),
            None => db.query_row(
                "SELECT draft_id FROM revisions WHERE digest=?1",
                [&instance.revision_digest],
                |r| r.get(0),
            )?,
        };
        db.execute(
            "INSERT OR IGNORE INTO lab_plan_uses VALUES(?1,?2,?3,?4)",
            params![
                instance.workspace_id,
                instance.id,
                instance.instance_key,
                plan
            ],
        )?;
        Ok(())
    }

    /// Caller must hold the lifecycle guard and verify exact runtime/namespace absence.
    #[allow(
        clippy::too_many_lines,
        reason = "all dependent deletions must remain visible in one atomic purge"
    )]
    pub fn purge_lab(&self, instance: &LabInstance) -> Result<(), StoreError> {
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_incarnation(&tx, instance)?;
        let ws = &instance.workspace_id;
        let id = &instance.id;
        let mut ids = BTreeSet::from([id.clone()]);
        for table in ["experiments", "sessions", "actions"] {
            ids.extend(
                tx.prepare(&format!(
                    "SELECT id FROM {table} WHERE workspace_id=?1 AND instance_id=?2"
                ))?
                .query_map(params![ws, id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?,
            );
        }
        let revisions=tx.prepare("SELECT revision_digest FROM instances WHERE workspace_id=?1 AND id=?2 UNION SELECT revision_digest FROM operation_revisions WHERE workspace_id=?1 AND operation_id IN (SELECT id FROM actions WHERE workspace_id=?1 AND instance_id=?2) UNION SELECT json_extract(plan_json,'$.base_revision') FROM lab_updates WHERE workspace_id=?1 AND instance_id=?2 UNION SELECT json_extract(plan_json,'$.target_revision') FROM lab_updates WHERE workspace_id=?1 AND instance_id=?2")?.query_map(params![ws,id],|r|r.get::<_,String>(0))?.collect::<Result<BTreeSet<_>,_>>()?;
        let mut drafts = tx
            .prepare("SELECT plan_id FROM lab_plan_uses WHERE workspace_id=?1 AND instance_key=?2")?
            .query_map(params![ws, instance.instance_key], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<BTreeSet<_>, _>>()?;
        let unrecorded_creation = drafts.is_empty();
        for revision in revisions.iter().filter(|_| unrecorded_creation) {
            if let Some(draft) = tx
                .query_row(
                    "SELECT draft_id FROM revisions WHERE workspace_id=?1 AND digest=?2",
                    params![ws, revision],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                drafts.insert(draft);
            }
        }
        drafts.extend(tx.prepare("SELECT plan_id FROM lab_update_plans WHERE workspace_id=?1 AND json_extract(plan_json,'$.target.instance_id')=?2")?.query_map(params![ws,id],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?);
        drafts.extend(
            tx.prepare(
                "SELECT plan_id FROM lab_plan_uses WHERE workspace_id=?1 AND instance_key=?2",
            )?
            .query_map(params![ws, instance.instance_key], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?,
        );
        ids.extend(drafts.iter().cloned());
        ids.extend(revisions.iter().cloned());
        // Remove receipts by their identity fields, not arbitrary user strings in artifacts.
        let receipts = tx
            .prepare(
                "SELECT principal_id,key,response_json FROM idempotency WHERE workspace_id=?1",
            )?
            .query_map([ws], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (principal, key, body) in receipts {
            if references_identity(&serde_json::from_str(&body)?, &ids) {
                tx.execute(
                    "DELETE FROM idempotency WHERE workspace_id=?1 AND principal_id=?2 AND key=?3",
                    params![ws, principal, key],
                )?;
            }
        }
        tx.execute("DELETE FROM private_access_grants WHERE workspace_id=?1 AND json_extract(grant_json,'$.instance_id')=?2",params![ws,id])?;
        tx.execute("DELETE FROM operation_revisions WHERE workspace_id=?1 AND operation_id IN (SELECT id FROM actions WHERE workspace_id=?1 AND instance_id=?2)",params![ws,id])?;
        for table in [
            "wallet_payment_claims",
            "wallet_quote_observations",
            "actions",
            "sessions",
            "experiments",
            "lab_updates",
            "lab_update_state",
            "retained_components",
            "lab_runtime_bindings",
            "lab_plan_uses",
            "lab_handles",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE workspace_id=?1 AND instance_id=?2"),
                params![ws, id],
            )?;
        }
        tx.execute("DELETE FROM lab_update_plans WHERE workspace_id=?1 AND json_extract(plan_json,'$.target.instance_id')=?2",params![ws,id])?;
        tx.execute(
            "DELETE FROM instances WHERE workspace_id=?1 AND id=?2 AND instance_key=?3",
            params![ws, id, instance.instance_key],
        )?;
        for draft in drafts {
            // A consumed plan is not a reusable template; old apply requests must replan.
            tx.execute(
                "DELETE FROM drafts WHERE workspace_id=?1 AND id=?2",
                params![ws, draft],
            )?;
        }
        for revision in revisions {
            tx.execute("DELETE FROM revisions WHERE workspace_id=?1 AND digest=?2 AND NOT EXISTS(SELECT 1 FROM instances WHERE revision_digest=?2) AND NOT EXISTS(SELECT 1 FROM operation_revisions WHERE revision_digest=?2) AND NOT EXISTS(SELECT 1 FROM lab_update_plans WHERE json_extract(plan_json,'$.base_revision')=?2 OR json_extract(plan_json,'$.target_revision')=?2) AND NOT EXISTS(SELECT 1 FROM lab_updates WHERE json_extract(plan_json,'$.base_revision')=?2 OR json_extract(plan_json,'$.target_revision')=?2)",params![ws,revision])?;
        }
        tx.commit()?;
        Ok(())
    }
}
fn references_identity(value: &serde_json::Value, ids: &BTreeSet<String>) -> bool {
    match value {
        serde_json::Value::Object(o) => o.iter().any(|(k, v)| {
            matches!(
                k.as_str(),
                "id" | "digest"
                    | "instance_id"
                    | "instanceId"
                    | "experiment_id"
                    | "session_id"
                    | "operation_id"
                    | "draft_id"
                    | "plan_id"
            ) && v.as_str().is_some_and(|s| ids.contains(s))
                || (!matches!(
                    k.as_str(),
                    "artifact" | "request" | "lab" | "lock" | "config"
                ) && references_identity(v, ids))
        }),
        serde_json::Value::Array(a) => a.iter().any(|v| references_identity(v, ids)),
        _ => false,
    }
}
fn stale() -> StoreError {
    StoreError::LabUpdate {
        code: "stale_incarnation",
        message: "Lab incarnation changed; read the current lab and replan".into(),
    }
}
fn require_incarnation(db: &Connection, instance: &LabInstance) -> Result<(), StoreError> {
    let key: Option<String> = db
        .query_row(
            "SELECT instance_key FROM instances WHERE workspace_id=?1 AND id=?2",
            params![instance.workspace_id, instance.id],
            |r| r.get(0),
        )
        .optional()?;
    if key.as_deref() != Some(instance.instance_key.as_str()) {
        return Err(stale());
    }
    Ok(())
}
