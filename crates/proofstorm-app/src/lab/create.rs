//! Named-lab convenience: prepare a specification and enter the shared apply path.
use super::{LabView, Labs};
use crate::{Error, ErrorKind};
use proofstorm_core::{Capability, LabSpec};
use proofstorm_store::{LabHandlePhase, StoreError};

impl Labs {
    /// Stages are idempotent and resumable, not a cross-system transaction.
    #[allow(
        clippy::too_many_lines,
        reason = "creation stages share one lifecycle guard"
    )]
    pub async fn up(&self, name: &str, spec: &LabSpec) -> Result<LabView, Error> {
        self.authorize(&[
            Capability::LabCreate,
            Capability::LabRead,
            Capability::LabPublish,
            Capability::LabMaterialize,
            Capability::LabStatus,
            Capability::CatalogRead,
            Capability::ExperimentRead,
            Capability::LabOperate,
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
                            if current.phase != LabHandlePhase::Open {
                                return Err(Error::problem(
                                    "lab_closing",
                                    "finish closing this lab before starting it again",
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
        let report = proofstorm_core::validate_lab(spec);
        if !report.valid {
            return Err(Error::problem(
                "lab_invalid",
                serde_json::to_string(&report.issues).unwrap_or_default(),
            ));
        }
        let effective = proofstorm_core::resolve_effective_lab(spec, &catalog)
            .map_err(|e| Error::problem("lab_invalid", e.to_string()))?;
        proofstorm_core::resolve_lock(&effective, &catalog)
            .map_err(|e| Error::problem("lab_invalid", e.to_string()))?;
        let digest = proofstorm_core::digest_json(spec);
        let lab = self
            .store
            .reserve_lab(&self.workspace, &self.principal, name, &digest)?;
        if lab.phase != LabHandlePhase::Open {
            return Err(Error::problem(
                "lab_closing",
                "finish closing this lab before starting it again",
            ));
        }
        let draft_id = format!("draft-{}", lab.instance_id);
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
                if draft.lab != *spec {
                    return Err(Error::problem(
                        "lab_config_conflict",
                        "stored draft differs from requested lab",
                    ));
                }
            }
            Err(e) => return Err(e.into()),
        }
        self.apply_draft_locked(&lab.instance_id, &draft_id, &digest, &lab.instance_id)
            .await.map_err(|mut error| {
                error.details = Some(serde_json::json!({"code":"lab_materialization_incomplete",
                    "lab":name,"stage":"published","recovery":"repeat up with the same name and configuration"}));
                error
            })?;
        self.ensure_run(&lab)?;
        self.inspect(name, 0).await
    }
}
