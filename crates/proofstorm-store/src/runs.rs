//! Internal run grouping for commands that do not explicitly select an experiment.
use super::{Capability, Experiment, ExperimentPhase, Store, StoreError, params};

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
        let digest = proofstorm_core::digest_json(&(workspace, &instance.instance_key, principal));
        Ok(format!("run-{}", &digest[7..39]))
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
        let digest = proofstorm_core::digest_json(&(workspace, &key, principal));
        if id != format!("run-{}", &digest[7..39]) {
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
        tx.commit()?;
        drop(db);
        let run = self.experiment_unchecked(workspace, id)?;
        if run.instance_id != instance
            || run.owner_principal_id != principal
            || run.phase != ExperimentPhase::Active
        {
            return Err(StoreError::Validation("default run is closed or belongs to another actor; select an explicit open experiment".into()));
        }
        Ok(())
    }
}
