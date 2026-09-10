//! Transport-independent admission and recovery of reviewed lab changes.
use super::Labs;
use crate::{Error, ErrorKind};
use proofstorm_core::{InstancePhase, LabInstance, LabInstanceStatus};
use proofstorm_core::{LabUpdatePlan, PublishedRevision, digest_json};
use proofstorm_store::Store;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationError {
    pub code: String,
    pub message: String,
    pub recovery: String,
}

/// Admission is durable even when immediate reconciliation fails.
#[derive(Debug, Serialize)]
pub struct AppliedLab {
    pub instance: LabInstance,
    pub phase: InstancePhase,
    pub generation: u64,
    pub plan_id: String,
    pub plan_digest: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub component_count: u32,
    pub reconciliation_error: Option<ReconciliationError>,
}

impl Labs {
    pub async fn materialize_revision(
        &self,
        reference: &str,
        revision: &str,
        key: &str,
        plan: Option<&str>,
    ) -> Result<LabInstanceStatus, Error> {
        self.authorize(&[
            proofstorm_core::Capability::LabMaterialize,
            proofstorm_core::Capability::LabStatus,
        ])?;
        let published =
            self.store
                .revision_for_materialize(&self.workspace, &self.principal, revision)?;
        self.prepare_images(&published).await?;
        let id = match self.resolve(reference) {
            Ok(lab) => lab.instance_id,
            Err(error) if error.kind == ErrorKind::Missing => reference.to_owned(),
            Err(error) => return Err(error),
        };
        crate::lifecycle::materialize(
            &self.runtime,
            &self.store,
            &self.workspace,
            &self.principal,
            &id,
            revision,
            key,
            plan,
        )
        .await
    }

    pub async fn apply_plan(
        &self,
        reference: &str,
        plan_id: &str,
        expected_digest: &str,
        key: &str,
    ) -> Result<AppliedLab, Error> {
        let reviewed = review_apply(
            &self.store,
            &self.workspace,
            &self.principal,
            reference,
            plan_id,
            expected_digest,
        )?;
        self.apply_reviewed(reviewed, key).await
    }

    pub async fn apply_reviewed(
        &self,
        reviewed: ReviewedApply,
        key: &str,
    ) -> Result<AppliedLab, Error> {
        let ReviewedApply {
            reference,
            plan_id,
            plan_digest,
            update,
        } = reviewed;
        if let Some(plan) = update {
            return self.apply_update(&plan_id, &plan, key).await;
        }
        let _guard = crate::lifecycle::guard(&self.store).await?;
        let id = match self.resolve(&reference) {
            Ok(lab) => lab.instance_id,
            Err(error) if error.kind == ErrorKind::Missing => reference.clone(),
            Err(error) => return Err(error),
        };
        crate::lifecycle::reconcile_name(
            &self.runtime,
            &self.store,
            &self.workspace,
            &self.principal,
            &id,
        )
        .await?;
        self.apply_draft_locked(&id, &plan_id, &plan_digest, key)
            .await
    }

    /// Both named up and reviewed apply enter here under the lifecycle guard.
    pub(super) async fn apply_draft_locked(
        &self,
        id: &str,
        plan_id: &str,
        plan_digest: &str,
        key: &str,
    ) -> Result<AppliedLab, Error> {
        let draft = self
            .store
            .read_draft(&self.workspace, &self.principal, plan_id)?;
        if digest_json(&draft.lab) != plan_digest {
            return Err(Error::problem(
                "lab_plan_digest_mismatch",
                "Plan changed before publication; nothing applied",
            ));
        }
        let revision = self.store.publish(
            &self.workspace,
            &self.principal,
            plan_id,
            draft.version,
            &format!("{key}:publish"),
        )?;
        self.prepare_images(&revision).await?;
        let instance = self.store.materialize(
            &self.workspace,
            &self.principal,
            id,
            &revision.digest,
            &format!("{key}:materialize"),
        )?;
        self.store.record_plan_use(&instance, Some(plan_id))?;
        let status = crate::lifecycle::materialize_locked(
            &self.runtime,
            &self.store,
            instance,
            revision.clone(),
        )
        .await?;
        Ok(applied_revision(
            plan_id,
            plan_digest.into(),
            revision,
            status,
        ))
    }

