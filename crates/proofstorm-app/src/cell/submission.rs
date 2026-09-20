//! One durable admission and runtime submission path for cell operations.
use super::Cells;
use crate::{Error, runtime::runtime_action_resource};
use proofstorm_core::{Capability, CellInstance, CellOperation, OperationKind, OperationPhase};
use proofstorm_kube::{CellAction, ProofstormCellAction};
use serde::Serialize;

#[derive(Clone, Copy)]
pub(super) struct OperationAdmission<'a> {
    pub experiment_id: &'a str,
    pub session_id: &'a str,
    pub operation_id: &'a str,
    pub idempotency_key: &'a str,
    pub kind: OperationKind,
    pub capability: Capability,
}

impl Cells {
    pub(super) fn admit_action(
        &self,
        instance: &CellInstance,
        admission: OperationAdmission<'_>,
        request: &impl Serialize,
    ) -> Result<CellOperation, Error> {
        let mut payload = serde_json::to_value(request).map_err(|error| {
            Error::failure(
                format!("operation request serialization failed: {error}"),
                Some(serde_json::json!({"code": "serialization_failed"})),
            )
        })?;
        if let Some(fields) = payload.as_object_mut() {
            fields.remove("idempotency_key");
        }
        Ok(self.store.create_operation_at_revision(
            &instance.revision_digest,
            &self.workspace,
            &self.principal,
            &instance.id,
            admission.experiment_id,
            admission.session_id,
            admission.operation_id,
            admission.kind,
            &payload,
            admission.idempotency_key,
            admission.capability,
        )?)
    }

    pub(super) async fn submit_action(
        &self,
        instance: &CellInstance,
        operation: CellOperation,
        action: CellAction,
    ) -> Result<CellOperation, Error> {
        let resource = runtime_action_resource(
            &self.runtime.control_namespace,
            instance,
            &operation,
            action,
        );
        self.submit_action_resource(instance, operation, resource)
            .await
    }

    pub(super) async fn submit_action_resource(
        &self,
        instance: &CellInstance,
        operation: CellOperation,
        mut resource: ProofstormCellAction,
    ) -> Result<CellOperation, Error> {
        if operation.phase != OperationPhase::Pending {
            return Ok(operation);
        }
        let current =
            self.store
                .operation_for_submission(&self.workspace, &self.principal, &operation.id)?;
        if current.phase != OperationPhase::Pending {
            return Ok(current);
        }
        resource.spec.access_scope = self.store.operation_access_scope(&current)?;
        if let Some(grant) = &resource.spec.access_scope {
            self.runtime.private_access(grant).await?;
            // Publishing permission is asynchronous. Cancellation or revocation
            // during that call must prevent the pending action from being sent.
            let current = self.store.operation_for_submission(
                &self.workspace,
                &self.principal,
                &operation.id,
            )?;
            if current.phase != OperationPhase::Pending {
                return Ok(current);
            }
        }
        self.runtime.apply_action(instance, &resource).await?;
        Ok(self.store.update_operation_phase(
            &self.workspace,
            &operation.id,
            OperationPhase::Running,
        )?)
    }
}

pub(super) fn invalid_operation(message: &str) -> Error {
    Error::problem("invalid_operation", message)
}
