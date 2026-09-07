//! Durable desired-state updates. `SQLite` acceptance and Kubernetes reconciliation are resumable.
use super::{
    BTreeSet, Capability, Connection, Deserialize, JsonSchema, LabInstance, OptionalExtension,
    PublishedRevision, Serialize, Store, StoreError, TransactionBehavior, now_unix, params,
    sql_version,
};
use proofstorm_core::{LabUpdatePlan, LabUpdateTarget};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LabUpdateState {
    pub generation: u64,
    pub applied_generation: u64,
    pub converged_revision: Option<String>,
    pub closing: bool,
}

pub(super) fn initialize_schema(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS lab_update_state(workspace_id TEXT NOT NULL,instance_id TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 1,applied_generation INTEGER NOT NULL DEFAULT 1,converged_revision TEXT,closing INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(workspace_id,instance_id));
        CREATE TABLE IF NOT EXISTS lab_update_plans(workspace_id TEXT NOT NULL,plan_id TEXT NOT NULL,plan_json TEXT NOT NULL,PRIMARY KEY(workspace_id,plan_id));
        CREATE TABLE IF NOT EXISTS lab_updates(workspace_id TEXT NOT NULL,instance_id TEXT NOT NULL,generation INTEGER NOT NULL,principal_id TEXT NOT NULL,request_key TEXT NOT NULL,plan_json TEXT NOT NULL,accepted_at INTEGER NOT NULL,PRIMARY KEY(workspace_id,instance_id,generation),UNIQUE(workspace_id,principal_id,request_key));
        CREATE TABLE IF NOT EXISTS operation_revisions(workspace_id TEXT NOT NULL,operation_id TEXT NOT NULL,revision_digest TEXT NOT NULL,PRIMARY KEY(workspace_id,operation_id));
        CREATE TRIGGER IF NOT EXISTS capture_operation_revision AFTER INSERT ON actions BEGIN INSERT OR IGNORE INTO operation_revisions SELECT NEW.workspace_id,NEW.id,revision_digest FROM instances WHERE workspace_id=NEW.workspace_id AND id=NEW.instance_id; END;
        CREATE TABLE IF NOT EXISTS retained_components(workspace_id TEXT NOT NULL,instance_id TEXT NOT NULL,component_id TEXT NOT NULL,PRIMARY KEY(workspace_id,instance_id,component_id));")?;
    Ok(())
}
pub(crate) fn state(
    db: &Connection,
    workspace: &str,
    instance: &str,
) -> Result<LabUpdateState, StoreError> {
    Ok(db.query_row("SELECT generation,applied_generation,converged_revision,closing FROM lab_update_state WHERE workspace_id=?1 AND instance_id=?2",params![workspace,instance],|r| Ok(LabUpdateState {generation:generation_column(r, 0)?,applied_generation:generation_column(r, 1)?,converged_revision:r.get(2)?,closing:r.get(3)?})).optional()?.unwrap_or(LabUpdateState {generation:1,applied_generation:1,converged_revision:None,closing:false}))
}
pub(crate) fn generation_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn error(code: &'static str, message: &str) -> StoreError {
    StoreError::LabUpdate {
        code,
        message: message.into(),
    }
}
fn references(value: &serde_json::Value, ids: &BTreeSet<String>) -> bool {
    match value {
        serde_json::Value::String(s) => ids.contains(s),
        serde_json::Value::Array(a) => a.iter().any(|v| references(v, ids)),
        serde_json::Value::Object(o) => o.values().any(|v| references(v, ids)),
        _ => false,
    }
}
impl Store {
    pub fn pending_updates(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<Vec<String>, StoreError> {
        self.authorize(workspace, principal, Capability::LabMaterialize)?;
        Ok(self.lock()?.prepare("SELECT instance_id FROM lab_update_state WHERE workspace_id=?1 AND generation>applied_generation AND closing=0 LIMIT 50")?.query_map([workspace],|r|r.get(0))?.collect::<Result<Vec<_>,_>>()?)
    }
    pub fn update_state(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<LabUpdateState, StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        state(&*self.lock()?, workspace, instance)
    }
    pub fn plan_update(
        &self,
        workspace: &str,
        principal: &str,
        target: LabUpdateTarget,
        revision: &PublishedRevision,
    ) -> Result<LabUpdatePlan, StoreError> {
        self.authorize(workspace, principal, Capability::LabEdit)?;
        let instance = self.instance_unchecked(workspace, &target.instance_id)?;
        let old = self.revision_unchecked(workspace, &instance.revision_digest)?;
        let current = state(&*self.lock()?, workspace, &target.instance_id)?;
        if current.closing {
            return Err(error("lab_closing", "closing labs cannot be edited"));
        }
        if current.generation != target.expected_generation {
            return Err(error(
                "lab_update_conflict",
                "desired generation changed; read current configuration and replan",
            ));
        }
        let mut plan =
            LabUpdatePlan::new(target, &old, revision).map_err(StoreError::Validation)?;
        plan.bind_instance(&instance.instance_key);
        let db = self.lock()?;
        for id in &plan.target.delete_retained {
            let retained:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM retained_components WHERE workspace_id=?1 AND instance_id=?2 AND component_id=?3)",params![workspace,plan.target.instance_id,id],|r|r.get(0))?;
            if !retained || revision.lab.components.iter().any(|c| &c.id == id) {
                return Err(error(
                    "retained_component_conflict",
                    "explicit retained deletion must identify removed data, never a current component",
                ));
            }
        }
        for id in &plan.changes.added {
            let retained:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM retained_components WHERE workspace_id=?1 AND instance_id=?2 AND component_id=?3)",params![workspace,plan.target.instance_id,id],|r|r.get(0))?;
            if retained {
                return Err(error(
                    "retained_component_conflict",
                    "component ID has retained state; use another ID or explicitly delete its retained data first",
                ));
            }
        }
        Ok(plan)
    }
    pub fn save_update_plan(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        plan: &LabUpdatePlan,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::LabEdit)?;
        let encoded = serde_json::to_string(plan)?;
        let db = self.lock()?;
        db.execute(
            "INSERT OR IGNORE INTO lab_update_plans VALUES(?1,?2,?3)",
            params![workspace, id, encoded],
        )?;
        let old: String = db.query_row(
            "SELECT plan_json FROM lab_update_plans WHERE workspace_id=?1 AND plan_id=?2",
            params![workspace, id],
            |r| r.get(0),
        )?;
        if old != encoded {
            return Err(error(
                "lab_plan_id_conflict",
                "use a new plan ID for a changed update",
            ));
        }
        Ok(())
    }
    pub fn update_plan(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<Option<LabUpdatePlan>, StoreError> {
        self.authorize(workspace, principal, Capability::LabRead)?;
        let row: Option<String> = self
            .lock()?
            .query_row(
                "SELECT plan_json FROM lab_update_plans WHERE workspace_id=?1 AND plan_id=?2",
                params![workspace, id],
                |r| r.get(0),
            )
            .optional()?;
        row.map(|s| serde_json::from_str(&s).map_err(StoreError::from))
            .transpose()
    }
    #[allow(
        clippy::too_many_lines,
        reason = "generation, operation conflicts, retained data and the durable receipt are one atomic transaction"
    )]
    pub fn accept_update(
        &self,
        workspace: &str,
        principal: &str,
        plan: &LabUpdatePlan,
        key: &str,
    ) -> Result<LabInstance, StoreError> {
        self.authorize(workspace, principal, Capability::LabEdit)?;
        self.authorize(workspace, principal, Capability::LabMaterialize)?;
        if !plan.changes.unsupported.is_empty() {
            return Err(error(
                "lab_update_unsupported",
                &plan.changes.unsupported.join("; "),
            ));
        }
        let revision = self.revision_unchecked(workspace, &plan.target_revision)?;
        let old = self.revision_unchecked(workspace, &plan.base_revision)?;
        let mut verified = LabUpdatePlan::new(plan.target.clone(), &old, &revision)
            .map_err(StoreError::Validation)?;
        verified.bind_instance(&plan.instance_key);
        if verified != *plan {
            return Err(error(
                "lab_plan_digest_mismatch",
                "update plan does not match its immutable revisions",
            ));
        }
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key_now: Option<String> = tx
            .query_row(
                "SELECT instance_key FROM instances WHERE workspace_id=?1 AND id=?2",
                params![workspace, plan.target.instance_id],
                |r| r.get(0),
            )
            .optional()?;
        if key_now.as_deref() != Some(plan.instance_key.as_str()) {
            return Err(error(
                "stale_incarnation",
                "This plan targets a deleted or replaced lab; replan",
            ));
        }
        let previous:Option<String>=tx.query_row("SELECT response_json FROM idempotency WHERE workspace_id=?1 AND principal_id=?2 AND key=?3",params![workspace,principal,key],|r|r.get(0)).optional()?;
        if let Some(ref previous) = previous {
            if serde_json::from_str::<LabUpdatePlan>(previous)
                .ok()
                .as_ref()
                != Some(plan)
            {
                return Err(error(
                    "idempotency_conflict",
                    "request key belongs to a different edit",
                ));
            }
        } else {
            let current = state(&tx, workspace, &plan.target.instance_id)?;
            if current.closing {
                return Err(error("lab_closing", "closing labs cannot be edited"));
            }
            let current_revision: String = tx.query_row(
                "SELECT revision_digest FROM instances WHERE workspace_id=?1 AND id=?2",
                params![workspace, plan.target.instance_id],
                |r| r.get(0),
            )?;
            if current.generation != plan.target.expected_generation
                || current_revision != plan.base_revision
            {
                return Err(error(
                    "lab_update_conflict",
                    "desired configuration changed; replan",
                ));
            }
            if plan.is_noop() {
                record_update_receipt(&tx, workspace, principal, key, plan)?;
                tx.commit()?;
                drop(db);
                return self.instance_unchecked(workspace, &plan.target.instance_id);
            }
            let affected = plan.affected();
            let active=tx.prepare("SELECT id,request_json FROM actions WHERE workspace_id=?1 AND instance_id=?2 AND phase_json IN ('\"pending\"','\"running\"')")?.query_map(params![workspace,plan.target.instance_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
            let conflicts = active
                .iter()
                .filter(|(_, request)| {
                    serde_json::from_str(request).map_or(true, |v| references(&v, &affected))
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            if !conflicts.is_empty() {
                return Err(error(
                    "lab_update_active_operations",
                    &format!(
                        "finish or cancel affected operations: {}",
                        conflicts.join(", ")
                    ),
                ));
            }
            for id in &plan.target.delete_retained {
                let retained: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM retained_components WHERE workspace_id=?1 AND instance_id=?2 AND component_id=?3)",params![workspace,plan.target.instance_id,id],|r|r.get(0))?;
                if !retained || revision.lab.components.iter().any(|c| &c.id == id) {
                    return Err(error(
                        "retained_component_conflict",
                        "retained deletion must identify removed data",
                    ));
                }
            }
            for id in &plan.changes.added {
                if tx.query_row("SELECT EXISTS(SELECT 1 FROM retained_components WHERE workspace_id=?1 AND instance_id=?2 AND component_id=?3)",params![workspace,plan.target.instance_id,id],|r|r.get::<_,bool>(0))? { return Err(error("retained_component_conflict","component ID has retained state")); }
            }
            let generation = current
                .generation
                .checked_add(1)
                .ok_or_else(|| error("generation_exhausted", "lab generation exhausted"))?;
            tx.execute("INSERT INTO lab_update_state(workspace_id,instance_id,generation,applied_generation) VALUES(?1,?2,?3,1) ON CONFLICT(workspace_id,instance_id) DO UPDATE SET generation=excluded.generation",params![workspace,plan.target.instance_id,sql_version(generation)?])?;
            tx.execute("UPDATE instances SET revision_digest=?1,lock_digest=?2 WHERE workspace_id=?3 AND id=?4",params![revision.digest,revision.lock.digest,workspace,plan.target.instance_id])?;
            tx.execute(
                "UPDATE lab_handles SET config_digest=?1 WHERE workspace_id=?2 AND instance_id=?3",
                params![
                    proofstorm_core::digest_json(&revision.lab),
                    workspace,
                    plan.target.instance_id
                ],
            )?;
            tx.execute(
                "INSERT INTO lab_updates VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    workspace,
                    plan.target.instance_id,
                    sql_version(generation)?,
                    principal,
                    key,
                    serde_json::to_string(plan)?,
                    now_unix()
                ],
            )?;
            for id in &plan.changes.removed {
                tx.execute(
                    "INSERT OR IGNORE INTO retained_components VALUES(?1,?2,?3)",
                    params![workspace, plan.target.instance_id, id],
                )?;
            }
        }
        if previous.is_none() {
            record_update_receipt(&tx, workspace, principal, key, plan)?;
        }
        tx.commit()?;
        drop(db);
        // A retry returns the latest desired identity; it must never reapply an old revision.
        self.instance_unchecked(workspace, &plan.target.instance_id)
    }
    pub fn pending_deleted_data(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<Vec<String>, StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        let db = self.lock()?;
        let current = state(&db, workspace, instance)?;
        Ok(
            pending_plans(&db, workspace, instance, current.applied_generation)?
                .into_iter()
                .flat_map(|p| p.changes.deleted_data)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        )
    }
    pub fn latest_update(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<Option<LabUpdatePlan>, StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        let row:Option<String>=self.lock()?.query_row("SELECT plan_json FROM lab_updates WHERE workspace_id=?1 AND instance_id=?2 ORDER BY generation DESC LIMIT 1",params![workspace,instance],|r|r.get(0)).optional()?;
        row.map(|s| serde_json::from_str(&s).map_err(StoreError::from))
            .transpose()
    }
    pub fn mark_update_applied(
        &self,
        workspace: &str,
        instance: &str,
        generation: u64,
        converged: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = state(&tx, workspace, instance)?;
        if current.generation != generation {
            return Ok(());
        }
        for id in pending_plans(&tx, workspace, instance, current.applied_generation)?
            .into_iter()
            .flat_map(|p| p.changes.deleted_data)
        {
            tx.execute("DELETE FROM retained_components WHERE workspace_id=?1 AND instance_id=?2 AND component_id=?3",params![workspace,instance,id])?;
        }
        tx.execute("UPDATE lab_update_state SET applied_generation=MAX(applied_generation,?3),converged_revision=COALESCE(?4,converged_revision) WHERE workspace_id=?1 AND instance_id=?2 AND generation=?3",params![workspace,instance,sql_version(generation)?,converged])?;
        tx.commit()?;
        Ok(())
    }
    pub fn begin_instance_close(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::LabClose)?;
        self.lock()?.execute("INSERT INTO lab_update_state(workspace_id,instance_id,closing) VALUES(?1,?2,1) ON CONFLICT(workspace_id,instance_id) DO UPDATE SET closing=1",params![workspace,instance])?;
        Ok(())
    }
}

