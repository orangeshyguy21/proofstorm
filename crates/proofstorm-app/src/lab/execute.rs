//! Native commands with automatic activity attribution.
use super::Labs;
use crate::{Error, runtime::runtime_action_resource};
use proofstorm_core::{
    Capability, LabOperation, OperationKind, OperationPhase, native::NativeCommand,
};
use proofstorm_kube::{ComponentExecLiveAction, LabAction};
use proofstorm_store::LabHandlePhase;

impl Labs {
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
        let lab = self.resolve(name)?;
        if lab.phase != LabHandlePhase::Open {
            return Err(Error::problem(
                "lab_closing",
                "new actions are not admitted while closing",
            ));
        }
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
            "",
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
}
