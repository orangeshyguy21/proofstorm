//! Transport-independent component lifecycle admission and idempotent submission.
use super::Cells;
use crate::{Error, runtime::runtime_action_resource};
use proofstorm_core::{Capability, CellOperation, ComponentKind, OperationKind, OperationPhase};
use proofstorm_kube::{CellAction, ComponentControlAction};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentControlRequest {
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    pub experiment_id: String,
    #[serde(default)]
    pub session_id: String,
    pub operation_id: String,
    pub component: String,
    pub idempotency_key: String,
}

impl Cells {
    pub async fn control_component(
        &self,
        request: ComponentControlRequest,
        kind: OperationKind,
    ) -> Result<CellOperation, Error> {
        let (capability, action) = control_action(&request.component, kind)?;
        self.authorize(&[capability])?;
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            capability,
        )?;
        let component = revision
            .cell
            .components
            .iter()
            .find(|c| c.id == request.component)
            .ok_or_else(|| {
                Error::problem(
                    "component_not_found",
                    "component is not part of this cell revision",
                )
            })?;
        if capability == Capability::NodeControl
            && !matches!(
                component.kind,
                ComponentKind::Bitcoin | ComponentKind::Lightning
            )
        {
            return Err(Error::problem(
                "invalid_operation",
                "node controls support only Bitcoin and Lightning; use component controls for other components",
            ));
        }
        // Validate the same executable backend contract used by the controller.
        proofstorm_kube::compile_component_plans(
            &instance.instance_key,
            &revision.digest,
            &revision.cell,
            &revision.lock,
        )
        .map_err(|e| Error::problem("invalid_operation", e.to_string()))?;
        let mut payload = serde_json::to_value(&request)
            .map_err(|e| Error::problem("invalid_operation", e.to_string()))?;
        if let Some(fields) = payload.as_object_mut() {
            fields.remove("idempotency_key");
        }
        let operation = self.store.create_operation_at_revision(
            &revision.digest,
            &self.workspace,
            &self.principal,
            &instance.id,
            &request.experiment_id,
            &request.session_id,
            &request.operation_id,
            kind,
            &payload,
            &request.idempotency_key,
            capability,
        )?;
        // The store returns the current durable result on replay.
        if operation.phase != OperationPhase::Pending {
            return Ok(operation);
        }
        let resource = runtime_action_resource(
            &self.runtime.control_namespace,
            &instance,
            &operation,
            action,
        );
        if let Some(grant) = &resource.spec.access_scope {
            self.runtime.private_access(grant).await?;
        }
        self.runtime.apply_action(&instance, &resource).await?;
        Ok(self.store.update_operation_phase(
            &self.workspace,
            &operation.id,
            OperationPhase::Running,
        )?)
    }
}

fn control_action(component: &str, kind: OperationKind) -> Result<(Capability, CellAction), Error> {
    let parameters = ComponentControlAction {
        component: component.to_owned(),
    };
    let result = match kind {
        OperationKind::NodeStart => (Capability::NodeControl, CellAction::NodeStart(parameters)),
        OperationKind::NodeStop => (Capability::NodeControl, CellAction::NodeStop(parameters)),
        OperationKind::NodeRestart => {
            (Capability::NodeControl, CellAction::NodeRestart(parameters))
        }
        OperationKind::ComponentStart => (
            Capability::ComponentControl,
            CellAction::ComponentStart(parameters),
        ),
        OperationKind::ComponentStop => (
            Capability::ComponentControl,
            CellAction::ComponentStop(parameters),
        ),
        OperationKind::ComponentRestart => (
            Capability::ComponentControl,
            CellAction::ComponentRestart(parameters),
        ),
        _ => {
            return Err(Error::problem(
                "invalid_operation",
                "expected a component lifecycle operation",
            ));
        }
    };
    Ok(result)
}