pub(super) fn admit_operation(
    tx: &Connection,
    workspace: &str,
    instance: &str,
    operation: &str,
    request: &serde_json::Value,
    kind: proofstorm_core::OperationKind,
) -> Result<String, StoreError> {
    let current = state(tx, workspace, instance)?;
    if current.closing {
        return Err(error("lab_closing", "new operations are not admitted"));
    }
    for plan in pending_plans(tx, workspace, instance, current.applied_generation)? {
        if kind != proofstorm_core::OperationKind::ComponentLogs
            && references(request, &plan.affected())
        {
            return Err(error(
                "component_updating",
                "component is scheduled for change; retry after reconciliation",
            ));
        }
    }
    let revision: String = tx.query_row(
        "SELECT revision_digest FROM instances WHERE workspace_id=?1 AND id=?2",
        params![workspace, instance],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO operation_revisions VALUES(?1,?2,?3)",
        params![workspace, operation, revision],
    )?;
    Ok(tx.query_row(
        "SELECT revision_digest FROM operation_revisions WHERE workspace_id=?1 AND operation_id=?2",
        params![workspace, operation],
        |r| r.get(0),
    )?)
}

fn pending_plans(
    db: &Connection,
    workspace: &str,
    instance: &str,
    after: u64,
) -> Result<Vec<LabUpdatePlan>, StoreError> {
    let rows=db.prepare("SELECT plan_json FROM lab_updates WHERE workspace_id=?1 AND instance_id=?2 AND generation>?3 ORDER BY generation")?.query_map(params![workspace,instance,sql_version(after)?],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    rows.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(StoreError::from))
        .collect()
}

fn record_update_receipt(
    tx: &Connection,
    workspace: &str,
    principal: &str,
    key: &str,
    plan: &LabUpdatePlan,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO idempotency VALUES(?1,?2,?3,'lab.update',?4,?5)",
        params![
            workspace,
            principal,
            key,
            proofstorm_core::digest_json(plan),
            serde_json::to_string(plan)?
        ],
    )?;
    Ok(())
}
