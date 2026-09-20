//! Native commands with automatic activity attribution.
use super::{Cells, component_reference::component_image_any, submission::OperationAdmission};
use crate::{Error, runtime::runtime_action_resource};
use proofstorm_core::{
    Capability, CellOperation, OperationKind, OperationPhase, native::NativeCommand,
};
use proofstorm_kube::{CellAction, ComponentExecLiveAction};
use proofstorm_store::CellHandlePhase;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeExecutionRequest {
    /// Opaque custody reference; token bytes never belong in this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_payload: Option<proofstorm_core::private_io::PayloadBinding>,
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
    /// POSIX shell program. Its exit code describes the shell, including any pipelines.
    #[serde(default)]
    pub script: String,
    /// Direct command and arguments. Prefer this to preserve the native process exit status.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Private default; public raw. `json_fields`: status,state,`failure_reason`,settled,
    /// `synced_to_chain`,amount,`amount_sat`,
    /// `fee_paid`,`fee_paid_sat`,`value_sat`,`total_fees`,`total_fees_msat`,`num_active_channels`,
    /// balance,`confirmed_balance`,`unconfirmed_balance`,`seedAccess.state`,
    /// `seedAccess.requiresPassphrase`,`cocoSession.state`.
    /// bolt11: invoice text. `lnd_invoice`: LND JSON with matching hash.
    /// Both return validated invoice/hash/amount/currency/expiry; raw streams stay private.
    #[serde(default)]
    pub output: proofstorm_core::native::NativeOutput,
    pub timeout_seconds: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

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

    /// Execute an explicitly attributed native request. Serialization retains the
    /// established MCP journal shape so retries survive transport consolidation.
    pub async fn execute_native(
        &self,
        request: NativeExecutionRequest,
    ) -> Result<CellOperation, Error> {
        let payload = serde_json::to_value(&request).map_err(|error| {
            Error::failure(
                format!("operation request serialization failed: {error}"),
                Some(serde_json::json!({"code": "serialization_failed"})),
            )
        })?;
        self.submit_native(request, payload, |_| async { Ok(()) })
            .await
    }

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
        // Preserve the application's existing request digest independently of the
        // explicit run/name/request_id envelope used by MCP.
        let payload = serde_json::json!({"component":component,"script":command.script,"argv":command.argv,"timeout_seconds":command.timeout_seconds,"output":command.output});
        self.submit_native(
            NativeExecutionRequest {
                instance_id: cell.instance_id,
                experiment_id: String::new(),
                session_id: String::new(),
                operation_id: request_id.into(),
                idempotency_key: request_id.into(),
                component: component.into(),
                private_payload: None,
                script: command.script,
                argv: command.argv,
                timeout_seconds: command.timeout_seconds,
                output: command.output,
            },
            payload,
            |instance| async move {
                prepare(instance).await?;
                self.authorize(&[Capability::ArtifactRead])
            },
        )
        .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep authorization, admission, preparation and controller submission in order"
    )]
    async fn submit_native<F, Fut>(
        &self,
        request: NativeExecutionRequest,
        payload: Value,
        prepare: F,
    ) -> Result<CellOperation, Error>
    where
        F: FnOnce(proofstorm_core::CellInstance) -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
        self.authorize(&[Capability::ComponentExecLive])?;
        NativeCommand {
            private_io: None,
            script: request.script.clone(),
            argv: request.argv.clone(),
            timeout_seconds: request.timeout_seconds,
            output: request.output.clone(),
        }
        .validate()
        .map_err(|e| Error::problem("invalid_operation", e))?;
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::ComponentExecLive,
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
        component_image_any(&revision, &request.component, component.kind)?;
        let control =
            proofstorm_core::workspace::control::start_request(&request.script, &request.argv)
                .and_then(|start| start.control);
        if let Some(scope) = &control {
            self.authorize(&scope.capabilities())?;
            if scope.targets().any(|id| {
                !revision
                    .cell
                    .components
                    .iter()
                    .any(|target| target.id == id)
            }) || scope.lifecycle.iter().any(|id| id == &request.component)
            {
                return Err(Error::problem(
                    "invalid_workspace_scope",
                    "scope targets must belong to this cell; a task cannot control its own workspace lifecycle",
                ));
            }
        }
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::ComponentExecLive,
                capability: Capability::ComponentExecLive,
            },
            &payload,
        )?;
        if operation.phase != OperationPhase::Pending {
            return Ok(operation);
        }
        prepare(instance.clone()).await?;
        // Preparation can stream a file. Recheck ownership, authority and durable
        // phase without requiring permission to read unrelated action artifacts.
        let operation =
            self.store
                .operation_for_submission(&self.workspace, &self.principal, &operation.id)?;
        if operation.phase != OperationPhase::Pending {
            return Ok(operation);
        }
        if let Some(scope) = &control {
            self.authorize(&scope.capabilities())?;
        }
        let mut action = runtime_action_resource(
            &self.runtime.control_namespace,
            &instance,
            &operation,
            CellAction::ComponentExecLive(ComponentExecLiveAction {
                private_payload: request.private_payload,
                component: request.component,
                script: request.script,
                argv: request.argv,
                timeout_seconds: request.timeout_seconds,
                output: request.output,
            }),
        );
        if let Some(scope) = &control {
            action.metadata.annotations.get_or_insert_default().insert(
                proofstorm_core::workspace::control::GRANT_ANNOTATION.into(),
                proofstorm_core::digest_json(scope),
            );
        }
        self.submit_action_resource(&instance, operation, action)
            .await
    }
}
