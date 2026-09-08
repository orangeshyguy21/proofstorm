//! Bounded observation of one incarnation and desired generation, shared by transports.
use super::Labs;
use crate::{Error, ErrorKind};
use proofstorm_core::{Capability, InstancePhase, LabInstance, LabInstanceStatus};
use proofstorm_store::StoreError;
use std::time::Duration;

pub struct WaitRequest<'a> {
    pub reference: &'a str,
    pub expected_instance_key: Option<&'a str>,
    pub expected_generation: Option<u64>,
    pub target_phase: InstancePhase,
    pub timeout_seconds: u32,
}

pub struct WaitResult {
    pub status: LabInstanceStatus,
    pub reached: bool,
    pub timed_out: bool,
    pub superseded: bool,
}

#[must_use]
pub const fn wait_terminal(phase: InstancePhase) -> bool {
    matches!(
        phase,
        InstancePhase::Closed | InstancePhase::CleanupBlocked | InstancePhase::Blocked
    )
}

fn blocked(status: &LabInstanceStatus) -> bool {
    status.components.iter().any(|component| {
        component.conditions.iter().any(|condition| {
            condition.state == proofstorm_core::ComponentConditionState::False
                && condition.reason.blocks_startup()
        })
    })
}

impl Labs {
    fn wait_identity(&self, request: &WaitRequest<'_>) -> Result<LabInstance, Error> {
        match self.resolve_instance(request.reference) {
            Ok(instance) => {
                if request
                    .expected_instance_key
                    .is_some_and(|key| key != instance.instance_key)
                {
                    return Err(Error::problem(
                        "stale_incarnation",
                        "The lab was replaced; read its current status",
                    ));
                }
                Ok(instance)
            }
            Err(error)
                if error.kind == ErrorKind::Missing
                    && request.target_phase == InstancePhase::Closed =>
            {
                let Some(key) = request.expected_instance_key else {
                    return Err(error);
                };
                if !(key.starts_with('i')
                    && key.len() == 20
                    && key[1..].bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err(Error::problem(
                        "invalid_operation",
                        "invalid expected_instance_key",
                    ));
                }
                Ok(LabInstance {
                    id: request.reference.into(),
                    workspace_id: self.workspace.clone(),
                    instance_key: key.into(),
                    resource_name: format!("lab-{key}"),
                    revision_digest: String::new(),
                    lock_digest: String::new(),
                    generation: 0,
                })
            }
            Err(error) => Err(error),
        }
    }

    async fn observe_tracked(
        &self,
        tracked: &LabInstance,
        target: InstancePhase,
    ) -> Result<LabInstanceStatus, Error> {
        let current = match self
            .store
            .instance(&self.workspace, &self.principal, &tracked.id)
        {
            Ok(current) if current.instance_key == tracked.instance_key => current,
            Ok(_) | Err(StoreError::NotFound { .. }) if target == InstancePhase::Closed => {
                return self.runtime.verify_absent(tracked.clone()).await;
            }
            Ok(_) => {
                return Err(Error::problem(
                    "stale_incarnation",
                    "The lab was replaced; read its current status",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        match self.runtime.status(current).await {
            Err(error) if error.kind == ErrorKind::Missing && target == InstancePhase::Closed => {
                self.runtime.verify_absent(tracked.clone()).await
            }
            result => result,
        }
    }

    pub async fn wait(&self, request: WaitRequest<'_>) -> Result<WaitResult, Error> {
        self.authorize(&[Capability::LabStatus])?;
        if !(1..=120).contains(&request.timeout_seconds) {
            return Err(Error::problem(
                "wait_timeout_invalid",
                "timeout_seconds must be in 1..=120",
            ));
        }
        let tracked = self.wait_identity(&request)?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(u64::from(request.timeout_seconds));
        let mut backoff = Duration::from_millis(250);
        let mut last = None;
        let mut expected_generation = request.expected_generation;
        loop {
            let status = match tokio::time::timeout_at(deadline, self.observe_tracked(&tracked, request.target_phase)).await {
                Ok(result) => result?,
                Err(_) => return last.map_or_else(
                    || Err(Error::problem("lab_wait_deadline_exceeded", "the runtime status backend did not answer before the requested lab wait deadline")),
                    |status| Ok(WaitResult { status, reached: false, timed_out: true, superseded: false })),
            };
            if status.phase == InstancePhase::Closed {
                let _guard = crate::lifecycle::guard(&self.store).await?;
                // Recheck under the guard: a reused name must not purge the replacement.
                if self
                    .store
                    .instance(&self.workspace, &self.principal, &tracked.id)
                    .is_ok_and(|current| current.instance_key == tracked.instance_key)
                {
                    crate::lifecycle::reconcile_name(
                        &self.runtime,
                        &self.store,
                        &self.workspace,
                        &self.principal,
                        &tracked.id,
                    )
                    .await?;
                }
            }
            let expected = *expected_generation.get_or_insert(status.instance.generation);
            if request.target_phase == InstancePhase::Ready
                && expected != status.instance.generation
            {
                return Ok(WaitResult {
                    status,
                    reached: false,
                    timed_out: false,
                    superseded: true,
                });
            }
            let reached = status.phase == request.target_phase;
            if reached && status.phase == InstancePhase::Ready {
                self.store.mark_update_applied(
                    &self.workspace,
                    &tracked.id,
                    status.instance.generation,
                    Some(&status.instance.revision_digest),
                )?;
            }
            if reached
                || wait_terminal(status.phase)
                || (request.target_phase == InstancePhase::Ready && blocked(&status))
            {
                return Ok(WaitResult {
                    status,
                    reached,
                    timed_out: false,
                    superseded: false,
                });
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(WaitResult {
                    status,
                    reached: false,
                    timed_out: true,
                    superseded: false,
                });
            }
            last = Some(status);
            tokio::time::sleep(
                backoff.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
            backoff = (backoff * 2).min(Duration::from_secs(2));
        }
    }
}
