use k8s_openapi::api::batch::v1::Job;
use kube::ResourceExt;
use proofstorm_core::{
    Capability, ComponentConditionReason, ComponentConditionState, ComponentConditionType,
    ComponentKind, ComponentPlanContract, ComponentStatus, EffectiveComponentConfig,
    ExecutionStorageSource, OperationClass, ReadinessPrerequisite,
};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    AuthenticationConformanceAction, AuthenticationProtectedSpendAction,
    AuthenticationReplayAction, CellAction, ComponentForensicsAction, ProofstormCell,
    ProofstormCellAction, ReachabilityOracleAction, component_ports, instance_namespace,
    pod::{container_security, instance_affinity, pod_security},
};

use crate::images::PROBE_IMAGE as REACHABILITY_PROBE_IMAGE;

pub struct AuthenticationConformanceJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub mint: &'a str,
    pub identity_provider: &'a str,
    pub mint_image: &'a str,
    /// Catalog implementation of the mint; selects its protocol error codes.
    pub mint_implementation: &'a str,
}

pub struct AuthenticationProtectedSpendJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub mint: &'a str,
    pub identity_provider: &'a str,
    pub mint_image: &'a str,
    /// Catalog implementation of the mint; selects its protocol error codes.
    pub mint_implementation: &'a str,
}

pub struct AuthenticationReplayJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub mint: &'a str,
    pub identity_provider: &'a str,
    pub mint_image: &'a str,
    /// Catalog implementation of the mint; selects its protocol error codes.
    pub mint_implementation: &'a str,
    pub session_secret: &'a str,
    pub source_operation_id: &'a str,
}

#[derive(Debug, Error)]
pub enum ActionRenderError {
    #[error("action identity does not match referenced cell: {0}")]
    Identity(&'static str),
    #[error("action capability is invalid for its typed request")]
    Capability,
    #[error("typed action request is outside bounded policy: {0}")]
    Bounds(&'static str),
    #[error("component {component:?} must use installed {implementation:?} {kind:?} adapter")]
    Component {
        component: String,
        implementation: &'static str,
        kind: ComponentKind,
    },
    #[error("component {0:?} has no immutable lock entry")]
    MissingLock(String),
    #[error("component {0:?} is not present in the immutable cell revision")]
    UnknownComponent(String),
    #[error("component {component:?} does not advertise logical service {service:?}")]
    UnknownService { component: String, service: String },
    #[error("component {component:?} uses unsupported action adapter {adapter:?}")]
    UnsupportedAdapter { component: String, adapter: String },
    #[error("component plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("typed action rendered an invalid Kubernetes Job: {0}")]
    InvalidResource(#[from] serde_json::Error),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ActionAdmissionError {
    #[error("cell is closing; new actions are not admitted")]
    CellClosing,
    #[error("action identity is invalid: {0}")]
    Identity(&'static str),
    #[error("component plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("component {component:?} has no {operation:?} admission contract")]
    MissingContract {
        component: String,
        operation: OperationClass,
    },
    #[error("component {component:?} does not satisfy {prerequisite:?} for {operation:?}")]
    PrerequisiteUnsatisfied {
        component: String,
        operation: OperationClass,
        prerequisite: ReadinessPrerequisite,
        condition: Option<ComponentConditionType>,
        state: Option<ComponentConditionState>,
        reason: Option<ComponentConditionReason>,
    },
}

impl ActionAdmissionError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CellClosing => "cell_closing",
            Self::Identity(_) => "action_identity_invalid",
            Self::InvalidPlan(_) => "action_plan_invalid",
            Self::MissingContract { .. } => "action_admission_contract_missing",
            Self::PrerequisiteUnsatisfied { .. } => "action_prerequisite_unsatisfied",
        }
    }
}

/// Evaluate a typed action against backend-declared readiness prerequisites.
///
/// Immutable execution contexts and target descriptors remain usable when a
/// component protocol is unhealthy. Runtime conditions are required only when
/// the selected operation contract names their prerequisite.
///
/// # Errors
///
/// Returns a stable failure when identity, compiled admission contracts, or an
/// applicable runtime readiness prerequisite is unavailable.
pub fn evaluate_action_admission(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
) -> Result<(), ActionAdmissionError> {
    evaluate_action_admission_at(
        action,
        cell,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
            }),
    )
}

/// Evaluate readiness at an explicit observation time.
/// # Errors
/// Returns an error when a required identity or fresh readiness prerequisite is unavailable.
pub fn evaluate_action_admission_at(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    now_unix: i64,
) -> Result<(), ActionAdmissionError> {
    require_open_cell(cell)?;
    validate_action_identity(action, cell).map_err(|error| match error {
        ActionRenderError::Identity(field) => ActionAdmissionError::Identity(field),
        _ => ActionAdmissionError::InvalidPlan(error.to_string()),
    })?;
    let plans = crate::compile_component_plans(
        &cell.spec.instance_key,
        &cell.spec.revision_digest,
        &cell.spec.cell,
        &cell.spec.lock,
    )
    .map_err(|error| ActionAdmissionError::InvalidPlan(error.to_string()))?;
    let mut statuses = cell
        .status
        .as_ref()
        .filter(|status| status.observed_revision_digest == cell.spec.revision_digest)
        .map_or_else(Vec::new, |status| status.components.clone());
    crate::expire_protocol_status(&plans, &mut statuses, now_unix);

    for (component, operation) in action_participants(&action.spec.action) {
        let plan = require_admission_plan(&plans, component, operation)?;
        let contract = plan
            .operation_admission
            .iter()
            .find(|contract| contract.operation == operation)
            .ok_or_else(|| ActionAdmissionError::MissingContract {
                component: component.to_owned(),
                operation,
            })?;
        for prerequisite in &contract.prerequisites {
            evaluate_prerequisite(plan, &statuses, operation, *prerequisite)?;
        }
    }

    if let Some((executor, target)) = action_execution_target(&action.spec.action)
        && executor != target
    {
        let target = require_admission_plan(&plans, target, OperationClass::NativeExec)?;
        evaluate_prerequisite(
            target,
            &statuses,
            OperationClass::NativeExec,
            ReadinessPrerequisite::TargetDescriptor,
        )?;
    }
    Ok(())
}

/// Admit work on a live cell independently of aggregate component health.
/// Operation-specific readiness is evaluated separately. Existing execution
/// receipts may still be collected during teardown.
///
/// # Errors
/// Returns `CellClosing` when deletion or cleanup has begun.
pub fn require_open_cell(cell: &ProofstormCell) -> Result<(), ActionAdmissionError> {
    if cell.metadata.deletion_timestamp.is_some()
        || cell.status.as_ref().is_some_and(|status| {
            matches!(
                status.phase,
                crate::CellPhase::Closing | crate::CellPhase::CleanupBlocked
            )
        })
    {
        return Err(ActionAdmissionError::CellClosing);
    }
    Ok(())
}

fn require_admission_plan<'a>(
    plans: &'a [ComponentPlanContract],
    component: &str,
    operation: OperationClass,
) -> Result<&'a ComponentPlanContract, ActionAdmissionError> {
    plans
        .iter()
        .find(|plan| plan.component_id == component)
        .ok_or_else(|| ActionAdmissionError::PrerequisiteUnsatisfied {
            component: component.to_owned(),
            operation,
            prerequisite: ReadinessPrerequisite::AcceptedIdentity,
            condition: None,
            state: None,
            reason: None,
        })
}

fn evaluate_prerequisite(
    plan: &ComponentPlanContract,
    statuses: &[ComponentStatus],
    operation: OperationClass,
    prerequisite: ReadinessPrerequisite,
) -> Result<(), ActionAdmissionError> {
    let condition_type = match prerequisite {
        ReadinessPrerequisite::Storage => Some(ComponentConditionType::StorageReady),
        ReadinessPrerequisite::Dependencies => Some(ComponentConditionType::DependenciesReady),
        ReadinessPrerequisite::Protocol => Some(ComponentConditionType::ProtocolReady),
        ReadinessPrerequisite::AcceptedIdentity
        | ReadinessPrerequisite::ExecutionContext
        | ReadinessPrerequisite::TargetDescriptor
        | ReadinessPrerequisite::FaultIdentity => return Ok(()),
        ReadinessPrerequisite::WorkloadIdentity => {
            return evaluate_workload_identity(plan, statuses, operation);
        }
    };
    let condition_type = condition_type.expect("runtime prerequisite has a condition");
    if !plan.applicable_conditions.contains(&condition_type) {
        return Ok(());
    }
    let status = current_component_status(plan, statuses);
    let condition = status.and_then(|status| {
        status
            .conditions
            .iter()
            .find(|condition| condition.condition_type == condition_type)
    });
    if condition.is_some_and(|condition| condition.state == ComponentConditionState::True) {
        return Ok(());
    }
    Err(unsatisfied(
        plan,
        operation,
        prerequisite,
        Some(condition_type),
        condition,
    ))
}

