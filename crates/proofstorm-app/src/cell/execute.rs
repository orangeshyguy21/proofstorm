//! Native commands with automatic activity attribution.
use super::Cells;
use crate::{Error, runtime::runtime_action_resource};
use proofstorm_core::{
    Capability, CellOperation, OperationKind, OperationPhase, native::NativeCommand,
};
use proofstorm_kube::{CellAction, ComponentExecLiveAction};
use proofstorm_store::CellHandlePhase;

impl Cells {
    /// Workspace requests are bounded control calls; the workspace owns the resulting task.
    pub async fn workspace_request(
        &self,
        name: &str,
        component: &str,
        request: &proofstorm_core::workspace::WorkspaceRequest,
        request_id: &str,
    ) -> Result<CellOperation, Error> {
        self.workspace_request_prepared(name, component, request, request_id, |_| async { Ok(()) })
            .await
    }

    pub(super) async fn workspace_request_prepared<F, Fut>(
        &self,
        name: &str,
        component: &str,
        request: &proofstorm_core::workspace::WorkspaceRequest,
        request_id: &str,
        prepare: F,
    ) -> Result<CellOperation, Error>
    where
        F: FnOnce(proofstorm_core::CellInstance) -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
        self.authorize(&[Capability::ComponentExecLive, Capability::ArtifactRead])?;
        request
            .validate()
            .map_err(|error| Error::problem("invalid_workspace_request", error))?;
        let cell = self.resolve(name)?;
        let (_, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &cell.instance_id,
            request_id,
            Capability::ComponentExecLive,
        )?;
        if !revision
            .cell
            .components
            .iter()
            .any(|item| item.id == component && item.implementation == "workspace")
        {
            return Err(Error::problem(
                "workspace_component_required",
                "select a component with implementation workspace",
            ));
        }
        if let proofstorm_core::workspace::WorkspaceRequest::Task(
            proofstorm_core::workspace::TaskRequest::Start(start),
        ) = request
            && let Some(scope) = &start.control
            && scope.targets().any(|id| {
                !revision
                    .cell
                    .components
                    .iter()
                    .any(|component| component.id == id)
            })
        {
            return Err(Error::problem(
                "invalid_workspace_scope",
                "control components must belong to this cell",
            ));
        }
        let encoded = proofstorm_core::workspace::wire::encode(request)
            .map_err(|error| Error::problem("invalid_workspace_request", error))?;
        self.exec_prepared(
            name,
            component,
            NativeCommand {
                private_io: None,
                script: String::new(),
                argv: vec![
                    proofstorm_core::workspace::WORKSPACE_RUNNER.into(),
                    "workspace".into(),
                    "request".into(),
                    encoded,
                ],
                timeout_seconds: 25,
                output: proofstorm_core::native::NativeOutput {
                    mode: proofstorm_core::native::OutputMode::Public,
                    fields: vec![],
                },
            },
            request_id,
            prepare,
        )
        .await
    }

    pub async fn exec(
        &self,
        name: &str,
        component: &str,
        command: NativeCommand,
        request_id: &str,
    ) -> Result<CellOperation, Error> {
        self.exec_prepared(name, component, command, request_id, |_| async { Ok(()) })
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep authorization, admission, preparation and controller submission in order"
    )]
    async fn exec_prepared<F, Fut>(
        &self,
        name: &str,
        component: &str,
        command: NativeCommand,
        request_id: &str,
        prepare: F,
    ) -> Result<CellOperation, Error>
    where
        F: FnOnce(proofstorm_core::CellInstance) -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
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
        let cell = self.resolve(name)?;
        if cell.phase != CellHandlePhase::Open {
            return Err(Error::problem(
                "cell_closing",
                "new actions are not admitted while closing",
            ));
        }
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &cell.instance_id,
            request_id,
            Capability::ComponentExecLive,
        )?;
        if !revision.cell.components.iter().any(|c| c.id == component) {
            return Err(Error::problem(
                "component_not_found",
                "component is not part of this cell",
            ));
        }
        let control =
            proofstorm_core::workspace::control::start_request(&command.script, &command.argv)
                .and_then(|start| start.control);
        if let Some(scope) = &control {
            self.authorize(&scope.capabilities())?;
            if scope.targets().any(|id| {
                !revision
                    .cell
                    .components
                    .iter()
                    .any(|target| target.id == id)
            }) || scope.lifecycle.iter().any(|id| id == component)
            {
                return Err(Error::problem(
                    "invalid_workspace_scope",
                    "scope targets must belong to this cell; a task cannot control its own workspace lifecycle",
                ));
            }
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
        prepare(instance.clone()).await?;
        // Preparation can stream a file. Honor cancellation or permission changes
        // that arrived while it was in flight before submitting its commit.
        self.authorize(&[Capability::ComponentExecLive, Capability::ArtifactRead])?;
        let op = self
            .store
            .operation(&self.workspace, &self.principal, &op.id)?;
        if op.phase != OperationPhase::Pending {
            return Ok(op);
        }
        let mut action = runtime_action_resource(
            &self.runtime.control_namespace,
            &instance,
            &op,
            CellAction::ComponentExecLive(ComponentExecLiveAction {
                private_payload: None,
                component: component.into(),
                script: command.script,
                argv: command.argv,
                timeout_seconds: command.timeout_seconds,
                output: command.output,
            }),
        );
        if let Some(scope) = &control {
            action.metadata.annotations.get_or_insert_default().insert(
                proofstorm_core::workspace::control::GRANT_ANNOTATION.into(),
                proofstorm_core::digest_json(scope),
            );
        }
        self.runtime.apply_action(&instance, &action).await?;
        Ok(self
            .store
            .update_operation_phase(&self.workspace, &op.id, OperationPhase::Running)?)
    }
}
