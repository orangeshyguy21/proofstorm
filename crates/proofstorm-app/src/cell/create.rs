//! Named-cell convenience: prepare a specification and enter the shared apply path.
use super::{CellView, Cells};
use crate::{Error, ErrorKind};
use proofstorm_core::{Capability, CellSpec};
use proofstorm_store::{CellHandlePhase, StoreError};

impl Cells {
    /// Stages are idempotent and resumable, not a cross-system transaction.
    #[allow(
        clippy::too_many_lines,
        reason = "creation stages share one lifecycle guard"
    )]
    pub async fn up(&self, name: &str, spec: &CellSpec) -> Result<CellView, Error> {
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
                            self.ensure_run(&current)?;
                            return self.edit(name, spec, false, &[]).await;
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
        self.apply_draft_locked(&cell.instance_id, &draft_id, &digest, &cell.instance_id)
            .await.map_err(|mut error| {
                error.details = Some(serde_json::json!({"code":"cell_materialization_incomplete",
                    "cell":name,"stage":"published","recovery":"repeat up with the same name and configuration"}));
                error
            })?;
        self.ensure_run(&cell)?;
        self.inspect(name, 0).await
    }
}