fn evaluate_workload_identity(
    plan: &ComponentPlanContract,
    statuses: &[ComponentStatus],
    operation: OperationClass,
) -> Result<(), ActionAdmissionError> {
    let condition = current_component_status(plan, statuses).and_then(|status| {
        status
            .conditions
            .iter()
            .find(|condition| condition.condition_type == ComponentConditionType::WorkloadReady)
    });
    if condition.is_some_and(|condition| {
        !matches!(
            condition.reason,
            ComponentConditionReason::NotObserved | ComponentConditionReason::StaleRevision
        )
    }) {
        return Ok(());
    }
    Err(unsatisfied(
        plan,
        operation,
        ReadinessPrerequisite::WorkloadIdentity,
        Some(ComponentConditionType::WorkloadReady),
        condition,
    ))
}

fn current_component_status<'a>(
    plan: &ComponentPlanContract,
    statuses: &'a [ComponentStatus],
) -> Option<&'a ComponentStatus> {
    statuses.iter().find(|status| {
        status.id == plan.component_id
            && status.observed_revision_digest == plan.revision_digest
            && status.observed_rollout_digest == plan.rollout_digest
    })
}

fn unsatisfied(
    plan: &ComponentPlanContract,
    operation: OperationClass,
    prerequisite: ReadinessPrerequisite,
    condition_type: Option<ComponentConditionType>,
    condition: Option<&proofstorm_core::ComponentCondition>,
) -> ActionAdmissionError {
    ActionAdmissionError::PrerequisiteUnsatisfied {
        component: plan.component_id.clone(),
        operation,
        prerequisite,
        condition: condition_type,
        state: condition.map(|condition| condition.state),
        reason: condition.map(|condition| condition.reason),
    }
}

fn action_execution_target(action: &CellAction) -> Option<(&str, &str)> {
    match action {
        CellAction::ComponentForensics(request) => {
            Some((&request.component, &request.target_component))
        }
        CellAction::ReachabilityOracle(request) => {
            Some((&request.from_component, &request.to_component))
        }
        _ => None,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive action-to-participant contract is clearest in one match"
)]
fn action_participants(action: &CellAction) -> Vec<(&str, OperationClass)> {
    use OperationClass as Operation;
    match action {
        CellAction::NodeStart(request) | CellAction::ComponentStart(request) => {
            vec![(&request.component, Operation::Start)]
        }
        CellAction::NodeStop(request) | CellAction::ComponentStop(request) => {
            vec![(&request.component, Operation::Stop)]
        }
        CellAction::NodeRestart(request) | CellAction::ComponentRestart(request) => {
            vec![(&request.component, Operation::Restart)]
        }
        CellAction::NetworkPartition(request) => vec![
            (&request.from_component, Operation::Inspect),
            (&request.to_component, Operation::Inspect),
        ],
        // Neither healing a fault nor reading a log has a component readiness
        // prerequisite. For a log that is deliberate: an unready,
        // crash-looping, or stopped component is when its log matters most.
        CellAction::NetworkHeal(_) | CellAction::ComponentLogs(_) => Vec::new(),
        CellAction::ReachabilityOracle(request) => {
            vec![(&request.from_component, Operation::NativeExec)]
        }
        CellAction::ComponentForensics(request) => {
            vec![(&request.component, Operation::NativeExec)]
        }
        CellAction::PrivateTransfer(_) => vec![],
        CellAction::ComponentExecLive(request) => {
            vec![(&request.component, Operation::NativeExec)]
        }
        CellAction::AuthenticationConformance(request) => vec![
            (&request.mint, Operation::Authentication),
            (&request.identity_provider, Operation::Authentication),
        ],
        CellAction::AuthenticationProtectedSpend(request) => vec![
            (&request.mint, Operation::Authentication),
            (&request.identity_provider, Operation::Authentication),
        ],
        CellAction::AuthenticationReplay(request) => vec![
            (&request.mint, Operation::Authentication),
            (&request.identity_provider, Operation::Authentication),
        ],
    }
}

#[must_use]
pub const fn action_result_container(action: &CellAction) -> &'static str {
    match action {
        CellAction::NodeStart(_)
        | CellAction::NodeStop(_)
        | CellAction::NodeRestart(_)
        | CellAction::ComponentStart(_)
        | CellAction::ComponentStop(_)
        | CellAction::ComponentRestart(_)
        | CellAction::NetworkPartition(_)
        | CellAction::NetworkHeal(_) => "result",
        CellAction::ReachabilityOracle(_) => "oracle",
        CellAction::ComponentForensics(_) => "forensics",
        CellAction::AuthenticationConformance(_)
        | CellAction::AuthenticationProtectedSpend(_)
        | CellAction::AuthenticationReplay(_) => "authentication",
        // Never rendered as a Job; the controller reads the log itself.
        CellAction::ComponentLogs(_)
        | CellAction::ComponentExecLive(_)
        | CellAction::PrivateTransfer(_) => "",
    }
}

/// Validate a typed action against its immutable cell and render its deterministic Job.
///
/// # Errors
///
/// Returns an error for identity drift, unsupported components, values outside
/// policy bounds, or an invalid internal Kubernetes resource.
pub fn render_cell_action_job(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
) -> Result<Job, ActionRenderError> {
    validate_action_identity(action, cell)?;
    let mut job = match &action.spec.action {
        CellAction::NodeStart(_)
        | CellAction::NodeStop(_)
        | CellAction::NodeRestart(_)
        | CellAction::ComponentStart(_)
        | CellAction::ComponentStop(_)
        | CellAction::ComponentRestart(_)
        | CellAction::ComponentExecLive(_)
        | CellAction::PrivateTransfer(_)
        | CellAction::NetworkPartition(_)
        | CellAction::NetworkHeal(_) => {
            return Err(ActionRenderError::Bounds(
                "direct controller actions do not render Jobs",
            ));
        }
        CellAction::ReachabilityOracle(request) => {
            render_reachability_oracle_action(action, cell, request)?
        }
        CellAction::ComponentForensics(request) => {
            render_native_exec_action(action, cell, request)?
        }
        CellAction::AuthenticationConformance(request) => {
            render_authentication_conformance_action(action, cell, request)?
        }
        CellAction::AuthenticationProtectedSpend(request) => {
            render_authentication_protected_spend_action(action, cell, request)?
        }
        CellAction::AuthenticationReplay(request) => {
            render_authentication_replay_action(action, cell, request)?
        }
        CellAction::ComponentLogs(_) => {
            return Err(ActionRenderError::InvalidPlan(
                "component logs are fulfilled by the controller, not by a Job".to_owned(),
            ));
        }
    };
    mark_controller_owned(&mut job, &action.name_any());
    Ok(job)
}

fn render_authentication_conformance_action(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    request: &AuthenticationConformanceAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::AuthenticationTest {
        return Err(ActionRenderError::Capability);
    }
    let (mint_image, mint_implementation) =
        authentication_components(cell, &request.mint, &request.identity_provider)?;
    render_authentication_conformance_job(&AuthenticationConformanceJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        mint: &request.mint,
        identity_provider: &request.identity_provider,
        mint_image,
        mint_implementation,
    })
    .map_err(ActionRenderError::from)
}

fn render_authentication_protected_spend_action(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    request: &AuthenticationProtectedSpendAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::AuthenticationTest {
        return Err(ActionRenderError::Capability);
    }
    let (mint_image, mint_implementation) =
        authentication_components(cell, &request.mint, &request.identity_provider)?;
    render_authentication_protected_spend_job(&AuthenticationProtectedSpendJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        mint: &request.mint,
        identity_provider: &request.identity_provider,
        mint_image,
        mint_implementation,
    })
    .map_err(ActionRenderError::from)
}

fn render_authentication_replay_action(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    request: &AuthenticationReplayAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::AuthenticationTest {
        return Err(ActionRenderError::Capability);
    }
    let (mint_image, mint_implementation) =
        authentication_components(cell, &request.mint, &request.identity_provider)?;
    render_authentication_replay_job(&AuthenticationReplayJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        mint: &request.mint,
        identity_provider: &request.identity_provider,
        mint_image,
        mint_implementation,
        session_secret: &request.session_secret,
        source_operation_id: &request.source_operation_id,
    })
    .map_err(ActionRenderError::from)
}

