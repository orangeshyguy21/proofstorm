//! Bounded component logs and offline forensics.
use super::{
    Cells,
    component_reference::component_image_any,
    submission::{OperationAdmission, invalid_operation},
};
use crate::Error;
use proofstorm_core::{Capability, CellOperation, OperationKind};
use proofstorm_kube::{CellAction, ComponentForensicsAction, ComponentLogsAction};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentLogsRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub component: String,
    /// Lines to read from the end of the component's current container log,
    /// between 1 and 2000. The artifact is additionally byte-bounded.
    pub tail_lines: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentExecRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub component: String,
    /// Cell component whose native service endpoint should be exposed to the
    /// command. When omitted, the execution component is also the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_component: Option<String>,
    /// An unrestricted non-interactive shell program run by `/bin/sh` inside
    /// the component's pinned image. Native command failures are returned as
    /// an exit code in the terminal artifact.
    pub script: String,
    pub timeout_seconds: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

impl ComponentLogsRequest {
    pub fn validate(&self) -> Result<(), Error> {
        if !(1..=2_000).contains(&self.tail_lines) {
            return Err(invalid_operation("tail_lines must be in 1..=2000"));
        }
        Ok(())
    }
}

impl Cells {
    pub async fn component_logs(
        &self,
        request: ComponentLogsRequest,
    ) -> Result<CellOperation, Error> {
        self.authorize(&[Capability::ComponentLogs])?;
        request.validate()?;

        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::ComponentLogs,
        )?;
        let component = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this cell revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::ComponentLogs,
                capability: Capability::ComponentLogs,
            },
            &request,
        )?;
        self.submit_action(
            &instance,
            operation,
            CellAction::ComponentLogs(ComponentLogsAction {
                component: request.component,
                tail_lines: request.tail_lines,
            }),
        )
        .await
    }
    pub async fn component_forensics(
        &self,
        request: ComponentExecRequest,
    ) -> Result<CellOperation, Error> {
        self.authorize(&[Capability::ComponentForensics])?;
        if request.script.is_empty() || request.script.len() > 16 * 1024 {
            return Err(invalid_operation(
                "script must contain 1..=16384 UTF-8 bytes",
            ));
        }
        if !(1..=300).contains(&request.timeout_seconds) {
            return Err(invalid_operation("timeout_seconds must be in 1..=300"));
        }
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::ComponentForensics,
        )?;
        let component = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.component)
            .ok_or_else(|| invalid_operation("component is not part of this cell revision"))?;
        component_image_any(&revision, &request.component, component.kind)?;
        let target_component = request
            .target_component
            .as_deref()
            .unwrap_or(&request.component)
            .to_owned();
        let target = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == target_component)
            .ok_or_else(|| {
                invalid_operation("target_component is not part of this cell revision")
            })?;
        component_image_any(&revision, &target_component, target.kind)?;
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::ComponentForensics,
                capability: Capability::ComponentForensics,
            },
            &request,
        )?;
        self.submit_action(
            &instance,
            operation,
            CellAction::ComponentForensics(ComponentForensicsAction {
                component: request.component,
                target_component,
                script: request.script,
                timeout_seconds: request.timeout_seconds,
            }),
        )
        .await
    }
}
