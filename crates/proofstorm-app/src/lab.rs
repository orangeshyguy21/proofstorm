//! The normal lab lifecycle. Durable names hide coordination IDs, not authority.
use crate::{Error, ErrorKind, Runtime, journal, runtime::runtime_action_resource};
use proofstorm_core::{
    Capability, Experiment, InstancePhase, LabInstance, LabInstanceStatus, LabOperation, LabSpec,
    OperationKind, OperationPhase, Session, native::NativeCommand,
};
use proofstorm_kube::{ComponentExecLiveAction, LabAction};
use proofstorm_store::{LabHandle, LabHandlePhase, Store, StoreError};
use schemars::JsonSchema;
use serde::Serialize;
use std::time::Duration;

#[derive(Clone)]
pub struct Labs {
    pub store: Store,
    pub runtime: Runtime,
    pub workspace: String,
    pub principal: String,
}

pub use proofstorm_view::Activity;

#[derive(Debug, Serialize, JsonSchema)]
pub struct LabView {
    pub lab: LabHandle,
    pub runtime: Option<LabInstanceStatus>,
    pub run: Option<Experiment>,
    pub sessions: proofstorm_store::SessionPage,
    pub activity: Vec<Activity>,
    pub next_sequence: Option<u64>,
    pub observed_at_unix: i64,
}

impl Labs {
    #[must_use]
    pub fn new(store: Store, runtime: Runtime, workspace: String, principal: String) -> Self {
        Self {
            store,
            runtime,
            workspace,
            principal,
        }
    }

    fn authorize(&self, capabilities: &[Capability]) -> Result<(), Error> {
        for capability in capabilities {
            self.store
                .authorize(&self.workspace, &self.principal, *capability)?;
        }
        Ok(())
    }