fn authentication_components<'a>(
    cell: &'a ProofstormCell,
    mint: &str,
    identity_provider: &str,
) -> Result<(&'a str, &'static str), ActionRenderError> {
    // The link check below and the catalog decide whether auth is supported.
    let implementation = cell
        .spec
        .cell
        .components
        .iter()
        .find(|component| component.id == mint)
        .map_or("nutshell", |component| {
            match component.implementation.as_str() {
                "cdk" => "cdk",
                _ => "nutshell",
            }
        });
    let mint_image = locked_component_image(cell, mint, ComponentKind::Mint, implementation)?;
    locked_component_image(
        cell,
        identity_provider,
        ComponentKind::IdentityProvider,
        "keycloak",
    )?;
    let links = cell
        .spec
        .cell
        .links
        .iter()
        .filter(|link| {
            link.kind == proofstorm_core::LinkKind::AuthenticationBackend
                && link.from == mint
                && link.to == identity_provider
                && matches!(
                    link.binding.as_ref(),
                    Some(proofstorm_core::DependencyBinding::Authentication {
                        protocol: proofstorm_core::AuthenticationProtocol::Oidc
                    })
                )
        })
        .count();
    if links != 1 {
        return Err(ActionRenderError::InvalidPlan(format!(
            "authentication conformance requires exactly one OIDC link from {mint:?} to {identity_provider:?}, found {links}"
        )));
    }
    Ok((mint_image, implementation))
}

fn render_native_exec_action(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    request: &ComponentForensicsAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::ComponentForensics {
        return Err(ActionRenderError::Capability);
    }
    if request.script.is_empty() || request.script.len() > 16 * 1024 {
        return Err(ActionRenderError::Bounds(
            "script must contain 1..=16384 UTF-8 bytes",
        ));
    }
    if !(1..=300).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 1..=300",
        ));
    }

    let plans = crate::compile_component_plans(
        &cell.spec.instance_key,
        &cell.spec.revision_digest,
        &cell.spec.cell,
        &cell.spec.lock,
    )
    .map_err(|error| ActionRenderError::InvalidPlan(error.to_string()))?;
    let component = plans
        .iter()
        .find(|plan| plan.component_id == request.component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.component.clone()))?;
    let target = plans
        .iter()
        .find(|plan| plan.component_id == request.target_component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.target_component.clone()))?;
    let context = native_exec_component_context(component)?;
    let mut environment = vec![
        ("PROOFSTORM_COMPONENT".to_owned(), request.component.clone()),
        (
            "PROOFSTORM_EXEC_COMPONENT".to_owned(),
            request.component.clone(),
        ),
        ("PROOFSTORM_SCRIPT".to_owned(), request.script.clone()),
    ];
    environment.extend(context.environment);
    environment.extend(native_exec_target_environment(
        &action.spec.instance_key,
        target,
    ));

    // A non-zero native exit is experiment data, not an infrastructure failure.
    // The controller reads the bounded pod log and this small exit metadata.
    let wrapper = "set +e; /bin/sh -c \"$PROOFSTORM_SCRIPT\"; code=$?; printf '{\"exit_code\":%s}' \"$code\" >/dev/termination-log; exit 0";
    let mut exec_container = container_with_env(
        "exec",
        &component.execution_context.image,
        wrapper,
        &context.mounts,
        environment,
    );
    exec_container["env"]
        .as_array_mut()
        .expect("native exec environment is an array")
        .extend(context.secret_environment);
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(1000),
        "affinity": instance_affinity(&action.spec.instance_key),
        "containers": [exec_container],
        "volumes": context.volumes,
    });
    let mut rendered = job(
        &action.name_any(),
        &instance_namespace(&action.spec.instance_key),
        &action.spec.instance_key,
        "native-exec",
        i64::from(request.timeout_seconds) + 10,
        &pod,
    )
    .map_err(ActionRenderError::from)?;
    let labels = rendered
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .and_then(|metadata| metadata.labels.as_mut())
        .expect("internally rendered exec pod has labels");
    // Native execution must observe the exact same network policy (including
    // active partitions) as its execution component. A distinct service target
    // does not change the caller's network identity or bypass a partition. It
    // must not match the wider policy used by portable controller-action jobs.
    labels.remove("proofstorm.dev/operation");
    labels.insert(
        "proofstorm.dev/network-identity".to_owned(),
        request.component.clone(),
    );
    Ok(rendered)
}

struct NativeExecComponentContext {
    mounts: Vec<Value>,
    volumes: Vec<Value>,
    environment: Vec<(String, String)>,
    secret_environment: Vec<Value>,
}

fn native_exec_component_context(
    plan: &ComponentPlanContract,
) -> Result<NativeExecComponentContext, ActionRenderError> {
    let mut context = NativeExecComponentContext {
        mounts: Vec::new(),
        volumes: Vec::new(),
        environment: plan
            .execution_context
            .environment
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    value.replace("{component_id}", &plan.component_id),
                )
            })
            .collect(),
        secret_environment: vec![],
    };
    if let EffectiveComponentConfig::Postgres(_) = &plan.effective_config {
        let secret_name = format!("{}-credentials", plan.component_id);
        context.environment.extend([
            ("PGHOST".into(), plan.component_id.clone()),
            ("PGPORT".into(), "5432".into()),
            ("PGUSER".into(), "proofstorm".into()),
            // Maintenance database; linked components own their databases.
            ("PGDATABASE".into(), "postgres".into()),
        ]);
        context.secret_environment.push(json!({
            "name": "PGPASSWORD",
            "valueFrom": {"secretKeyRef": {"name": secret_name, "key": "POSTGRES_PASSWORD"}}
        }));
    }
    for binding in &plan.execution_context.mounts {
        context
            .mounts
            .push(mount(&binding.name, &binding.mount_path, binding.read_only));
        let source = match binding.source {
            ExecutionStorageSource::StatefulData => {
                json!({"persistentVolumeClaim": {"claimName": format!("data-{}-0", plan.component_id)}})
            }
            ExecutionStorageSource::ComponentPersistentData => {
                json!({"persistentVolumeClaim": {"claimName": format!("{}-data", plan.component_id)}})
            }
            ExecutionStorageSource::ComponentConfig => {
                json!({"configMap": {"name": format!("{}-config", plan.component_id)}})
            }
            ExecutionStorageSource::LinkedStatefulData { ref link_id } => {
                let target = plan
                    .relevant_links
                    .iter()
                    .find(|link| link.id == *link_id && link.from == plan.component_id)
                    .ok_or_else(|| {
                        ActionRenderError::InvalidPlan(format!(
                            "component {:?} lacks resolved execution binding {link_id:?}",
                            plan.component_id,
                        ))
                    })?;
                let credentials = plan
                    .credentials
                    .iter()
                    .filter(|credential| credential.mount_name == binding.name)
                    .collect::<Vec<_>>();
                let [credential] = credentials.as_slice() else {
                    return Err(ActionRenderError::InvalidPlan(format!(
                        "component {:?} execution mount {:?} requires exactly one compiled credential, found {}",
                        plan.component_id,
                        binding.name,
                        credentials.len()
                    )));
                };
                if credential.source_component_id != target.to {
                    return Err(ActionRenderError::InvalidPlan(format!(
                        "component {:?} execution binding {link_id:?} target {:?} does not match credential source {:?}",
                        plan.component_id, target.to, credential.source_component_id
                    )));
                }
                json!({"persistentVolumeClaim": {"claimName": credential.claim_name}})
            }
        };
        let mut volume = json!({"name": binding.name});
        volume
            .as_object_mut()
            .expect("execution volume is an object")
            .extend(
                source
                    .as_object()
                    .expect("volume source is an object")
                    .clone(),
            );
        context.volumes.push(volume);
    }
    Ok(context)
}

fn native_exec_target_environment(
    instance_key: &str,
    target: &ComponentPlanContract,
) -> Vec<(String, String)> {
    let namespace = instance_namespace(instance_key);
    let ports = &target.target_descriptor.ports;
    let mut environment = vec![
        (
            "PROOFSTORM_TARGET_COMPONENT".to_owned(),
            target.component_id.clone(),
        ),
        (
            "PROOFSTORM_TARGET_KIND".to_owned(),
            serde_json::to_value(target.kind)
                .expect("component kind serializes")
                .as_str()
                .expect("component kind is a string")
                .to_owned(),
        ),
        (
            "PROOFSTORM_TARGET_IMPLEMENTATION".to_owned(),
            target.backend_id.clone(),
        ),
        (
            "PROOFSTORM_TARGET_HOST".to_owned(),
            target.component_id.clone(),
        ),
        (
            "PROOFSTORM_TARGET_FQDN".to_owned(),
            format!("{}.{namespace}.svc", target.component_id),
        ),
        (
            "PROOFSTORM_TARGET_SERVICES_JSON".to_owned(),
            serde_json::to_string(&ports).expect("component ports serialize"),
        ),
    ];
    environment.extend(ports.iter().map(|(name, port)| {
        (
            format!("PROOFSTORM_TARGET_PORT_{}", name.to_ascii_uppercase()),
            port.to_string(),
        )
    }));
    match target.kind {
        ComponentKind::Bitcoin => environment.extend([
            ("BITCOIN_RPC_HOST".to_owned(), target.component_id.clone()),
            (
                "BITCOIN_RPC_PORT".to_owned(),
                ports.get("rpc").copied().unwrap_or_default().to_string(),
            ),
            ("BITCOIN_RPC_USER".to_owned(), "proofstorm".to_owned()),
            (
                "BITCOIN_RPC_PASSWORD".to_owned(),
                "proofstorm-regtest-only".to_owned(),
            ),
        ]),
        ComponentKind::Lightning if target.backend_id == "lnd" => environment.extend([
            ("LND_RPC_HOST".to_owned(), target.component_id.clone()),
            (
                "LND_RPC_PORT".to_owned(),
                ports.get("rpc").copied().unwrap_or_default().to_string(),
            ),
        ]),
        ComponentKind::Lightning if target.backend_id == "cln" => {
            environment.push(("CLN_P2P_HOST".to_owned(), target.component_id.clone()));
        }
        ComponentKind::Mint => environment.push((
            "CASHU_MINT_URL".to_owned(),
            format!(
                "http://{}:{}",
                target.component_id,
                ports.get("http").copied().unwrap_or_default()
            ),
        )),
        ComponentKind::Database => environment.extend([
            (
                "PROOFSTORM_DATABASE_HOST".to_owned(),
                target.component_id.clone(),
            ),
            (
                "PROOFSTORM_DATABASE_PORT".to_owned(),
                ports
                    .get("postgres")
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            ),
        ]),
        _ => {}
    }
    environment
}