    pub async fn apply_update(
        &self,
        plan_id: &str,
        plan: &LabUpdatePlan,
        key: &str,
    ) -> Result<AppliedLab, Error> {
        let accepted = self
            .store
            .accept_update(&self.workspace, &self.principal, plan, key)?;
        let result = async {
            let revision = self.store.revision_for_materialize(
                &self.workspace,
                &self.principal,
                &plan.target_revision,
            )?;
            self.prepare_images(&revision).await?;
            crate::updates::reconcile(
                &self.runtime,
                &self.store,
                &self.workspace,
                &self.principal,
                &accepted.id,
            )
            .await
        }
        .await;
        let (instance, phase, reconciliation_error) = match result {
            Ok(status) => (status.instance, status.phase, None),
            Err(error) => {
                let current = self
                    .store
                    .instance(&self.workspace, &self.principal, &accepted.id)
                    .unwrap_or(accepted);
                let closing = self
                    .store
                    .update_state(&self.workspace, &self.principal, &current.id)
                    .is_ok_and(|state| state.closing);
                let detail = ReconciliationError {
                    code: error.details.as_ref().and_then(|d| d["code"].as_str())
                        .unwrap_or("lab_update_runtime").into(),
                    message: error.message,
                    recovery: "The edit was accepted. Retry the same plan_id and idempotency_key, or wait for recovery. Do not create another edit to retry it.".into(),
                };
                (
                    current,
                    if closing {
                        InstancePhase::Closing
                    } else {
                        InstancePhase::Pending
                    },
                    Some(detail),
                )
            }
        };
        Ok(AppliedLab {
            instance,
            phase,
            reconciliation_error,
            generation: plan.target.expected_generation + u64::from(!plan.is_noop()),
            plan_id: plan_id.into(),
            plan_digest: plan.digest.clone(),
            revision_digest: plan.target_revision.clone(),
            lock_digest: plan.target_lock.clone(),
            component_count: u32::try_from(
                plan.changes.added.len()
                    + plan.changes.unchanged.len()
                    + plan.changes.restarted.len(),
            )
            .unwrap_or(u32::MAX),
        })
    }
}

fn applied_revision(
    plan_id: &str,
    plan_digest: String,
    revision: PublishedRevision,
    status: LabInstanceStatus,
) -> AppliedLab {
    AppliedLab {
        generation: status.instance.generation,
        instance: status.instance,
        phase: status.phase,
        plan_id: plan_id.into(),
        plan_digest,
        revision_digest: revision.digest,
        lock_digest: revision.lock.digest,
        component_count: u32::try_from(revision.lab.components.len()).unwrap_or(u32::MAX),
        reconciliation_error: None,
    }
}

/// A checked request carries no runtime dependency and performs no writes.
pub struct ReviewedApply {
    reference: String,
    plan_id: String,
    plan_digest: String,
    update: Option<LabUpdatePlan>,
}

pub fn review_apply(
    store: &Store,
    workspace: &str,
    principal: &str,
    reference: &str,
    plan_id: &str,
    expected_digest: &str,
) -> Result<ReviewedApply, Error> {
    let update = store.update_plan(workspace, principal, plan_id)?;
    let actual = if let Some(plan) = &update {
        if plan.digest != expected_digest {
            return Err(Error::problem(
                "lab_plan_digest_mismatch",
                "Update digest differs from the reviewed plan; nothing applied",
            ));
        }
        let target = store.resolve_lab(workspace, principal, reference)?;
        if target.instance_id != plan.target.instance_id {
            return Err(Error::problem(
                "lab_plan_digest_mismatch",
                "Update target differs from the reviewed plan; nothing applied",
            ));
        }
        plan.digest.clone()
    } else {
        digest_json(&store.read_draft(workspace, principal, plan_id)?.lab)
    };
    if actual != expected_digest {
        return Err(Error {
            kind: ErrorKind::Invalid,
            message: "stored lab plan does not match expected_plan_digest; nothing was applied"
                .into(),
            details: Some(
                serde_json::json!({"code":"lab_plan_digest_mismatch", "plan_id":plan_id,
                "expected_plan_digest":expected_digest, "actual_plan_digest":actual,
                "recovery":"read or recreate the plan and apply the returned digest"}),
            ),
        });
    }
    Ok(ReviewedApply {
        reference: reference.into(),
        plan_id: plan_id.into(),
        plan_digest: actual,
        update,
    })
}