    fn owned(&self, name: &str) -> Result<LabHandle, Error> {
        let lab = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)?;
        if lab.owner != self.principal {
            return Err(Error::problem(
                "lab_owner_mismatch",
                "lab belongs to another principal",
            ));
        }
        Ok(lab)
    }

    fn instance(&self, lab: &LabHandle) -> Result<LabInstance, Error> {
        Ok(self
            .store
            .instance(&self.workspace, &self.principal, &lab.instance_id)?)
    }

    pub fn plan_edit(
        &self,
        name: &str,
        spec: &LabSpec,
        delete_data: bool,
        delete_retained: &[String],
    ) -> Result<proofstorm_core::LabUpdatePlan, Error> {
        let handle = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)?;
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
        if !plan.is_noop() {
            self.store.accept_update(
                &self.workspace,
                &self.principal,
                &plan,
                &format!("edit:{}", plan.digest),
            )?;
        }
        crate::updates::reconcile(
            &self.runtime,
            &self.store,
            &self.workspace,
            &self.principal,
            &plan.target.instance_id,
        )
        .await?;
        self.inspect(name, 0).await
    }

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
            Capability::ExperimentCreate,
            Capability::ExperimentRead,
            Capability::LabOperate,
        ])?;
        let _lifecycle = crate::lifecycle::guard(&self.store).await?;
        if let Ok(handle) = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)
        {
            crate::lifecycle::reconcile_name(
                &self.runtime,
                &self.store,
                &self.workspace,
                &self.principal,
                &handle.instance_id,
            )
            .await?;
        }
        if let Ok(handle) = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)
        {
            if handle.phase == LabHandlePhase::Open
                && self.instance(&handle).is_ok_and(|instance| {
                    instance.generation > 1
                        || handle.config_digest != proofstorm_core::digest_json(spec)
                })
            {
                return self.edit(name, spec, false, &[]).await;
            }
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
        let revision = self.store.publish(
            &self.workspace,
            &self.principal,
            &draft_id,
            1,
            &format!("{draft_id}:publish"),
        )?;
        let instance = self.store.materialize(
            &self.workspace,
            &self.principal,
            &lab.instance_id,
            &revision.digest,
            &format!("{}:materialize", lab.instance_id),
        )?;
        crate::lifecycle::materialize_locked(&self.runtime,&self.store,instance,revision).await.map_err(|mut e| {
            e.details=Some(serde_json::json!({"code":"lab_materialization_incomplete","lab":name,"stage":"published","recovery":"repeat up with the same name and configuration"}));e
        })?;
        self.ensure_run(&lab)?;
        self.inspect(name, 0).await
    }

    fn ensure_run(&self, lab: &LabHandle) -> Result<Session, Error> {
        if optional(
            self.store
                .experiment(&self.workspace, &self.principal, &lab.run_id()),
        )?
        .is_none()
        {
            self.store.create_experiment(
                &self.workspace,
                &self.principal,
                &lab.run_id(),
                &lab.instance_id,
                &format!("{}:create", lab.run_id()),
            )?;
        }
        Ok(self
            .store
            .track_session(&self.workspace, &self.principal, &lab.run_id(), "")?)
    }

    /// Pure observation: no jobs, or journal synchronization.
    pub async fn inspect(&self, name: &str, after_sequence: u64) -> Result<LabView, Error> {
        self.authorize(&[Capability::LabStatus, Capability::ExperimentRead])?;
        let lab = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)?;
        let runtime = match self.instance(&lab) {
            Ok(instance) => match self.runtime.status(instance.clone()).await {
                Ok(status) => Some(status),
                Err(e) if e.kind == ErrorKind::Missing && lab.phase == LabHandlePhase::Closed => {
                    Some(self.runtime.verify_absent(instance).await?)
                }
                Err(e) if e.kind == ErrorKind::Missing => None,
                Err(e) => return Err(e),
            },
            Err(e) if e.kind == ErrorKind::Missing => None,
            Err(e) => return Err(e),
        };
        let run = optional(
            self.store
                .experiment(&self.workspace, &self.principal, &lab.run_id()),
        )?;
        let sessions =
            self.store
                .sessions(&self.workspace, &self.principal, &lab.instance_id, "", 20)?;
        let activity = if run.is_some() {
            self.store
                .actions(
                    &self.workspace,
                    &self.principal,
                    &lab.run_id(),
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
        let lab = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)?;
        if optional(
            self.store
                .experiment(&self.workspace, &self.principal, &lab.run_id()),
        )?
        .is_none()
        {
            return Ok(Vec::new());
        }
        journal::reconcile(
            &self.runtime,
            &self.store,
            &self.workspace,
            &self.principal,
            &lab.run_id(),
        )
        .await
    }

    pub async fn exec(
        &self,
        name: &str,
        component: &str,
        command: NativeCommand,
        request_id: &str,
    ) -> Result<LabOperation, Error> {
        self.authorize(&[Capability::ComponentExecLive, Capability::ArtifactRead])?;
        command
            .validate()
            .map_err(|e| Error::problem("invalid_operation", e))?;
        if command.private_io.is_some() {
            return Err(Error::problem(
                "private_binding_unsupported",
                "use the explicit private-transfer surface for custody input",
            ));
        }
        let lab = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)?;
        if lab.phase != LabHandlePhase::Open {
            return Err(Error::problem(
                "lab_closing",
                "new actions are not admitted while closing",
            ));
        }
        self.ensure_run(&lab)?;
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &lab.instance_id,
            request_id,
            Capability::ComponentExecLive,
        )?;
        if !revision.lab.components.iter().any(|c| c.id == component) {
            return Err(Error::problem(
                "component_not_found",
                "component is not part of this lab",
            ));
        }
        let request = serde_json::json!({"component":component,"script":command.script,"argv":command.argv,"timeout_seconds":command.timeout_seconds,"output":command.output});
        let op = self.store.create_operation_at_revision(
            &revision.digest,
            &self.workspace,
            &self.principal,
            &instance.id,
            &lab.run_id(),
            "",
            request_id,
            OperationKind::ComponentExecLive,
            &request,
            request_id,
            Capability::ComponentExecLive,
        )?;
        let op = self
            .store
            .operation(&self.workspace, &self.principal, &op.id)?;
        if op.phase != OperationPhase::Pending {
            return Ok(op);
        }
        let action = runtime_action_resource(
            &self.runtime.control_namespace,
            &instance,
            &op,
            LabAction::ComponentExecLive(ComponentExecLiveAction {
                private_payload: None,
                component: component.into(),
                script: command.script,
                argv: command.argv,
                timeout_seconds: command.timeout_seconds,
                output: command.output,
            }),
        );
        self.runtime.apply_action(&instance, &action).await?;
        Ok(self
            .store
            .update_operation_phase(&self.workspace, &op.id, OperationPhase::Running)?)
    }

    /// Revoke admission, collect/cancel owned work, preserve receipts, and verify teardown.
    /// A timeout leaves a closing lab which the same call can safely resume.
    #[allow(
        clippy::too_many_lines,
        reason = "keep the ordered, resumable shutdown stages visible together"
    )]
    pub async fn down(&self, name: &str, timeout_seconds: u32) -> Result<LabView, Error> {
        self.down_checked(name, timeout_seconds, None).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "shutdown and purge are one serialized lifecycle transition"
    )]
    pub async fn down_checked(
        &self,
        name: &str,
        timeout_seconds: u32,
        expected_instance: Option<&str>,
    ) -> Result<LabView, Error> {
        self.authorize(&[
            Capability::LabClose,
            Capability::ExperimentRead,
            Capability::ExperimentClose,
            Capability::ExperimentRead,
            Capability::ArtifactRead,
            Capability::ActionCancel,
        ])?;
        let _lifecycle = crate::lifecycle::guard(&self.store).await?;
        let lab = self.owned(name)?;
        if expected_instance.is_some_and(|id| id != lab.instance_id) {
            return Err(Error::problem(
                "stale_incarnation",
                "The named lab was replaced; inspect it before closing",
            ));
        }

        if lab.phase == LabHandlePhase::Closed {
            return self.inspect(name, 0).await;
        }
        self.store
            .begin_instance_close(&self.workspace, &self.principal, &lab.instance_id)?;
        self.store.set_lab_phase(
            &self.workspace,
            &self.principal,
            &lab,
            LabHandlePhase::Closing,
        )?;
        self.store
            .finish_lab_sessions(&self.workspace, &self.principal, &lab.instance_id)?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(u64::from(timeout_seconds));
        loop {
            let pending = self.sync(name).await?;
            for op in &pending {
                if !self
                    .runtime
                    .request_action_cancellation(op, &format!("{}:close", lab.instance_id))
                    .await?
                {
                    journal::record(
                        &self.store,
                        &self.workspace,
                        op,
                        OperationPhase::Cancelled,
                        serde_json::json!({"code":"lab_closed_without_runtime_receipt","message":"new admission revoked; runtime outcome was not observed","cleanup_verified":false}),
                    )?;
                }
            }
            if pending.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::problem(
                    "lab_close_pending",
                    "cancellation requested; repeat down to collect receipts and finish cleanup",
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if optional(
            self.store
                .experiment(&self.workspace, &self.principal, &lab.run_id()),
        )?
        .is_some()
        {
            self.store.close_experiment(
                &self.workspace,
                &self.principal,
                &lab.run_id(),
                &format!("{}:close", lab.run_id()),
            )?;
        }
        match self.instance(&lab) {
            Ok(instance) => {
                let instance = self.store.instance_for_close(
                    &self.workspace,
                    &self.principal,
                    &instance.id,
                )?;
                let mut status = self.runtime.close(instance.clone()).await?;
                while status.phase != InstancePhase::Closed {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(Error::problem(
                            "lab_close_pending",
                            "teardown in progress; repeat down to verify absence",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    status = self.runtime.close(instance.clone()).await?;
                }
                if !status
                    .teardown_receipt
                    .as_ref()
                    .is_some_and(|r| r.verified_absent)
                {
                    return Err(Error::problem(
                        "cleanup_unverified",
                        "runtime did not verify lab absence",
                    ));
                }
            }
            Err(e) if e.kind == ErrorKind::Missing => {}
            Err(e) => return Err(e),
        }
        self.store.set_lab_phase(
            &self.workspace,
            &self.principal,
            &lab,
            LabHandlePhase::Closed,
        )?;
        let view = self.inspect(name, 0).await?;
        crate::lifecycle::reconcile_name(
            &self.runtime,
            &self.store,
            &self.workspace,
            &self.principal,
            &lab.instance_id,
        )
        .await?;
        Ok(view)
    }
}

fn optional<T>(result: Result<T, StoreError>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(StoreError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