fn render_reachability_oracle_action(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    request: &ReachabilityOracleAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::OracleRun {
        return Err(ActionRenderError::Capability);
    }
    validate_reachability_oracle_action(request)?;
    let source = cell
        .spec
        .cell
        .components
        .iter()
        .find(|component| component.id == request.from_component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.from_component.clone()))?;
    let destination = cell
        .spec
        .cell
        .components
        .iter()
        .find(|component| component.id == request.to_component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.to_component.clone()))?;
    let port = component_ports(destination)
        .get(&request.service)
        .copied()
        .ok_or_else(|| ActionRenderError::UnknownService {
            component: destination.id.clone(),
            service: request.service.clone(),
        })?;
    let deadline = i64::from(request.timeout_seconds * request.attempts + 15);
    let script = format!(
        "set -eu; reachable=false; completed=0; i=1; while test \"$i\" -le {attempts}; do completed=$i; if nc -z -w {timeout} {destination} {port}; then reachable=true; break; fi; i=$((i+1)); done; printf '{{\"from_component\":\"{source}\",\"to_component\":\"{destination}\",\"service\":\"{service}\",\"port\":{port},\"reachable\":%s,\"attempts\":%s,\"timeout_seconds\":{timeout}}}' \"$reachable\" \"$completed\" >/dev/termination-log",
        attempts = request.attempts,
        timeout = request.timeout_seconds,
        destination = destination.id,
        source = source.id,
        service = request.service,
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(1000), "affinity": instance_affinity(&action.spec.instance_key),
        "containers": [container("oracle", REACHABILITY_PROBE_IMAGE, &script, &[])]
    });
    let mut rendered = job(
        &action.name_any(),
        &instance_namespace(&action.spec.instance_key),
        &action.spec.instance_key,
        "reachability-oracle",
        deadline,
        &pod,
    )?;
    let labels = rendered
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .and_then(|metadata| metadata.labels.as_mut())
        .expect("internally rendered probe pod has labels");
    // The Pod must not match the controller-action firewall exception. Giving it
    // the source component identity makes the cell's actual source policy govern
    // this observation, including any active partitions.
    labels.remove("proofstorm.dev/operation");
    labels.insert(
        "proofstorm.dev/network-identity".to_owned(),
        request.from_component.clone(),
    );
    Ok(rendered)
}

fn validate_action_identity(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
) -> Result<(), ActionRenderError> {
    for (matches, field) in [
        (action.spec.cell_name == cell.name_any(), "cell_name"),
        (
            action.spec.workspace_id == cell.spec.workspace_id,
            "workspace_id",
        ),
        (
            action.spec.instance_id == cell.spec.instance_id,
            "instance_id",
        ),
        (
            action.spec.instance_key == cell.spec.instance_key,
            "instance_key",
        ),
    ] {
        if !matches {
            return Err(ActionRenderError::Identity(field));
        }
    }
    Ok(())
}

fn validate_reachability_oracle_action(
    request: &ReachabilityOracleAction,
) -> Result<(), ActionRenderError> {
    if request.from_component == request.to_component {
        return Err(ActionRenderError::Bounds(
            "from_component and to_component must differ",
        ));
    }
    if !(1..=5).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 1..=5",
        ));
    }
    if !(1..=5).contains(&request.attempts) {
        return Err(ActionRenderError::Bounds("attempts must be in 1..=5"));
    }
    Ok(())
}

fn locked_component_image<'a>(
    cell: &'a ProofstormCell,
    id: &str,
    kind: ComponentKind,
    implementation: &'static str,
) -> Result<&'a str, ActionRenderError> {
    let valid = cell.spec.cell.components.iter().any(|component| {
        component.id == id && component.kind == kind && component.implementation == implementation
    });
    if !valid {
        return Err(ActionRenderError::Component {
            component: id.to_owned(),
            implementation,
            kind,
        });
    }
    cell.spec
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id && entry.catalog_id == implementation)
        .map(|entry| entry.image.as_str())
        .ok_or_else(|| ActionRenderError::MissingLock(id.to_owned()))
}

fn mark_controller_owned(job: &mut Job, action_name: &str) {
    for labels in [
        job.metadata.labels.as_mut(),
        job.spec
            .as_mut()
            .and_then(|spec| spec.template.metadata.as_mut())
            .and_then(|metadata| metadata.labels.as_mut()),
    ]
    .into_iter()
    .flatten()
    {
        labels.insert(
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstormd".to_owned(),
        );
        labels.insert("proofstorm.dev/action".to_owned(), action_name.to_owned());
    }
}

/// Run the fixed secret-bearing authentication baseline in the locked mint image.
///
/// # Panics
///
/// Panics only if the controller-owned container template stops rendering its
/// environment as a JSON array.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_authentication_conformance_job(
    spec: &AuthenticationConformanceJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let AuthenticationConformanceJobSpec {
        resource_name,
        instance_key,
        mint,
        identity_provider,
        mint_image,
        mint_implementation,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let mint_url = format!("http://{mint}:3338");
    let script = "exec /opt/proofstorm/driver authentication conformance > /dev/termination-log";
    let mut authentication = container_with_env(
        "authentication",
        mint_image,
        script,
        &[],
        vec![
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_MINT_IMPLEMENTATION", mint_implementation),
            ("PROOFSTORM_IDENTITY_PROVIDER", identity_provider),
            ("PROOFSTORM_MINT_URL", mint_url.as_str()),
        ],
    );
    authentication["env"]
        .as_array_mut()
        .expect("authentication environment is an array")
        .extend([
            json!({
                "name": "OIDC_TEST_USERNAME",
                "valueFrom": {"secretKeyRef": {
                    "name": format!("{identity_provider}-credentials"),
                    "key": "OIDC_TEST_USERNAME"
                }}
            }),
            json!({
                "name": "OIDC_TEST_PASSWORD",
                "valueFrom": {"secretKeyRef": {
                    "name": format!("{identity_provider}-credentials"),
                    "key": "OIDC_TEST_PASSWORD"
                }}
            }),
        ]);
    // This driver handles every exception and emits a fixed diagnostic. Do not
    // fall back to container logs: an unexpected library traceback is not a
    // valid secret-bearing artifact.
    authentication["terminationMessagePolicy"] = json!("File");
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(1000),
        "affinity": instance_affinity(instance_key),
        "containers": [authentication]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "authentication-conformance",
        120,
        &pod,
    )
}

/// Mint and spend a BAT while keeping the token in a private termination message.
///
/// # Panics
///
/// Panics only if the controller-owned container template stops rendering its
/// environment as a JSON array.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_authentication_protected_spend_job(
    spec: &AuthenticationProtectedSpendJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let AuthenticationProtectedSpendJobSpec {
        resource_name,
        instance_key,
        mint,
        identity_provider,
        mint_image,
        mint_implementation,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let mint_url = format!("http://{mint}:3338");
    let script =
        "exec /opt/proofstorm/driver authentication protected-spend > /dev/termination-log";
    let mut authentication = container_with_env(
        "authentication",
        mint_image,
        script,
        &[],
        vec![
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_MINT_IMPLEMENTATION", mint_implementation),
            ("PROOFSTORM_IDENTITY_PROVIDER", identity_provider),
            ("PROOFSTORM_MINT_URL", mint_url.as_str()),
        ],
    );
    authentication["env"]
        .as_array_mut()
        .expect("authentication environment is an array")
        .extend(authentication_identity_environment(identity_provider));
    authentication["terminationMessagePolicy"] = json!("File");
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(1000),
        "affinity": instance_affinity(instance_key),
        "containers": [authentication]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "authentication-protected-spend",
        120,
        &pod,
    )
}

