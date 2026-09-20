//! Transport-independent component lifecycle admission and idempotent submission.
use super::{Cells, submission::OperationAdmission};
use crate::Error;
use proofstorm_core::{Capability, CellOperation, ComponentKind, OperationKind};
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
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind,
                capability,
            },
            &request,
        )?;
        self.submit_action(&instance, operation, action).await
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
