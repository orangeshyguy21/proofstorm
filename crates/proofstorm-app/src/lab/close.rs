//! One shutdown path: fence the incarnation, revoke admission, collect work, verify absence.
use super::{LabView, Labs, now};
use crate::{Error, ErrorKind, journal};
use proofstorm_core::{Capability, InstancePhase, LabInstance, LabInstanceStatus, OperationPhase};
use proofstorm_store::LabHandlePhase;
use std::time::Duration;

impl Labs {
    /// Advance teardown once. Polling policy belongs to the calling transport.
    pub async fn close(
        &self,
        reference: &str,
        expected_key: &str,
    ) -> Result<LabInstanceStatus, Error> {
        self.authorize(&[Capability::LabClose])?;
        let _guard = crate::lifecycle::guard(&self.store).await?;
        let instance = self.resolve_instance(reference)?;
        if instance.instance_key != expected_key {
            return Err(Error::problem(
                "stale_incarnation",
                "The lab was replaced; read its status before closing",
            ));
        }
        crate::lifecycle::validate_runtime(&self.runtime, &self.store, &instance).await?;
        self.store
            .begin_instance_close(&self.workspace, &self.principal, &instance.id)?;
        self.store
            .finish_lab_sessions(&self.workspace, &self.principal, &instance.id)?;
        self.finalize_operations(&instance).await?;
        let status = self.runtime.close(instance.clone()).await?;
        if status.phase == InstancePhase::Closed {
            if !status
                .teardown_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.verified_absent)
            {
                return Err(Error::problem(
                    "cleanup_unverified",
                    "runtime did not verify lab absence",
                ));
            }
            crate::lifecycle::reconcile_name(
                &self.runtime,
                &self.store,
                &self.workspace,
                &self.principal,
                &instance.id,
            )
            .await?;
        }
        Ok(status)
    }

    async fn finalize_operations(&self, instance: &LabInstance) -> Result<(), Error> {
        for operation in self
            .store
            .active_operations(&self.workspace, &instance.id)?
        {
            // Preserve a terminal runtime result before teardown removes its evidence.
            if let Some((phase, artifact)) = self.runtime.action_status(&operation).await? {
                journal::record(&self.store, &self.workspace, &operation, phase, artifact)?;
                continue;
            }
            let token =
                proofstorm_core::digest_json(&(&instance.instance_key, &operation.id, "lab_close"));
            self.runtime
                .request_action_cancellation(&operation, &token)
                .await?;
            journal::record(
                &self.store,
                &self.workspace,
                &operation,
                OperationPhase::Cancelled,
                serde_json::json!({"code":"lab_closed_without_runtime_receipt",
                    "message":"admission revoked; teardown requested before a terminal runtime result was observed",
                    "cleanup_verified":false}),
            )?;
        }
        Ok(())
    }

    pub async fn down(&self, reference: &str, timeout_seconds: u32) -> Result<LabView, Error> {
        self.down_checked(reference, timeout_seconds, None).await
    }

    pub async fn down_checked(
        &self,
        reference: &str,
        timeout_seconds: u32,
        expected_key: Option<&str>,
    ) -> Result<LabView, Error> {
        self.authorize(&[Capability::LabClose])?;
        let mut view = self.inspect(reference, 0).await?;
        if expected_key.is_some_and(|key| view.instance_key.as_deref() != Some(key)) {
            return Err(Error::problem(
                "stale_incarnation",
                "The lab was replaced; inspect it before closing",
            ));
        }
        let instance = match self.instance(&view.lab) {
            Ok(instance) => instance,
            Err(error) if error.kind == ErrorKind::Missing => {
                let _guard = crate::lifecycle::guard(&self.store).await?;
                crate::lifecycle::reconcile_name(
                    &self.runtime,
                    &self.store,
                    &self.workspace,
                    &self.principal,
                    &view.lab.instance_id,
                )
                .await?;
                view.lab.phase = LabHandlePhase::Closed;
                return Ok(view);
            }
            Err(error) => return Err(error),
        };
        if view.instance_key.as_deref() != Some(instance.instance_key.as_str()) {
            return Err(Error::problem(
                "stale_incarnation",
                "The lab changed during inspection; read it again",
            ));
        }
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(u64::from(timeout_seconds));
        loop {
            let status = self.close(&instance.id, &instance.instance_key).await?;
            if status.phase == InstancePhase::Closed {
                view.lab.phase = LabHandlePhase::Closed;
                view.runtime = Some(status);
                // Lab-owned history was purged only after exact absence was verified.
                view.run = None;
                view.sessions.sessions.clear();
                view.sessions.next_cursor = None;
                view.activity.clear();
                view.next_sequence = None;
                view.observed_at_unix = now();
                return Ok(view);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::problem(
                    "lab_close_pending",
                    "teardown in progress; repeat down to verify absence",
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