/// Replay a private spent BAT and prove that a fresh BAT still works.
///
/// # Panics
///
/// Panics only if the controller-owned container template stops rendering its
/// environment as a JSON array.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_authentication_replay_job(
    spec: &AuthenticationReplayJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let AuthenticationReplayJobSpec {
        resource_name,
        instance_key,
        mint,
        identity_provider,
        mint_image,
        mint_implementation,
        session_secret,
        source_operation_id,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let mint_url = format!("http://{mint}:3338");
    let script = "exec /opt/proofstorm/driver authentication replay > /dev/termination-log";
    let mut authentication = container_with_env(
        "authentication",
        mint_image,
        script,
        &[],
        vec![
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_MINT_IMPLEMENTATION", mint_implementation),
            ("PROOFSTORM_IDENTITY_PROVIDER", identity_provider),
            ("PROOFSTORM_MINT_URL", mint_url.as_str()),
            ("PROOFSTORM_SOURCE_OPERATION_ID", source_operation_id),
        ],
    );
    let mut private_environment = authentication_identity_environment(identity_provider);
    private_environment.push(json!({
        "name": "PROOFSTORM_SPENT_BAT",
        "valueFrom": {"secretKeyRef": {
            "name": session_secret,
            "key": "SPENT_BAT"
        }}
    }));
    authentication["env"]
        .as_array_mut()
        .expect("authentication environment is an array")
        .extend(private_environment);
    authentication["terminationMessagePolicy"] = json!("File");
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(1000),
        "affinity": instance_affinity(instance_key),
        "containers": [authentication]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "authentication-replay",
        120,
        &pod,
    )
}

fn authentication_identity_environment(identity_provider: &str) -> Vec<Value> {
    vec![
        json!({
            "name": "OIDC_TEST_USERNAME",
            "valueFrom": {"secretKeyRef": {
                "name": format!("{identity_provider}-credentials"),
                "key": "OIDC_TEST_USERNAME"
            }}
        }),
        json!({
            "name": "OIDC_TEST_PASSWORD",
            "valueFrom": {"secretKeyRef": {
                "name": format!("{identity_provider}-credentials"),
                "key": "OIDC_TEST_PASSWORD"
            }}
        }),
    ]
}

fn job(
    name: &str,
    namespace: &str,
    instance_key: &str,
    operation: &str,
    deadline_seconds: i64,
    pod: &Value,
) -> Result<Job, serde_json::Error> {
    let mut pod: k8s_openapi::api::core::v1::PodSpec = serde_json::from_value(pod.clone())?;
    if serde_json::to_string(&pod)?.contains(crate::drivers::DRIVER_PATH) {
        crate::drivers::install(&mut pod)?;
    }
    resource(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": metadata(name, namespace, instance_key, operation),
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": deadline_seconds,
            "ttlSecondsAfterFinished": 600,
            "template": {
                "metadata": {"labels": labels(instance_key, operation)},
                "spec": pod
            }
        }
    }))
}

fn metadata(name: &str, namespace: &str, instance_key: &str, operation: &str) -> Value {
    json!({"name": name, "namespace": namespace, "labels": labels(instance_key, operation)})
}

fn labels(instance_key: &str, operation: &str) -> Value {
    json!({"proofstorm.dev/instance": instance_key, "proofstorm.dev/operation": operation,
        "app.kubernetes.io/managed-by": "proofstorm-mcp"})
}

fn mount(name: &str, path: &str, read_only: bool) -> Value {
    json!({"name": name, "mountPath": path, "readOnly": read_only})
}

fn container(name: &str, image: &str, script: &str, mounts: &[Value]) -> Value {
    container_with_env(name, image, script, mounts, Vec::<(&str, &str)>::new())
}

fn container_with_env<N, V>(
    name: &str,
    image: &str,
    script: &str,
    mounts: &[Value],
    environment: Vec<(N, V)>,
) -> Value
where
    N: AsRef<str>,
    V: AsRef<str>,
{
    // Typed scripts send native command output to the Pod log, which the
    // controller does not read for a failed action. FallbackToLogsOnError makes
    // Kubernetes itself populate the termination message from that log whenever
    // a container exits non-zero without writing its own diagnostic, so a
    // failure carries the native error instead of a bare exit code.
    json!({"name": name, "image": image, "imagePullPolicy": "IfNotPresent",
        "command": ["/bin/sh", "-c", script], "volumeMounts": mounts,
        "env": environment.into_iter().map(|(name, value)| json!({"name": name.as_ref(), "value": value.as_ref()})).collect::<Vec<_>>(),
        "terminationMessagePolicy": "FallbackToLogsOnError",
        "securityContext": container_security()})
}

