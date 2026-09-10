//! Live-edit convenience over the same durable plans used by reviewed apply.
use super::{LabView, Labs};
use crate::Error;
use proofstorm_core::LabSpec;

impl Labs {
    pub fn plan_edit(
        &self,
        name: &str,
        spec: &LabSpec,
        delete_data: bool,
        delete_retained: &[String],
    ) -> Result<proofstorm_core::LabUpdatePlan, Error> {
        let handle = self.resolve(name)?;
        let instance = self.instance(&handle)?;
        let draft_id = format!(
            "edit-{}",
            &proofstorm_core::digest_json(&(
                instance.id.clone(),
                instance.generation,
                spec,
                delete_data,
                delete_retained
            ))[7..39]
        );
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
            proofstorm_core::LabUpdateTarget {
                delete_retained: delete_retained.to_vec(),
                instance_id: instance.id,
                expected_generation: instance.generation,
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
        spec: &LabSpec,
        delete_data: bool,
        delete_retained: &[String],
    ) -> Result<LabView, Error> {
        let plan = self.plan_edit(name, spec, delete_data, delete_retained)?;
        let applied = self
            .apply_update("", &plan, &format!("edit:{}", plan.digest))
            .await?;
        let mut view = self.inspect(name, 0).await.map_err(|error| Error::failure(
            format!("Edit accepted at generation {}; current status unavailable: {}", applied.instance.generation, error.message),
            Some(serde_json::json!({"code":"lab_edit_accepted_status_unavailable", "accepted":true,
                "instance_id":applied.instance.id, "generation":applied.instance.generation,
                "reconciliation_error":applied.reconciliation_error,
                "recovery":"inspect the lab or repeat up with the same desired configuration"})),
        ))?;
        view.reconciliation_error = applied.reconciliation_error;
        Ok(view)
    }
}
