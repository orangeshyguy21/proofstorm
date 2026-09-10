//! Pure inspection and explicit synchronization of recorded activity.
use super::{Activity, LabView, Labs, now, optional};
use crate::{Error, ErrorKind, journal};
use proofstorm_core::{Capability, LabOperation, Session};
use proofstorm_store::{LabHandle, LabHandlePhase};

impl Labs {
    pub(super) fn run(
        &self,
        lab: &LabHandle,
    ) -> Result<Option<proofstorm_core::Experiment>, Error> {
        let Some(id) = optional(self.store.default_run_id(
            &self.workspace,
            &self.principal,
            &lab.instance_id,
        ))?
        else {
            return Ok(None);
        };
        optional(self.store.experiment(&self.workspace, &self.principal, &id))
    }

    pub(super) fn ensure_run(&self, lab: &LabHandle) -> Result<Session, Error> {
        let run = self.store.ensure_default_run(
            &self.workspace,
            &self.principal,
            &lab.instance_id,
            Capability::LabOperate,
        )?;
        Ok(self
            .store
            .track_session(&self.workspace, &self.principal, &run.id, "")?)
    }

    /// Pure observation: no jobs, or journal synchronization.
    pub async fn inspect(&self, name: &str, after_sequence: u64) -> Result<LabView, Error> {
        self.authorize(&[Capability::LabStatus, Capability::ExperimentRead])?;
        let lab = self.resolve(name)?;
        let instance = match self.instance(&lab) {
            Ok(instance) => Some(instance),
            Err(error) if error.kind == ErrorKind::Missing => None,
            Err(error) => return Err(error),
        };
        let instance_key = instance
            .as_ref()
            .map(|instance| instance.instance_key.clone());
        let runtime = match instance {
            Some(instance) => match self.runtime.status(instance.clone()).await {
                Ok(status) => Some(status),
                Err(e) if e.kind == ErrorKind::Missing && lab.phase == LabHandlePhase::Closed => {
                    Some(self.runtime.verify_absent(instance).await?)
                }
                Err(e) if e.kind == ErrorKind::Missing => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        let run = self.run(&lab)?;
        let sessions =
            self.store
                .sessions(&self.workspace, &self.principal, &lab.instance_id, "", 20)?;
        let activity = if let Some(run) = &run {
            self.store
                .actions(
                    &self.workspace,
                    &self.principal,
                    &run.id,
                    after_sequence,
                    20,
                )?
                .into_iter()
                .map(Activity::from)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let next_sequence = activity
            .last()
            .filter(|_| activity.len() == 20)
            .map(|item| item.sequence);
        Ok(LabView {
            lab,
            instance_key,
            reconciliation_error: None,
            runtime,
            run,
            sessions,
            activity,
            next_sequence,
            observed_at_unix: now(),
        })
    }

    pub async fn sync(&self, name: &str) -> Result<Vec<LabOperation>, Error> {
        self.authorize(&[Capability::ArtifactRead, Capability::ExperimentRead])?;
        let lab = self.resolve(name)?;
        let Some(run) = self.run(&lab)? else {
            return Ok(Vec::new());
        };
        journal::reconcile(
            &self.runtime,
            &self.store,
            &self.workspace,
            &self.principal,
            &run.id,
        )
        .await
    }
}