fn resource(value: Value) -> Result<Job, serde_json::Error> {
    serde_json::from_value(value)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proofstorm_core::{
        API_VERSION, CellPolicy, CellSpec, ComponentCondition, ComponentSpec, ComponentStatus,
        ControlClass, default_catalog, resolve_lock,
    };

    use super::*;

    fn action_fixture() -> (ProofstormCell, ProofstormCellAction) {
        let component = |id: &str, kind: ComponentKind, implementation: &str| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: match implementation {
                "bitcoin-core" => "bitcoin-core/31/v1",
                "lnd" => "lnd/0.20/v1",
                "cln" => "cln/26.06/v1",
                "cdk" => "cdk-mintd/0.18/v1",
                "nutshell-wallet" => "nutshell-wallet/0.20/v1",
                _ => panic!("unknown test implementation {implementation:?}"),
            }
            .into(),
            control: if kind == ComponentKind::Mint {
                ControlClass::Target
            } else {
                ControlClass::Cell
            },
            config: BTreeMap::new(),
        };
        let cell_spec = CellSpec {
            api_version: API_VERSION.into(),
            name: "action-cell".into(),
            components: vec![
                component("chain", ComponentKind::Bitcoin, "bitcoin-core"),
                component("chain-b", ComponentKind::Bitcoin, "bitcoin-core"),
                component("mint-lnd", ComponentKind::Lightning, "lnd"),
                component("payer-lnd", ComponentKind::Lightning, "lnd"),
                component("workspace-cln", ComponentKind::Lightning, "cln"),
                component("mint", ComponentKind::Mint, "cdk"),
                component("wallet", ComponentKind::Wallet, "nutshell-wallet"),
            ],
            links: vec![proofstorm_core::LinkSpec {
                id: "mint-bolt11".into(),
                kind: proofstorm_core::LinkKind::PaymentBackend,
                from: "mint".into(),
                to: "mint-lnd".into(),
                binding: Some(proofstorm_core::DependencyBinding::Payment {
                    method: proofstorm_core::PaymentMethod::Bolt11,
                    unit: "sat".into(),
                }),
            }],
            policy: CellPolicy::default(),
        };
        let lock = resolve_lock(&cell_spec, default_catalog()).expect("lock");
        let cell = ProofstormCell::new(
            "cell-resource",
            crate::ProofstormCellSpec {
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                revision_digest: "sha256:revision".into(),
                lock,
                cell: cell_spec,
            },
        );
        let action = ProofstormCellAction::new(
            "action-123",
            crate::ProofstormCellActionSpec {
                access_scope: None,
                cell_name: "cell-resource".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                experiment_id: "experiment".into(),
                session_id: "session".into(),
                principal_id: "principal".into(),
                sequence: 1,
                operation_id: "probe".into(),
                request_digest: "sha256:request".into(),
                capability: Capability::OracleRun,
                accepted_at_unix: 1,
                action: CellAction::ReachabilityOracle(ReachabilityOracleAction {
                    from_component: "wallet".into(),
                    to_component: "mint".into(),
                    service: "http".into(),
                    timeout_seconds: 3,
                    attempts: 1,
                }),
            },
        );
        (cell, action)
    }

    fn ready_admission_status(cell: &mut ProofstormCell) {
        let plans = crate::compile_component_plans(
            &cell.spec.instance_key,
            &cell.spec.revision_digest,
            &cell.spec.cell,
            &cell.spec.lock,
        )
        .expect("plans");
        let components = plans
            .iter()
            .map(|plan| ComponentStatus {
                protocol_observation: Some(proofstorm_core::ProtocolObservation {
                    observed_at_unix: 0,
                    expires_at_unix: i64::MAX,
                    elapsed_micros: 1,
                }),
                id: plan.component_id.clone(),
                kind: plan.kind,
                observed_revision_digest: plan.revision_digest.clone(),
                observed_rollout_digest: plan.rollout_digest.clone(),
                conditions: plan
                    .applicable_conditions
                    .iter()
                    .map(|condition_type| ComponentCondition {
                        condition_type: *condition_type,
                        state: ComponentConditionState::True,
                        reason: match condition_type {
                            ComponentConditionType::WorkloadReady => {
                                ComponentConditionReason::WorkloadAvailable
                            }
                            ComponentConditionType::StorageReady => {
                                ComponentConditionReason::StorageBound
                            }
                            ComponentConditionType::CredentialsReady => {
                                ComponentConditionReason::CredentialsProjected
                            }
                            ComponentConditionType::ServiceReady => {
                                ComponentConditionReason::EndpointsReady
                            }
                            ComponentConditionType::ProtocolReady => {
                                ComponentConditionReason::ProtocolResponding
                            }
                            ComponentConditionType::DependenciesReady => {
                                ComponentConditionReason::DependenciesSatisfied
                            }
                            ComponentConditionType::ComponentReady => {
                                ComponentConditionReason::ComponentOperational
                            }
                            ComponentConditionType::ExperimentControllable => {
                                ComponentConditionReason::ControlAvailable
                            }
                        },
                        message: "ready".into(),
                        last_transition_unix: 1,
                    })
                    .collect(),
                ready: true,
                service: format!("{}.instance.svc", plan.component_id),
                ports: plan.target_descriptor.ports.clone(),
            })
            .collect();
        cell.status = Some(crate::ProofstormCellStatus {
            phase: crate::CellPhase::Ready,
            observed_revision_digest: cell.spec.revision_digest.clone(),
            observed_protocol_probe_lease: Some("lease-current".into()),
            components,
            ..crate::ProofstormCellStatus::default()
        });
    }

    fn set_condition(
        cell: &mut ProofstormCell,
        component: &str,
        condition_type: ComponentConditionType,
        state: ComponentConditionState,
        reason: ComponentConditionReason,
    ) {
        let condition = cell
            .status
            .as_mut()
            .expect("status")
            .components
            .iter_mut()
            .find(|status| status.id == component)
            .expect("component status")
            .conditions
            .iter_mut()
            .find(|condition| condition.condition_type == condition_type)
            .expect("condition");
        condition.state = state;
        condition.reason = reason;
    }

    #[test]
    fn closing_cells_refuse_new_work_even_when_components_are_ready() {
        let (mut cell, action) = action_fixture();
        ready_admission_status(&mut cell);
        for phase in [crate::CellPhase::Closing, crate::CellPhase::CleanupBlocked] {
            cell.status.as_mut().expect("status").phase = phase;
            assert_eq!(
                evaluate_action_admission(&action, &cell),
                Err(ActionAdmissionError::CellClosing)
            );
        }
        cell.status.as_mut().expect("status").phase = crate::CellPhase::Ready;
        cell.metadata.deletion_timestamp =
            Some(serde_json::from_value(json!("2026-09-06T00:00:00Z")).expect("timestamp"));
        assert_eq!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::CellClosing)
        );
    }

    #[test]
    fn admission_uses_operation_prerequisites_instead_of_cell_ready() {
        let (mut cell, mut action) = action_fixture();

        action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "mint-lnd".into(),
            script: "bitcoin-cli -help".into(),
            timeout_seconds: 30,
        });
        assert!(
            evaluate_action_admission(&action, &cell).is_ok(),
            "immutable execution and target contracts do not require cell readiness"
        );

        ready_admission_status(&mut cell);
        cell.status.as_mut().expect("status").phase = crate::CellPhase::Pending;
        set_condition(
            &mut cell,
            "chain",
            ComponentConditionType::ProtocolReady,
            ComponentConditionState::False,
            ComponentConditionReason::ProtocolProbeFailed,
        );
        action.spec.action = CellAction::NodeStart(crate::ComponentControlAction {
            component: "chain".into(),
        });
        assert!(
            evaluate_action_admission(&action, &cell).is_ok(),
            "start depends on storage, not protocol or aggregate cell phase"
        );

        set_condition(
            &mut cell,
            "chain",
            ComponentConditionType::StorageReady,
            ComponentConditionState::False,
            ComponentConditionReason::StoragePending,
        );
        assert!(matches!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::Storage,
                ..
            })
        ));
    }

    #[test]
    fn admission_allows_stopped_recovery_and_rejects_unhealthy_mutation() {
        let (mut cell, mut action) = action_fixture();
        ready_admission_status(&mut cell);
        set_condition(
            &mut cell,
            "mint",
            ComponentConditionType::WorkloadReady,
            ComponentConditionState::False,
            ComponentConditionReason::IntentionallyStopped,
        );
        set_condition(
            &mut cell,
            "mint",
            ComponentConditionType::ProtocolReady,
            ComponentConditionState::False,
            ComponentConditionReason::IntentionallyStopped,
        );

        action.spec.action = CellAction::NodeRestart(crate::ComponentControlAction {
            component: "mint".into(),
        });
        assert!(evaluate_action_admission(&action, &cell).is_ok());

        action.spec.action =
            CellAction::AuthenticationConformance(AuthenticationConformanceAction {
                mint: "mint".into(),
                identity_provider: "wallet".into(),
            });
        assert_eq!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                component: "mint".into(),
                operation: OperationClass::Authentication,
                prerequisite: ReadinessPrerequisite::Protocol,
                condition: Some(ComponentConditionType::ProtocolReady),
                state: Some(ComponentConditionState::False),
                reason: Some(ComponentConditionReason::IntentionallyStopped),
            })
        );
    }

    #[test]
    fn workload_identity_rejects_stale_status_but_network_control_does_not() {
        let (mut cell, mut action) = action_fixture();
        ready_admission_status(&mut cell);
        cell.status
            .as_mut()
            .expect("status")
            .components
            .iter_mut()
            .find(|status| status.id == "chain")
            .expect("chain")
            .observed_rollout_digest = "sha256:stale".into();

        action.spec.action = CellAction::NodeStop(crate::ComponentControlAction {
            component: "chain".into(),
        });
        assert!(matches!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::WorkloadIdentity,
                ..
            })
        ));

        action.spec.action = CellAction::NetworkPartition(crate::NetworkPartitionAction {
            from_component: "chain".into(),
            to_component: "mint-lnd".into(),
        });
        assert!(evaluate_action_admission(&action, &cell).is_ok());
    }

    #[test]
    fn read_only_wallet_inspection_survives_mint_protocol_failure() {
        let (mut cell, mut action) = action_fixture();
        ready_admission_status(&mut cell);
        set_condition(
            &mut cell,
            "mint",
            ComponentConditionType::ProtocolReady,
            ComponentConditionState::False,
            ComponentConditionReason::ProtocolProbeFailed,
        );

        action.spec.action = CellAction::ComponentExecLive(crate::ComponentExecLiveAction {
            component: "wallet".into(),
            script: "cashu -w wallet balance".into(),
            argv: vec![],
            timeout_seconds: 30,
            output: proofstorm_core::native::NativeOutput::default(),
            private_payload: None,
        });
        assert!(evaluate_action_admission(&action, &cell).is_ok());

        action.spec.action =
            CellAction::AuthenticationConformance(AuthenticationConformanceAction {
                mint: "mint".into(),
                identity_provider: "wallet".into(),
            });
        assert_eq!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                component: "mint".into(),
                operation: OperationClass::Authentication,
                prerequisite: ReadinessPrerequisite::Protocol,
                condition: Some(ComponentConditionType::ProtocolReady),
                state: Some(ComponentConditionState::False),
                reason: Some(ComponentConditionReason::ProtocolProbeFailed),
            })
        );
    }

    #[test]
    fn stale_cell_revision_fences_runtime_admission_only() {
        let (mut cell, mut action) = action_fixture();
        ready_admission_status(&mut cell);
        cell.status
            .as_mut()
            .expect("status")
            .observed_revision_digest = "sha256:previous-revision".into();

        action.spec.action = CellAction::NodeRestart(crate::ComponentControlAction {
            component: "chain".into(),
        });
        assert!(matches!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::WorkloadIdentity,
                state: None,
                reason: None,
                ..
            })
        ));

        action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "chain-b".into(),
            script: "bitcoin-cli -help".into(),
            timeout_seconds: 30,
        });
        assert!(
            evaluate_action_admission(&action, &cell).is_ok(),
            "a newly compiled immutable execution contract does not consume stale status"
        );
    }

    #[test]
    fn expired_observation_fences_protocol_admission_without_blocking_recovery() {
        let (mut cell, mut action) = action_fixture();
        ready_admission_status(&mut cell);
        for component in &mut cell.status.as_mut().expect("status").components {
            component
                .protocol_observation
                .as_mut()
                .expect("timing")
                .expires_at_unix = 1;
        }

        action.spec.action =
            CellAction::AuthenticationConformance(AuthenticationConformanceAction {
                mint: "mint".into(),
                identity_provider: "wallet".into(),
            });
        assert!(matches!(
            evaluate_action_admission(&action, &cell),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::Dependencies | ReadinessPrerequisite::Protocol,
                state: Some(ComponentConditionState::Unknown),
                ..
            })
        ));

        action.spec.action = CellAction::NodeStart(crate::ComponentControlAction {
            component: "mint-lnd".into(),
        });
        assert!(evaluate_action_admission(&action, &cell).is_ok());
        action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
            component: "mint-lnd".into(),
            target_component: "payer-lnd".into(),
            script: "lncli --help".into(),
            timeout_seconds: 30,
        });
        assert!(evaluate_action_admission(&action, &cell).is_ok());
    }

    #[test]
    fn authentication_conformance_job_keeps_credentials_in_secret_refs() {
        let job = render_authentication_conformance_job(&AuthenticationConformanceJobSpec {
            resource_name: "op-auth",
            instance_key: "i0123456789012345678",
            mint: "mint",
            identity_provider: "identity",
            mint_image: "nutshell-image",
            mint_implementation: "nutshell",
        })
        .expect("authentication conformance job");
        let encoded = serde_json::to_value(&job).expect("job JSON");
        let pod = &encoded["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        let container = &pod["containers"][0];
        assert_eq!(container["name"], "authentication");
        assert_eq!(container["image"], "nutshell-image");
        assert_eq!(container["terminationMessagePolicy"], "File");
        let environment = container["env"].as_array().expect("environment");
        for (name, key) in [
            ("OIDC_TEST_USERNAME", "OIDC_TEST_USERNAME"),
            ("OIDC_TEST_PASSWORD", "OIDC_TEST_PASSWORD"),
        ] {
            let variable = environment
                .iter()
                .find(|entry| entry["name"] == name)
                .expect("secret environment variable");
            assert_eq!(
                variable["valueFrom"]["secretKeyRef"]["name"],
                "identity-credentials"
            );
            assert_eq!(variable["valueFrom"]["secretKeyRef"]["key"], key);
            assert!(variable.get("value").is_none());
        }
        assert!(
            !environment
                .iter()
                .any(|entry| entry["name"] == "PROOFSTORM_AUTHENTICATION_DRIVER")
        );
        assert_eq!(
            container["command"].as_array().unwrap().last().unwrap(),
            "exec /opt/proofstorm/driver authentication conformance > /dev/termination-log"
        );
        assert_eq!(pod["initContainers"][0]["name"], "proofstorm-driver");

        let protected =
            render_authentication_protected_spend_job(&AuthenticationProtectedSpendJobSpec {
                resource_name: "op-auth-spend",
                instance_key: "i0123456789012345678",
                mint: "mint",
                identity_provider: "identity",
                mint_image: "nutshell-image",
                mint_implementation: "nutshell",
            })
            .expect("protected spend job");
        let protected = serde_json::to_value(protected).expect("protected job JSON");
        let protected_container = &protected["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(protected_container["terminationMessagePolicy"], "File");
        assert!(
            protected_container["env"]
                .as_array()
                .expect("environment")
                .iter()
                .any(|entry| entry["name"] == "OIDC_TEST_PASSWORD"
                    && entry["valueFrom"]["secretKeyRef"]["name"] == "identity-credentials")
        );

        let replay = render_authentication_replay_job(&AuthenticationReplayJobSpec {
            resource_name: "op-auth-replay",
            instance_key: "i0123456789012345678",
            mint: "mint",
            identity_provider: "identity",
            mint_image: "nutshell-image",
            mint_implementation: "nutshell",
            session_secret: "op-source-auth-session",
            source_operation_id: "auth-source",
        })
        .expect("authentication replay job");
        let replay = serde_json::to_value(replay).expect("replay job JSON");
        let replay_pod = &replay["spec"]["template"]["spec"];
        assert_eq!(replay_pod["automountServiceAccountToken"], false);
        let replay_environment = replay_pod["containers"][0]["env"]
            .as_array()
            .expect("replay environment");
        let spent_bat = replay_environment
            .iter()
            .find(|entry| entry["name"] == "PROOFSTORM_SPENT_BAT")
            .expect("private spent BAT");
        assert_eq!(
            spent_bat["valueFrom"]["secretKeyRef"]["name"],
            "op-source-auth-session"
        );
        assert_eq!(spent_bat["valueFrom"]["secretKeyRef"]["key"], "SPENT_BAT");
        assert!(spent_bat.get("value").is_none());
    }

    #[test]
    fn bounded_jobs_preserve_deadlines_security_and_instance_placement() {
        let (cell, action) = action_fixture();
        let job = render_cell_action_job(&action, &cell).expect("probe job");
        let spec = job.spec.expect("job spec");
        assert!(
            spec.active_deadline_seconds
                .is_some_and(|seconds| seconds > 0 && seconds <= 120)
        );
        let pod = spec.template.spec.expect("pod");
        assert_eq!(pod.automount_service_account_token, Some(false));
        assert_eq!(pod.enable_service_links, Some(false));
        assert_eq!(
            pod.service_account_name.as_deref(),
            Some("proofstorm-workload")
        );
        assert_eq!(
            serde_json::to_value(&pod.security_context).unwrap(),
            json!({
                "runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000, "fsGroup": 1000,
                "seccompProfile": {"type": "RuntimeDefault"}
            })
        );
        assert_eq!(
            serde_json::to_value(&pod.affinity).unwrap(),
            json!({"podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
                "labelSelector": {"matchLabels": {"proofstorm.dev/instance": action.spec.instance_key}},
                "topologyKey": "kubernetes.io/hostname"
            }]}})
        );
        for container in &pod.containers {
            assert_eq!(
                serde_json::to_value(&container.security_context).unwrap(),
                json!({"allowPrivilegeEscalation": false, "capabilities": {"drop": ["ALL"]}})
            );
        }
    }

    #[test]
    fn native_exec_uses_locked_component_image_data_and_uninterpolated_script() {
        let (cell, mut action) = action_fixture();
        let locked_bitcoin = cell
            .spec
            .lock
            .entries
            .iter()
            .find(|entry| entry.component_id == "chain")
            .expect("bitcoin lock")
            .image
            .clone();
        let script = "bitcoin-cli --help; printf '%s' '$NOT_EXPANDED_BY_RENDERER'";
        action.spec.capability = Capability::ComponentForensics;
        action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "chain".into(),
            script: script.into(),
            timeout_seconds: 30,
        });

        let job = render_cell_action_job(&action, &cell).expect("native exec job");
        let spec = job.spec.as_ref().expect("job spec");
        assert_eq!(spec.active_deadline_seconds, Some(40));
        let pod = spec.template.spec.as_ref().expect("pod");
        assert_eq!(pod.automount_service_account_token, Some(false));
        let exec = &pod.containers[0];
        assert_eq!(exec.name, "exec");
        assert_eq!(exec.image.as_deref(), Some(locked_bitcoin.as_str()));
        assert!(exec.command.as_ref().expect("wrapper command")[2].contains("$PROOFSTORM_SCRIPT"));
        assert!(!exec.command.as_ref().expect("wrapper command")[2].contains(script));
        assert_eq!(
            exec.env
                .as_ref()
                .expect("exec environment")
                .iter()
                .find(|entry| entry.name == "PROOFSTORM_SCRIPT")
                .and_then(|entry| entry.value.as_deref()),
            Some(script)
        );
        assert_eq!(
            pod.volumes.as_ref().expect("data volume")[0]
                .persistent_volume_claim
                .as_ref()
                .map(|claim| claim.claim_name.as_str()),
            Some("data-chain-0")
        );
        assert_eq!(action_result_container(&action.spec.action), "forensics");
        let pod_labels = job
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.labels.as_ref())
            .expect("pod labels");
        assert_eq!(
            pod_labels
                .get("proofstorm.dev/network-identity")
                .map(String::as_str),
            Some("chain")
        );
        assert!(!pod_labels.contains_key("proofstorm.dev/component"));
        assert!(!pod_labels.contains_key("proofstorm.dev/operation"));
    }

    #[test]
    fn native_exec_can_target_a_distinct_bitcoin_component() {
        let (cell, mut action) = action_fixture();
        action.spec.capability = Capability::ComponentForensics;
        action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "chain-b".into(),
            script: "bitcoin-cli getblockchaininfo".into(),
            timeout_seconds: 30,
        });

        let job = render_cell_action_job(&action, &cell).expect("targeted native exec job");
        let pod = job
            .spec
            .as_ref()
            .expect("job spec")
            .template
            .spec
            .as_ref()
            .expect("pod spec");
        let environment = pod.containers[0]
            .env
            .as_ref()
            .expect("target environment")
            .iter()
            .map(|entry| {
                (
                    entry.name.as_str(),
                    entry.value.as_deref().unwrap_or_default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(environment["PROOFSTORM_EXEC_COMPONENT"], "chain");
        assert_eq!(environment["PROOFSTORM_TARGET_COMPONENT"], "chain-b");
        assert_eq!(environment["PROOFSTORM_TARGET_HOST"], "chain-b");
        assert_eq!(
            environment["PROOFSTORM_TARGET_FQDN"],
            "chain-b.proofstorm-i0123456789012345678.svc"
        );
        assert_eq!(environment["PROOFSTORM_TARGET_PORT_RPC"], "18443");
        assert_eq!(environment["BITCOIN_RPC_HOST"], "chain-b");
        assert_eq!(environment["BITCOIN_RPC_PORT"], "18443");
        assert_eq!(environment["BITCOIN_RPC_USER"], "proofstorm");
        assert_eq!(
            environment["BITCOIN_RPC_PASSWORD"],
            "proofstorm-regtest-only"
        );
        let labels = job
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.labels.as_ref())
            .expect("pod labels");
        assert_eq!(labels["proofstorm.dev/network-identity"], "chain");
        assert!(!labels.contains_key("proofstorm.dev/component"));
    }

    #[test]
    fn native_exec_mounts_are_compiled_from_the_executor_plan() {
        let (cell, mut action) = action_fixture();
        action.spec.capability = Capability::ComponentForensics;
        action.spec.action = CellAction::ComponentForensics(ComponentForensicsAction {
            component: "mint".into(),
            target_component: "chain-b".into(),
            script: "cdk-mintd --help".into(),
            timeout_seconds: 30,
        });

        let job = render_cell_action_job(&action, &cell).expect("mint native exec");
        let pod = job
            .spec
            .as_ref()
            .expect("job spec")
            .template
            .spec
            .as_ref()
            .expect("pod spec");
        let volumes = pod.volumes.as_ref().expect("plan volumes");
        assert_eq!(
            volumes[0].config_map.as_ref().expect("config").name,
            "mint-config"
        );
        assert_eq!(
            volumes[1]
                .persistent_volume_claim
                .as_ref()
                .expect("mint data")
                .claim_name,
            "mint-data"
        );
        assert_eq!(
            volumes[2]
                .persistent_volume_claim
                .as_ref()
                .expect("linked LND data")
                .claim_name,
            "data-mint-lnd-0"
        );
        let mounts = pod.containers[0]
            .volume_mounts
            .as_ref()
            .expect("plan mounts");
        assert_eq!(mounts[2].mount_path, "/lnd");
        assert_eq!(mounts[2].read_only, Some(true));

        let mut plans = crate::compile_component_plans(
            &cell.spec.instance_key,
            &cell.spec.revision_digest,
            &cell.spec.cell,
            &cell.spec.lock,
        )
        .expect("compiled plans");
        let mint = plans
            .iter_mut()
            .find(|plan| plan.component_id == "mint")
            .expect("mint plan");
        mint.credentials[0].claim_name = "opaque-linked-state".into();
        let context = native_exec_component_context(mint).expect("compiled native context");
        assert_eq!(
            context.volumes[2]["persistentVolumeClaim"]["claimName"],
            "opaque-linked-state"
        );
    }

    #[test]
    fn action_is_identity_checked_and_controller_owned() {
        let (cell, mut action) = action_fixture();
        let mut document = serde_json::to_value(&action.spec).expect("serialize action");
        document["action"]["parameters"]["command"] = json!("unexpected field");
        assert!(serde_json::from_value::<crate::ProofstormCellActionSpec>(document).is_err());
        let job = render_cell_action_job(&action, &cell).expect("typed job");
        assert_eq!(job.metadata.name.as_deref(), Some("action-123"));
        assert_eq!(
            job.metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
                .map(String::as_str),
            Some("proofstormd")
        );
        action.spec.instance_id = "another-instance".into();
        assert!(matches!(
            render_cell_action_job(&action, &cell),
            Err(ActionRenderError::Identity("instance_id"))
        ));
    }

    #[test]
    fn node_lifecycle_is_typed_and_never_renders_a_privileged_job() {
        let (cell, mut action) = action_fixture();
        action.spec.capability = Capability::NodeControl;
        action.spec.action = CellAction::NodeRestart(crate::ComponentControlAction {
            component: "chain".into(),
        });
        let serialized = serde_json::to_value(&action.spec.action).expect("serialize action");
        assert_eq!(serialized["kind"], "node_restart");
        assert_eq!(serialized["parameters"]["component"], "chain");
        assert!(matches!(
            render_cell_action_job(&action, &cell),
            Err(ActionRenderError::Bounds(_))
        ));
        assert_eq!(action_result_container(&action.spec.action), "result");
    }

    #[test]
    fn reachability_oracle_uses_source_firewall_identity_and_advertised_service() {
        let (cell, mut action) = action_fixture();
        action.spec.capability = Capability::OracleRun;
        action.spec.action = CellAction::ReachabilityOracle(ReachabilityOracleAction {
            from_component: "wallet".into(),
            to_component: "mint".into(),
            service: "http".into(),
            timeout_seconds: 2,
            attempts: 3,
        });

        let job = render_cell_action_job(&action, &cell).expect("reachability job");
        assert!(
            job.metadata
                .labels
                .as_ref()
                .is_some_and(|labels| labels.contains_key("proofstorm.dev/operation"))
        );
        let template = &job.spec.as_ref().expect("job spec").template;
        let labels = template
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.labels.as_ref())
            .expect("pod labels");
        assert_eq!(
            labels
                .get("proofstorm.dev/network-identity")
                .map(String::as_str),
            Some("wallet")
        );
        assert!(!labels.contains_key("proofstorm.dev/component"));
        assert!(!labels.contains_key("proofstorm.dev/operation"));
        let pod = template.spec.as_ref().expect("pod spec");
        assert!(!pod.automount_service_account_token.unwrap_or(true));
        let probe = &pod.containers[0];
        assert_eq!(probe.image.as_deref(), Some(REACHABILITY_PROBE_IMAGE));
        let script = &probe.command.as_ref().expect("shell command")[2];
        assert!(script.contains("nc -z -w 2 mint 3338"));
        assert!(script.contains("\"reachable\":%s"));
        assert_eq!(action_result_container(&action.spec.action), "oracle");
    }

    #[test]
    fn reachability_oracle_refuses_unknown_services_and_unbounded_probes() {
        let (cell, mut action) = action_fixture();
        action.spec.capability = Capability::OracleRun;
        action.spec.action = CellAction::ReachabilityOracle(ReachabilityOracleAction {
            from_component: "wallet".into(),
            to_component: "mint".into(),
            service: "ssh".into(),
            timeout_seconds: 2,
            attempts: 3,
        });
        assert!(matches!(
            render_cell_action_job(&action, &cell),
            Err(ActionRenderError::UnknownService { .. })
        ));

        let CellAction::ReachabilityOracle(request) = &mut action.spec.action else {
            unreachable!()
        };
        request.service = "http".into();
        request.attempts = 6;
        assert!(matches!(
            render_cell_action_job(&action, &cell),
            Err(ActionRenderError::Bounds(_))
        ));
    }
    #[test]
    fn expired_chain_evidence_invalidates_dependent_actions_at_the_boundary() {
        let (mut cell, mut action) = action_fixture();
        for lightning in ["mint-lnd", "payer-lnd"] {
            cell.spec.cell.links.push(proofstorm_core::LinkSpec {
                id: format!("{lightning}-chain"),
                kind: proofstorm_core::LinkKind::ChainBackend,
                from: lightning.into(),
                to: "chain".into(),
                binding: Some(proofstorm_core::DependencyBinding::Chain {
                    network: proofstorm_core::BitcoinNetwork::Regtest,
                }),
            });
        }
        cell.spec.lock = resolve_lock(&cell.spec.cell, default_catalog()).unwrap();
        ready_admission_status(&mut cell);
        action.spec.action =
            CellAction::AuthenticationConformance(AuthenticationConformanceAction {
                mint: "mint".into(),
                identity_provider: "wallet".into(),
            });
        let chain = cell
            .status
            .as_mut()
            .unwrap()
            .components
            .iter_mut()
            .find(|status| status.id == "chain")
            .unwrap();
        chain.protocol_observation.as_mut().unwrap().expires_at_unix = 100;
        assert!(evaluate_action_admission_at(&action, &cell, 99).is_ok());
        assert!(matches!(
            evaluate_action_admission_at(&action, &cell, 100),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::Dependencies,
                state: Some(ComponentConditionState::Unknown),
                ..
            })
        ));
        let mut projected = cell.clone();
        crate::probes::expire_cell_status(&mut projected, 100);
        let lightning = projected
            .status
            .as_ref()
            .unwrap()
            .components
            .iter()
            .find(|status| status.id == "mint-lnd")
            .unwrap();
        assert!(!lightning.ready);
        assert!(
            lightning
                .protocol_observation
                .as_ref()
                .unwrap()
                .is_fresh(100),
            "dependency expiry is independent of this component's own probe"
        );
        assert!(
            cell.status
                .as_ref()
                .unwrap()
                .components
                .iter()
                .find(|status| status.id == "chain")
                .unwrap()
                .ready,
            "admission does not rewrite observed status"
        );
    }
}
