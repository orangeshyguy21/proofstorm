//! Internal run grouping for commands that do not explicitly select an experiment.
use super::{
    Capability, Connection, Experiment, ExperimentPhase, OptionalExtension, Store, StoreError,
    params,
};

pub(super) fn initialize_schema(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS automatic_runs(workspace_id TEXT NOT NULL,instance_key TEXT NOT NULL,principal_id TEXT NOT NULL,ordinal INTEGER NOT NULL,id TEXT NOT NULL,PRIMARY KEY(workspace_id,instance_key,principal_id)); CREATE TABLE IF NOT EXISTS run_seals(workspace_id TEXT NOT NULL,run_id TEXT NOT NULL,instance_json TEXT NOT NULL,revision_json TEXT NOT NULL,PRIMARY KEY(workspace_id,run_id),FOREIGN KEY(workspace_id,run_id) REFERENCES experiments(workspace_id,id) ON DELETE CASCADE);")?;
    Ok(())
}

fn next_run(
    db: &Connection,
    workspace: &str,
    key: &str,
    principal: &str,
) -> Result<(i64, String), StoreError> {
    let previous:Option<(i64,String,Option<String>)>=db.query_row("SELECT a.ordinal,a.id,e.phase_json FROM automatic_runs a LEFT JOIN experiments e ON e.workspace_id=a.workspace_id AND e.id=a.id WHERE a.workspace_id=?1 AND a.instance_key=?2 AND a.principal_id=?3",params![workspace,key,principal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let ordinal = match previous {
        Some((ordinal, id, Some(phase))) if phase == "\"active\"" => return Ok((ordinal, id)),
        Some((ordinal, _, _)) => ordinal
            .checked_add(1)
            .ok_or_else(|| StoreError::Validation("run sequence exhausted".into()))?,
        None => 0,
    };
    let digest = proofstorm_core::digest_json(&(workspace, key, principal, ordinal));
    Ok((ordinal, format!("run-{}", &digest[7..39])))
}

impl Store {
    pub fn default_run_id(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<String, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.implicit_run_id(workspace, principal, instance)
    }

    /// Resolve grouping without creating it; compound operations can validate their references first.
    pub fn operation_run_id(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        requested: &str,
        capability: Capability,
    ) -> Result<String, StoreError> {
        self.authorize(workspace, principal, capability)?;
        if requested.is_empty() {
            self.implicit_run_id(workspace, principal, instance)
        } else {
            Ok(requested.into())
        }
    }

    pub(super) fn implicit_run_id(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<String, StoreError> {
        let instance = self.instance_unchecked(workspace, instance)?;
        Ok(next_run(&*self.lock()?, workspace, &instance.instance_key, principal)?.1)
    }

    /// Bookkeeping under an existing operation capability, never a new permission grant.
    pub fn ensure_default_run(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        capability: Capability,
    ) -> Result<Experiment, StoreError> {
        self.authorize(workspace, principal, capability)?;
        let id = self.implicit_run_id(workspace, principal, instance)?;
        self.ensure_implicit_run(workspace, principal, instance, &id)?;
        self.experiment_unchecked(workspace, &id)
    }

    pub(super) fn ensure_implicit_run(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        id: &str,
    ) -> Result<(), StoreError> {
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(super::TransactionBehavior::Immediate)?;
        let key: String = tx.query_row(
            "SELECT instance_key FROM instances WHERE workspace_id=?1 AND id=?2",
            params![workspace, instance],
            |r| r.get(0),
        )?;
        let (ordinal, expected_id) = next_run(&tx, workspace, &key, principal)?;
        if id != expected_id {
            return Err(StoreError::Validation(
                "cell incarnation changed during run admission; read current cell and retry".into(),
            ));
        }
        let closing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM cell_handles WHERE workspace_id=?1 AND instance_id=?2 AND phase!='\"open\"') OR EXISTS(SELECT 1 FROM cell_update_state WHERE workspace_id=?1 AND instance_id=?2 AND closing=1)",params![workspace,instance],|r|r.get(0))?;
        if closing {
            return Err(StoreError::Validation(
                "cell is closing; new actions are not admitted".into(),
            ));
        }
        tx.execute(
            "INSERT OR IGNORE INTO experiments(workspace_id,id,instance_id,owner_principal_id,phase_json,created_at)
             VALUES(?1,?2,?3,?4,'\"active\"',unixepoch())",
            params![workspace,id,instance,principal],
        )?;
        tx.execute("INSERT INTO automatic_runs VALUES(?1,?2,?3,?4,?5) ON CONFLICT(workspace_id,instance_key,principal_id) DO UPDATE SET ordinal=excluded.ordinal,id=excluded.id",params![workspace,key,principal,ordinal,id])?;
        tx.commit()?;
        drop(db);
        let run = self.experiment_unchecked(workspace, id)?;
        if run.instance_id != instance
            || run.owner_principal_id != principal
            || run.phase != ExperimentPhase::Active
        {
            return Err(StoreError::Validation(
                "default run changed during admission; retry the same request".into(),
            ));
        }
        Ok(())
    }
}

impl Store {
    pub fn sealed_run_context(
        &self,
        workspace: &str,
        principal: &str,
        run: &str,
    ) -> Result<
        (
            proofstorm_core::CellInstance,
            proofstorm_core::PublishedRevision,
        ),
        StoreError,
    > {
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let (instance, revision): (String, String) = self.lock()?.query_row(
            "SELECT instance_json,revision_json FROM run_seals WHERE workspace_id=?1 AND run_id=?2",
            params![workspace, run],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((
            serde_json::from_str(&instance)?,
            serde_json::from_str(&revision)?,
        ))
    }

    pub fn run_directory(
        &self,
        workspace: &str,
        principal: &str,
        instance: Option<&str>,
        after: &str,
        limit: u32,
    ) -> Result<Vec<Experiment>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let ids=self.lock()?.prepare("SELECT id FROM experiments WHERE workspace_id=?1 AND (?2 IS NULL OR instance_id=?2) AND id>?3 ORDER BY id LIMIT ?4")?.query_map(params![workspace,instance,after,limit.min(201)],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        ids.into_iter()
            .map(|id| self.experiment_unchecked(workspace, &id))
            .collect()
    }
}
