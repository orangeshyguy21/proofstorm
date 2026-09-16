//! Named-cell convenience: prepare a specification and enter the shared apply path.
use super::{AppliedCell, CellView, Cells};
use crate::{Error, ErrorKind};
use proofstorm_core::{Capability, CellSpec};
use proofstorm_store::{CellHandle, CellHandlePhase, StoreError};

/// Durable admission, without fetching status, activity or sessions for presentation.
#[derive(Debug)]
pub struct UpResult {
    pub cell: CellHandle,
    pub applied: AppliedCell,
    pub activity_ready: bool,
}

impl Cells {
    pub async fn up(&self, name: &str, spec: &CellSpec) -> Result<CellView, Error> {
        let accepted = self.up_accepted(name, spec, None, None).await?;
        let mut view = self.inspect(name, 0).await?;
        view.reconciliation_error = accepted.applied.reconciliation_error;
        Ok(view)
    }

    /// Stages are idempotent and resumable, not a cross-system transaction.
    /// Preconditions fence an existing cell; omit them for creation. Supplying
    /// the instance key as well as the generation also fences replacement.
    #[allow(
        clippy::too_many_lines,
        reason = "creation stages share one lifecycle guard"
    )]
    pub async fn up_accepted(
        &self,
        name: &str,
        spec: &CellSpec,
        expected_generation: Option<u64>,
        expected_instance_key: Option<&str>,
    ) -> Result<UpResult, Error> {
        self.authorize(&[
            Capability::CellCreate,
            Capability::CellRead,
            Capability::CellPublish,
            Capability::CellMaterialize,
            Capability::CellStatus,
            Capability::CatalogRead,
            Capability::ExperimentRead,
            Capability::CellOperate,
        ])?;
        let _lifecycle = crate::lifecycle::guard(&self.store).await?;
        if expected_generation.is_some() || expected_instance_key.is_some() {
            let cell = self
                .resolve(name)
                .and_then(|cell| self.instance(&cell).map(|instance| (cell, instance)))
                .map_err(|error| {
                    if error.kind == ErrorKind::Missing {
                        Error::problem(
                            "cell_update_conflict",
                            "Expected an existing cell; no change accepted",
                        )
                    } else {
                        error
                    }
                })?;
            if expected_generation == Some(0) {
                return Err(Error::problem(
                    "cell_generation_invalid",
                    "expected_generation must be positive",
                ));
            }
            if expected_instance_key.is_some_and(|key| key != cell.1.instance_key) {
                return Err(Error::problem(
                    "stale_incarnation",
                    "This name refers to a different cell incarnation; inspect it before editing",
                ));
            }
            return self.up_edit(&cell.0, spec, expected_generation).await;
        }
        match self.resolve(name) {
            Ok(handle) => {
                crate::lifecycle::reconcile_name(
                    &self.runtime,
                    &self.store,
                    &self.workspace,
                    &self.principal,
                    &handle.instance_id,
                )
                .await?;
                match self.resolve(name) {
                    Ok(current) => match self.instance(&current) {
                        Ok(instance)
                            if self
                                .store
                                .runtime_binding(&instance)?
                                .is_some_and(|binding| binding.resource_uid.is_none())
                                && instance.generation == 1 => {}
                        Ok(_) => {
                            if current.phase != CellHandlePhase::Open {
                                return Err(Error::problem(
                                    "cell_closing",
                                    "finish closing this cell before starting it again",
                                ));
                            }
                            return self.up_edit(&current, spec, None).await;
                        }
                        Err(error) if error.kind == ErrorKind::Missing => {}
                        Err(error) => return Err(error),
                    },
                    Err(error) if error.kind == ErrorKind::Missing => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) if error.kind == ErrorKind::Missing => {}
            Err(error) => return Err(error),
        }
        let catalog = self
            .store
            .effective_catalog(&self.workspace, &self.principal)?;
        let report = proofstorm_core::validate_cell(spec);
        if !report.valid {
            return Err(Error::problem(
                "cell_invalid",
                serde_json::to_string(&report.issues).unwrap_or_default(),
            ));
        }
        let effective = proofstorm_core::resolve_effective_cell(spec, &catalog)
            .map_err(|e| Error::problem("cell_invalid", e.to_string()))?;
        proofstorm_core::validate_new_cell_versions(&effective, &catalog)
            .map_err(|e| Error::problem("cell_invalid", e))?;
        proofstorm_core::resolve_lock(&effective, &catalog)
            .map_err(|e| Error::problem("cell_invalid", e.to_string()))?;
        let digest = proofstorm_core::digest_json(spec);
        let cell = self
            .store
            .reserve_cell(&self.workspace, &self.principal, name, &digest)?;
        if cell.phase != CellHandlePhase::Open {
            return Err(Error::problem(
                "cell_closing",
                "finish closing this cell before starting it again",
            ));
        }
        let draft_id = format!("draft-{}", cell.instance_id);
        match self.store.create_draft(
            &self.workspace,
            &self.principal,
            &draft_id,
            spec,
            &format!("{draft_id}:create"),
        ) {
            Ok(_) => {}
            Err(StoreError::Conflict {
                resource: "draft", ..
            }) => {
                // Recover a crash between inserting a draft and recording its receipt.
                let draft = self
                    .store
                    .read_draft(&self.workspace, &self.principal, &draft_id)?;
                if draft.cell != *spec {
                    return Err(Error::problem(
                        "cell_config_conflict",
                        "stored draft differs from requested cell",
                    ));
                }
            }
            Err(e) => return Err(e.into()),
        }
        let applied = self.apply_draft_locked(&cell.instance_id, &draft_id, &digest, &cell.instance_id)
            .await.map_err(|mut error| {
                error.details = Some(serde_json::json!({"code":"cell_materialization_incomplete",
                    "cell":name,"stage":"published","recovery":"repeat up with the same name and configuration"}));
                error
            })?;
        // Activity setup cannot turn already accepted configuration into a
        // failed mutation. The receipt exposes whether it needs another try.
        let activity_ready = self.ensure_run(&cell).is_ok();
        Ok(UpResult {
            cell,
            applied,
            activity_ready,
        })
    }

    async fn up_edit(
        &self,
        cell: &CellHandle,
        spec: &CellSpec,
        expected_generation: Option<u64>,
    ) -> Result<UpResult, Error> {
        if cell.phase != CellHandlePhase::Open {
            return Err(Error::problem(
                "cell_closing",
                "finish closing this cell before editing it",
            ));
        }
        let plan = self.plan_edit_at(&cell.name, spec, false, &[], expected_generation)?;
        let applied = self
            .apply_update("", &plan, &format!("edit:{}", plan.digest))
            .await?;
        let activity_ready = self.ensure_run(cell).is_ok();
        Ok(UpResult {
            cell: cell.clone(),
            applied,
            activity_ready,
        })
    }
}
