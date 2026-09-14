//! Apply the exact immutable revision that was previewed, through shared lifecycle admission.
use super::{AppliedCell, Cells, UpResult};
use crate::Error;
use proofstorm_store::CellPreview;

impl Cells {
    pub async fn up_preview(&self, preview: &CellPreview) -> Result<UpResult, Error> {
        self.authorize(
            proofstorm_core::mcp::tool("cell_up")
                .ok_or_else(|| {
                    Error::problem(
                        "invalid_tool_registry",
                        "cell_up is missing from the public registry",
                    )
                })?
                .capabilities,
        )?;
        let _guard = crate::lifecycle::guard(&self.store).await?;
        if self
            .store
            .cell_preview(&self.workspace, &self.principal, &preview.id)?
            .as_ref()
            != Some(preview)
        {
            return Err(Error::problem(
                "cell_plan_digest_mismatch",
                "Preview differs from its immutable record",
            ));
        }
        let (cell, applied) = if let Some(plan) = &preview.update {
            let cell = self.resolve(&preview.name)?;
            if cell.instance_id != plan.target.instance_id {
                return Err(Error::problem(
                    "stale_incarnation",
                    "This preview targets a replaced cell",
                ));
            }
            let applied = self.apply_update(&preview.id, plan, &preview.id).await?;
            (cell, applied)
        } else {
            let cell = self
                .store
                .reserve_preview(&self.workspace, &self.principal, preview)?;
            let revision = self.store.revision_for_materialize(
                &self.workspace,
                &self.principal,
                &preview.revision_digest,
            )?;
            self.prepare_images(&revision).await?;
            let instance = self.store.materialize(
                &self.workspace,
                &self.principal,
                &cell.instance_id,
                &revision.digest,
                &format!("{}:materialize", preview.id),
            )?;
            // Replay never replaces a newer revision. Reconciliation uses the current desired state.
            let current = self
                .store
                .instance(&self.workspace, &self.principal, &instance.id)?;
            let current_revision = self.store.revision_for_materialize(
                &self.workspace,
                &self.principal,
                &current.revision_digest,
            )?;
            self.store.record_plan_use(&current, Some(&preview.id))?;
            let status = crate::lifecycle::materialize_locked(
                &self.runtime,
                &self.store,
                current,
                current_revision,
            )
            .await?;
            let applied = AppliedCell {
                instance: status.instance,
                phase: status.phase,
                generation: 1,
                plan_id: preview.id.clone(),
                plan_digest: proofstorm_core::digest_json(preview),
                revision_digest: revision.digest,
                lock_digest: revision.lock.digest,
                component_count: u32::try_from(revision.cell.components.len()).unwrap_or(u32::MAX),
                reconciliation_error: None,
            };
            (cell, applied)
        };
        let activity_ready = self.ensure_run(&cell).is_ok();
        Ok(UpResult {
            cell,
            applied,
            activity_ready,
        })
    }
}
