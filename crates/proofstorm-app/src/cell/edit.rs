//! Live-edit convenience over the same durable plans used by reviewed apply.
use super::{CellView, Cells};
use crate::Error;
use proofstorm_core::CellSpec;

impl Cells {
    pub fn plan_edit(
        &self,
        name: &str,
        spec: &CellSpec,
        delete_data: bool,
        delete_retained: &[String],
    ) -> Result<proofstorm_core::CellUpdatePlan, Error> {
        self.plan_edit_at(name, spec, delete_data, delete_retained, None)
    }

    pub(super) fn plan_edit_at(
        &self,
        name: &str,
        spec: &CellSpec,
        delete_data: bool,
        delete_retained: &[String],
        expected_generation: Option<u64>,
    ) -> Result<proofstorm_core::CellUpdatePlan, Error> {
        let handle = self.resolve(name)?;
        let instance = self.instance(&handle)?;
        let generation = expected_generation.unwrap_or(instance.generation);
        let draft_id = format!(
            "edit-{}",
            &proofstorm_core::digest_json(&(
                instance.id.clone(),
                generation,
                spec,
                delete_data,
                delete_retained
            ))[7..39]
        );
        // A fenced retry must find the same immutable plan after its generation
        // has advanced. accept_update checks its durable receipt before the CAS,
        // and never restores an older desired revision when replaying it.
        if let Some(plan) = self
            .store
            .update_plan(&self.workspace, &self.principal, &draft_id)?
        {
            return Ok(plan);
        }
        if generation != instance.generation {
            return Err(Error::failure(
                "Desired configuration changed; inspect the cell and replan",
                Some(
                    serde_json::json!({"code":"cell_update_conflict", "accepted":false,
                    "expected_generation":generation, "desired_generation":instance.generation,
                    "instance_key":instance.instance_key, "next_tool":"cell_inspect"}),
                ),
            ));
        }
        self.store.create_draft(
            &self.workspace,
            &self.principal,
            &draft_id,
            spec,
            &format!("{draft_id}:draft"),
        )?;
        let revision = self.store.publish(
            &self.workspace,
            &self.principal,
            &draft_id,
            1,
            &format!("{draft_id}:publish"),
        )?;
        let plan = self.store.plan_update(
            &self.workspace,
            &self.principal,
            proofstorm_core::CellUpdateTarget {
                delete_retained: delete_retained.to_vec(),
                instance_id: instance.id,
                expected_generation: generation,
                delete_data,
            },
            &revision,
        )?;
        self.store
            .save_update_plan(&self.workspace, &self.principal, &draft_id, &plan)?;
        Ok(plan)
    }

    pub async fn edit(
        &self,
        name: &str,
        spec: &CellSpec,
        delete_data: bool,
        delete_retained: &[String],
    ) -> Result<CellView, Error> {
        let plan = self.plan_edit(name, spec, delete_data, delete_retained)?;
        let applied = self
            .apply_update("", &plan, &format!("edit:{}", plan.digest))
            .await?;
        let mut view = self.inspect(name, 0).await.map_err(|error| Error::failure(
            format!("Edit accepted at generation {}; current status unavailable: {}", applied.instance.generation, error.message),
            Some(serde_json::json!({"code":"cell_edit_accepted_status_unavailable", "accepted":true,
                "instance_id":applied.instance.id, "generation":applied.instance.generation,
                "reconciliation_error":applied.reconciliation_error,
                "recovery":"inspect the cell or repeat up with the same desired configuration"})),
        ))?;
        view.reconciliation_error = applied.reconciliation_error;
        Ok(view)
    }
}
