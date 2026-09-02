use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    batch::v1::{Job, JobStatus},
    core::v1::{
        ConfigMap, LimitRange, Namespace, PersistentVolumeClaim, Pod, ResourceQuota, Secret,
        Service, ServiceAccount,
    },
    discovery::v1::EndpointSlice,
    networking::v1::NetworkPolicy,
    rbac::v1::{Role, RoleBinding},
};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, ListParams, LogParams, Patch, PatchParams, PropagationPolicy},
    runtime::{
        Controller,
        controller::Action,
        finalizer::{Event, finalizer},
        watcher,
    },
};
use proofstorm_core::{BackendContractRegistry, default_backend_registry};
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionAdmissionError, ActionPhase, ActionRenderError, AdapterError,
    BACKEND_ID_ANNOTATION, ComponentObservationResources, EXECUTION_STATE_CONTRACT_ANNOTATION,
    INSTANCE_LABEL, LIFECYCLE_RESTART_ANNOTATION, LIFECYCLE_SEQUENCE_ANNOTATION,
    LIFECYCLE_STATE_ANNOTATION, LabAction, LabPhase, MAX_PROTOCOL_PROBES_PER_LAB,
    PROTOCOL_PROBER_LABEL, PROTOCOL_PROBER_LEASE_ANNOTATION, PROTOCOL_PROBER_NAME, ProofstormLab,
    ProofstormLabAction, ProofstormLabActionStatus, ProofstormLabStatus, action_result_container,
    compile_component_plans, evaluate_action_admission, instance_namespace,
    observe_component_statuses, render_component_network_policy, render_lab,
    render_lab_action_cleanup_job, render_lab_action_job, render_security_spine,
    schedule_protocol_probers,
};
use thiserror::Error;

const FINALIZER: &str = "proofstorm.dev/lab-cleanup";
const FIELD_MANAGER: &str = "proofstormd";
const LAB_CONTROLLER_CONCURRENCY: u16 = 8;
const ACTION_CONTROLLER_CONCURRENCY: u16 = 16;
const MAX_LAB_STATUS_BYTES: usize = 256 * 1024;
const MAX_ACTION_STATUS_BYTES: usize = 64 * 1024;

#[derive(Clone)]
struct Context {
    client: Client,
}

#[derive(Debug, Error)]
enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("ProofstormLab {0} has no namespace")]
    MissingNamespace(String),
    #[error("invalid instance key {0:?}")]
    InvalidInstanceKey(String),
    #[error("controller invariant failed: {0}")]
    ControllerInvariant(&'static str),
    #[error("generated Secret contract failed: {0}")]
    SecretContract(String),
    #[error("cleanup pending while instance namespace {0} still exists")]
    CleanupPending(String),
    #[error("cleanup pending while runtime actions for instance {0} are being removed")]
    ActionCleanupPending(String),
    #[error("component adapter failed: {0}")]
    Adapter(#[from] AdapterError),
    #[error("action adapter failed: {0}")]
    Action(#[from] ActionRenderError),
    #[error("{kind} status is {actual} bytes; controller maximum is {maximum} bytes")]
    StatusBudgetExceeded {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::try_default().await?;
    let labs = Api::<ProofstormLab>::all(client.clone());
    let actions = Api::<ProofstormLabAction>::all(client.clone());
    let context = Arc::new(Context { client });
    let lab_controller = Controller::new(labs, watcher::Config::default())
        .with_config(
            kube::runtime::controller::Config::default().concurrency(LAB_CONTROLLER_CONCURRENCY),
        )
        .shutdown_on_signal()
        .run(reconcile, error_policy, context.clone())
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormLab {object:?}"),
                Err(error) => eprintln!("reconciliation failed: {error}"),
            }
        });
    let action_controller = Controller::new(actions, watcher::Config::default())
        .with_config(
            kube::runtime::controller::Config::default().concurrency(ACTION_CONTROLLER_CONCURRENCY),
        )
        .shutdown_on_signal()
        .run(reconcile_action, action_error_policy, context.clone())
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormLabAction {object:?}"),
                Err(error) => eprintln!("action reconciliation failed: {error}"),
            }
        });
    let probe_scheduler = run_protocol_probe_scheduler(context);
    tokio::join!(lab_controller, action_controller, probe_scheduler);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one reconciliation pass keeps typed admission, deterministic Job identity, and terminal observation visibly contiguous"
)]
async fn reconcile_action(
    action: Arc<ProofstormLabAction>,
    context: Arc<Context>,
) -> Result<Action, Error> {
    let control_namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    if action
        .status
        .as_ref()
        .is_some_and(|status| is_terminal_action(status.phase))
    {
        return Ok(Action::await_change());
    }
    if action.annotations().contains_key(ACTION_CANCEL_ANNOTATION) {
        return reconcile_action_cancellation(action.as_ref(), &context).await;
    }
    let labs = Api::<ProofstormLab>::namespaced(context.client.clone(), &control_namespace);
    let lab = labs.get(&action.spec.lab_name).await?;
    if let Err(error) = evaluate_action_admission(action.as_ref(), &lab) {
        patch_action_status(
            action.as_ref(),
            &context,
            ProofstormLabActionStatus {
                phase: ActionPhase::Failed,
                observed_generation: action.metadata.generation,
                completed_at_unix: Some(now_unix()),
                error: Some(action_admission_failure(&error)),
                ..ProofstormLabActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::await_change());
    }
    if matches!(
        action.spec.action,
        LabAction::NodeStart(_) | LabAction::NodeStop(_) | LabAction::NodeRestart(_)
    ) {
        return reconcile_node_lifecycle(action.as_ref(), &lab, &context).await;
    }
    if matches!(
        action.spec.action,
        LabAction::NetworkPartition(_) | LabAction::NetworkHeal(_)
    ) {
        return reconcile_network_fault(action.as_ref(), &lab, &context).await;
    }
    let job = match render_lab_action_job(action.as_ref(), &lab) {
        Ok(job) => job,
        Err(error) => {
            patch_action_status(
                action.as_ref(),
                &context,
                ProofstormLabActionStatus {
                    phase: ActionPhase::Failed,
                    observed_generation: action.metadata.generation,
                    completed_at_unix: Some(now_unix()),
                    error: Some(status_object(serde_json::json!({
                        "code": "invalid_action",
                        "message": error.to_string(),
                    }))),
                    ..ProofstormLabActionStatus::default()
                },
            )
            .await?;
            return Ok(Action::await_change());
        }
    };
    let instance_namespace = instance_namespace(&action.spec.instance_key);
    let jobs = Api::<Job>::namespaced(context.client.clone(), &instance_namespace);
    let name = action.name_any();
    let Some(observed) = jobs.get_opt(&name).await? else {
        if action_execution_started(action.status.as_ref()) {
            patch_action_status(
                action.as_ref(),
                &context,
                lost_action_job_status(action.as_ref(), name, now_unix()),
            )
            .await?;
            return Ok(Action::await_change());
        }
        jobs.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&job),
        )
        .await?;
        patch_action_status(
            action.as_ref(),
            &context,
            ProofstormLabActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix: Some(now_unix()),
                ..ProofstormLabActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(2)));
    };

    let job_status = observed.status.unwrap_or_default();
    let succeeded = job_status.succeeded.unwrap_or_default() > 0;
    let failed = job_status.failed.unwrap_or_default() > 0
        || job_status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Failed" && condition.status == "True")
        });
    if !succeeded && !failed {
        let started = action
            .status
            .as_ref()
            .and_then(|status| status.started_at_unix)
            .unwrap_or_else(now_unix);
        patch_action_status(
            action.as_ref(),
            &context,
            ProofstormLabActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix: Some(started),
                ..ProofstormLabActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(2)));
    }

    let pod_api = Api::<Pod>::namespaced(context.client.clone(), &instance_namespace);
    let pods = pod_api
        .list(&ListParams::default().labels(&format!("job-name={name}")))
        .await?;
    let started_at_unix = action
        .status
        .as_ref()
        .and_then(|status| status.started_at_unix);
    let status = if failed {
        ProofstormLabActionStatus {
            phase: ActionPhase::Failed,
            observed_generation: action.metadata.generation,
            job_name: Some(name),
            started_at_unix,
            completed_at_unix: Some(now_unix()),
            error: Some(status_object(action_failure(&job_status, &pods.items))),
            ..ProofstormLabActionStatus::default()
        }
    } else {
        let artifact = if matches!(action.spec.action, LabAction::NativeExec(_)) {
            native_exec_artifact(&pod_api, action.as_ref(), &pods.items).await?
        } else {
            pods.items
                .iter()
                .find_map(|pod| {
                    termination_message(pod, action_result_container(&action.spec.action))
                })
                .and_then(|message| serde_json::from_str(&message).ok())
        };
        match artifact {
            Some(artifact) => ProofstormLabActionStatus {
                phase: ActionPhase::Succeeded,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix,
                completed_at_unix: Some(now_unix()),
                artifact: Some(artifact),
                ..ProofstormLabActionStatus::default()
            },
            None => ProofstormLabActionStatus {
                phase: ActionPhase::Failed,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix,
                completed_at_unix: Some(now_unix()),
                error: Some(status_object(
                    serde_json::json!({"code": "terminal_artifact_missing"}),
                )),
                ..ProofstormLabActionStatus::default()
            },
        }
    };
    patch_action_status(action.as_ref(), &context, status).await?;
    Ok(Action::await_change())
}

#[allow(
    clippy::too_many_lines,
    reason = "node lifecycle reconciliation keeps sequence fencing, mutation, and readiness proof contiguous"
)]
async fn reconcile_node_lifecycle(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    context: &Context,
) -> Result<Action, Error> {
    if action.spec.capability != proofstorm_core::Capability::NodeControl {
        return patch_invalid_action(action, context, "node lifecycle requires node.control").await;
    }
    let (LabAction::NodeStart(request)
    | LabAction::NodeStop(request)
    | LabAction::NodeRestart(request)) = &action.spec.action
    else {
        return Err(Error::ControllerInvariant("expected node lifecycle action"));
    };
    let plans = compile_component_plans(
        &lab.spec.instance_key,
        &lab.spec.revision_digest,
        &lab.spec.lab,
        &lab.spec.lock,
    )?;
    let component = plans
        .iter()
        .find(|plan| plan.component_id == request.component);
    let Some(component) = component else {
        return patch_invalid_action(
            action,
            context,
            "node component is not in the immutable lab",
        )
        .await;
    };
    if !matches!(
        component.kind,
        proofstorm_core::ComponentKind::Bitcoin | proofstorm_core::ComponentKind::Lightning
    ) {
        return patch_invalid_action(
            action,
            context,
            "node lifecycle supports only Bitcoin and Lightning components",
        )
        .await;
    }
    if action.status.is_none() {
        patch_action_status(
            action,
            context,
            ProofstormLabActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                started_at_unix: Some(now_unix()),
                ..ProofstormLabActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let namespace = instance_namespace(&action.spec.instance_key);
    let workloads = Api::<StatefulSet>::namespaced(context.client.clone(), &namespace);
    let Some(workload) = workloads.get_opt(&request.component).await? else {
        return Ok(Action::requeue(Duration::from_secs(1)));
    };
    if !stateful_set_matches_plan(&workload, component) {
        return patch_action_failure(
            action,
            context,
            "stale_component_plan",
            "node workload does not match the accepted component plan",
        )
        .await;
    }
    let annotations = workload.metadata.annotations.as_ref();
    let observed_sequence = annotations
        .and_then(|values| values.get(LIFECYCLE_SEQUENCE_ANNOTATION))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    if observed_sequence > action.spec.sequence {
        return patch_action_failure(
            action,
            context,
            "lifecycle_action_superseded",
            "a newer lifecycle action already controls this component",
        )
        .await;
    }
    let desired_state = match action.spec.action {
        LabAction::NodeStop(_) => "stopped",
        LabAction::NodeStart(_) | LabAction::NodeRestart(_) => "running",
        _ => unreachable!(),
    };
    let restart_token = format!("{}-{}", action.spec.sequence, action.spec.operation_id);
    let current_state = annotations
        .and_then(|values| values.get(LIFECYCLE_STATE_ANNOTATION))
        .map_or("running", String::as_str);
    let current_restart = workload
        .spec
        .as_ref()
        .and_then(|spec| spec.template.metadata.as_ref())
        .and_then(|metadata| metadata.annotations.as_ref())
        .and_then(|values| values.get(LIFECYCLE_RESTART_ANNOTATION));
    if matches!(action.spec.action, LabAction::NodeRestart(_))
        && workload.spec.as_ref().and_then(|spec| spec.replicas) == Some(0)
    {
        return patch_action_failure(
            action,
            context,
            "node_not_running",
            "a stopped node must be started before it can be restarted",
        )
        .await;
    }
    let mutation_needed = observed_sequence < action.spec.sequence
        || current_state != desired_state
        || (matches!(action.spec.action, LabAction::NodeRestart(_))
            && current_restart != Some(&restart_token));
    if mutation_needed {
        let mut patch = serde_json::json!({
            "metadata": {"annotations": {
                LIFECYCLE_STATE_ANNOTATION: desired_state,
                LIFECYCLE_SEQUENCE_ANNOTATION: action.spec.sequence.to_string(),
            }},
            "spec": {"replicas": i32::from(desired_state != "stopped")}
        });
        if matches!(action.spec.action, LabAction::NodeRestart(_)) {
            patch["spec"]["template"]["metadata"]["annotations"] =
                serde_json::json!({LIFECYCLE_RESTART_ANNOTATION: restart_token});
        }
        workloads
            .patch(
                &request.component,
                &PatchParams::default(),
                &Patch::Merge(patch),
            )
            .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let status = workload.status.as_ref();
    let complete = match action.spec.action {
        LabAction::NodeStop(_) => status.map(|status| status.replicas).unwrap_or_default() == 0,
        LabAction::NodeStart(_) => {
            status
                .and_then(|status| status.ready_replicas)
                .unwrap_or_default()
                >= 1
        }
        LabAction::NodeRestart(_) => {
            let generation_observed = status
                .and_then(|status| status.observed_generation)
                .zip(workload.metadata.generation)
                .is_some_and(|(observed, desired)| observed >= desired);
            generation_observed
                && status
                    .and_then(|status| status.ready_replicas)
                    .unwrap_or_default()
                    >= 1
                && status.and_then(|status| status.current_revision.as_ref())
                    == status.and_then(|status| status.update_revision.as_ref())
        }
        _ => false,
    };
    if !complete {
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    patch_action_status(
        action,
        context,
        ProofstormLabActionStatus {
            phase: ActionPhase::Succeeded,
            observed_generation: action.metadata.generation,
            started_at_unix: action
                .status
                .as_ref()
                .and_then(|status| status.started_at_unix),
            completed_at_unix: Some(now_unix()),
            artifact: Some(status_object(serde_json::json!({
                "component": request.component,
                "kind": component.kind,
                "state": desired_state,
                "restarted": matches!(action.spec.action, LabAction::NodeRestart(_)),
                "sequence": action.spec.sequence,
            }))),
            ..ProofstormLabActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

#[allow(
    clippy::too_many_lines,
    reason = "network fault reconciliation keeps journal validation, policy application, and terminal proof contiguous"
)]
async fn reconcile_network_fault(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    context: &Context,
) -> Result<Action, Error> {
    let expected_capability = match action.spec.action {
        LabAction::NetworkPartition(_) => proofstorm_core::Capability::NetworkPartition,
        LabAction::NetworkHeal(_) => proofstorm_core::Capability::NetworkHeal,
        _ => return Err(Error::ControllerInvariant("expected network fault action")),
    };
    if action.spec.capability != expected_capability {
        return patch_invalid_action(action, context, "network fault capability mismatch").await;
    }
    if action.spec.lab_name != lab.name_any()
        || action.spec.workspace_id != lab.spec.workspace_id
        || action.spec.instance_id != lab.spec.instance_id
        || action.spec.instance_key != lab.spec.instance_key
    {
        return patch_invalid_action(action, context, "network fault identity mismatch").await;
    }

    let actions = list_instance_actions(action, context).await?;
    let (from_component, to_component, healed) = match &action.spec.action {
        LabAction::NetworkPartition(request) => {
            if request.from_component == request.to_component
                || !lab
                    .spec
                    .lab
                    .components
                    .iter()
                    .any(|component| component.id == request.from_component)
                || !lab
                    .spec
                    .lab
                    .components
                    .iter()
                    .any(|component| component.id == request.to_component)
            {
                return patch_invalid_action(
                    action,
                    context,
                    "partition endpoints must be distinct components in the immutable lab",
                )
                .await;
            }
            (
                request.from_component.clone(),
                request.to_component.clone(),
                false,
            )
        }
        LabAction::NetworkHeal(request) => {
            let partition = actions.iter().find(|candidate| {
                candidate.spec.sequence < action.spec.sequence
                    && candidate.spec.operation_id == request.partition_operation_id
                    && candidate
                        .status
                        .as_ref()
                        .is_some_and(|status| status.phase == ActionPhase::Succeeded)
                    && matches!(candidate.spec.action, LabAction::NetworkPartition(_))
            });
            let Some(partition) = partition else {
                return patch_invalid_action(
                    action,
                    context,
                    "heal requires a prior succeeded partition in this lab",
                )
                .await;
            };
            let LabAction::NetworkPartition(partition) = &partition.spec.action else {
                unreachable!();
            };
            (
                partition.from_component.clone(),
                partition.to_component.clone(),
                true,
            )
        }
        _ => unreachable!(),
    };

    if action.status.is_none() {
        patch_action_status(
            action,
            context,
            ProofstormLabActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                started_at_unix: Some(now_unix()),
                ..ProofstormLabActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let active =
        apply_network_fault_policies(lab, &actions, Some(action.spec.sequence), context).await?;
    let partition_operation_id = match &action.spec.action {
        LabAction::NetworkPartition(_) => &action.spec.operation_id,
        LabAction::NetworkHeal(request) => &request.partition_operation_id,
        _ => unreachable!(),
    };
    let expected_active = !healed;
    if active.contains_key(partition_operation_id) != expected_active {
        return patch_action_failure(
            action,
            context,
            "network_fault_state_conflict",
            "ordered network fault state does not match the requested transition",
        )
        .await;
    }
    patch_action_status(
        action,
        context,
        ProofstormLabActionStatus {
            phase: ActionPhase::Succeeded,
            observed_generation: action.metadata.generation,
            started_at_unix: action
                .status
                .as_ref()
                .and_then(|status| status.started_at_unix),
            completed_at_unix: Some(now_unix()),
            artifact: Some(status_object(serde_json::json!({
                "partition_operation_id": partition_operation_id,
                "from_component": from_component,
                "to_component": to_component,
                "partitioned": !healed,
                "healed": healed,
                "active_partition_count": active.len(),
                "sequence": action.spec.sequence,
            }))),
            ..ProofstormLabActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

async fn list_instance_actions(
    action: &ProofstormLabAction,
    context: &Context,
) -> Result<Vec<ProofstormLabAction>, Error> {
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    Ok(
        Api::<ProofstormLabAction>::namespaced(context.client.clone(), &namespace)
            .list(&ListParams::default().labels(&format!(
                "proofstorm.dev/instance={}",
                action.spec.instance_key
            )))
            .await?
            .items,
    )
}

fn active_network_partitions(
    actions: &[ProofstormLabAction],
    maximum_sequence: Option<u64>,
) -> BTreeMap<String, (String, String)> {
    let mut ordered = actions.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|action| action.spec.sequence);
    let mut active = BTreeMap::new();
    for action in ordered {
        if maximum_sequence.is_some_and(|maximum| action.spec.sequence > maximum)
            || action.status.as_ref().is_some_and(|status| {
                matches!(status.phase, ActionPhase::Failed | ActionPhase::Cancelled)
            })
        {
            continue;
        }
        match &action.spec.action {
            LabAction::NetworkPartition(request) => {
                active.insert(
                    action.spec.operation_id.clone(),
                    (request.from_component.clone(), request.to_component.clone()),
                );
            }
            LabAction::NetworkHeal(request) => {
                active.remove(&request.partition_operation_id);
            }
            _ => {}
        }
    }
    active
}

async fn apply_network_fault_policies(
    lab: &ProofstormLab,
    actions: &[ProofstormLabAction],
    maximum_sequence: Option<u64>,
    context: &Context,
) -> Result<BTreeMap<String, (String, String)>, Error> {
    let active = active_network_partitions(actions, maximum_sequence);
    let mut exclusions = lab
        .spec
        .lab
        .components
        .iter()
        .map(|component| (component.id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (from, to) in active.values() {
        if let Some(peers) = exclusions.get_mut(from) {
            peers.insert(to.clone());
        }
        if let Some(peers) = exclusions.get_mut(to) {
            peers.insert(from.clone());
        }
    }
    let namespace = instance_namespace(&lab.spec.instance_key);
    let policies = Api::<NetworkPolicy>::namespaced(context.client.clone(), &namespace);
    for (component, peers) in exclusions {
        let peers = peers.into_iter().collect::<Vec<_>>();
        let policy = render_component_network_policy(&lab.spec.instance_key, &component, &peers)
            .map_err(|_| Error::ControllerInvariant("network policy did not serialize"))?;
        policies
            .patch(
                &component,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&policy),
            )
            .await?;
    }
    Ok(active)
}

async fn patch_invalid_action(
    action: &ProofstormLabAction,
    context: &Context,
    message: &str,
) -> Result<Action, Error> {
    patch_action_failure(action, context, "invalid_action", message).await
}

async fn patch_action_failure(
    action: &ProofstormLabAction,
    context: &Context,
    code: &str,
    message: &str,
) -> Result<Action, Error> {
    patch_action_status(
        action,
        context,
        ProofstormLabActionStatus {
            phase: ActionPhase::Failed,
            observed_generation: action.metadata.generation,
            started_at_unix: action
                .status
                .as_ref()
                .and_then(|status| status.started_at_unix),
            completed_at_unix: Some(now_unix()),
            error: Some(status_object(
                serde_json::json!({"code": code, "message": message}),
            )),
            ..ProofstormLabActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

const fn is_terminal_action(phase: ActionPhase) -> bool {
    matches!(
        phase,
        ActionPhase::Succeeded | ActionPhase::Failed | ActionPhase::Cancelled
    )
}

fn action_execution_started(status: Option<&ProofstormLabActionStatus>) -> bool {
    status.is_some_and(|status| {
        status.phase == ActionPhase::Running
            || status.job_name.is_some()
            || status.started_at_unix.is_some()
    })
}

fn lost_action_job_status(
    action: &ProofstormLabAction,
    job_name: String,
    completed_at_unix: i64,
) -> ProofstormLabActionStatus {
    ProofstormLabActionStatus {
        phase: ActionPhase::Failed,
        observed_generation: action.metadata.generation,
        job_name: action
            .status
            .as_ref()
            .and_then(|status| status.job_name.clone())
            .or(Some(job_name)),
        started_at_unix: action
            .status
            .as_ref()
            .and_then(|status| status.started_at_unix),
        completed_at_unix: Some(completed_at_unix),
        error: Some(status_object(serde_json::json!({
            "code": "action_job_lost",
            "message": "controller-owned Job disappeared after execution began; automatic replay refused",
        }))),
        ..ProofstormLabActionStatus::default()
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "cancellation reconciliation keeps primary deletion, private cleanup, and terminal proof contiguous"
)]
async fn reconcile_action_cancellation(
    action: &ProofstormLabAction,
    context: &Context,
) -> Result<Action, Error> {
    if matches!(
        action.spec.action,
        LabAction::NodeStart(_)
            | LabAction::NodeStop(_)
            | LabAction::NodeRestart(_)
            | LabAction::NetworkPartition(_)
            | LabAction::NetworkHeal(_)
    ) && action_execution_started(action.status.as_ref())
    {
        return patch_action_failure(
            action,
            context,
            "direct_action_cancellation_inconclusive",
            "direct controller execution began before cancellation; inspect current state before issuing another action",
        )
        .await;
    }
    let instance_namespace = instance_namespace(&action.spec.instance_key);
    let jobs = Api::<Job>::namespaced(context.client.clone(), &instance_namespace);
    let name = action.name_any();
    if jobs.get_opt(&name).await?.is_some() {
        jobs.delete(
            &name,
            &DeleteParams {
                propagation_policy: Some(PropagationPolicy::Foreground),
                ..DeleteParams::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    let control_namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    let labs = Api::<ProofstormLab>::namespaced(context.client.clone(), &control_namespace);
    let lab = labs.get(&action.spec.lab_name).await?;
    if let Some(cleanup) = render_lab_action_cleanup_job(action, &lab)? {
        let cleanup_name = cleanup.name_any();
        let Some(observed) = jobs.get_opt(&cleanup_name).await? else {
            jobs.patch(
                &cleanup_name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&cleanup),
            )
            .await?;
            return Ok(Action::requeue(Duration::from_secs(1)));
        };
        let cleanup_status = observed.status.unwrap_or_default();
        if cleanup_status.failed.unwrap_or_default() > 0
            || cleanup_status
                .conditions
                .as_ref()
                .is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Failed" && condition.status == "True")
                })
        {
            patch_action_status(
                action,
                context,
                ProofstormLabActionStatus {
                    phase: ActionPhase::Failed,
                    observed_generation: action.metadata.generation,
                    job_name: Some(cleanup_name),
                    started_at_unix: action
                        .status
                        .as_ref()
                        .and_then(|status| status.started_at_unix),
                    completed_at_unix: Some(now_unix()),
                    error: Some(status_object(serde_json::json!({
                        "code": "cancellation_cleanup_failed",
                        "message": "private action state could not be proven absent",
                    }))),
                    ..ProofstormLabActionStatus::default()
                },
            )
            .await?;
            return Ok(Action::await_change());
        }
        if cleanup_status.succeeded.unwrap_or_default() == 0 {
            return Ok(Action::requeue(Duration::from_secs(1)));
        }
    }
    let started_at_unix = action
        .status
        .as_ref()
        .and_then(|status| status.started_at_unix);
    patch_action_status(
        action,
        context,
        ProofstormLabActionStatus {
            phase: ActionPhase::Cancelled,
            observed_generation: action.metadata.generation,
            job_name: action
                .status
                .as_ref()
                .and_then(|status| status.job_name.clone()),
            started_at_unix,
            completed_at_unix: Some(now_unix()),
            error: Some(status_object(
                serde_json::json!({"code": "action_cancelled"}),
            )),
            ..ProofstormLabActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

fn action_failure(status: &JobStatus, pods: &[Pod]) -> serde_json::Value {
    if status.conditions.as_ref().is_some_and(|conditions| {
        conditions.iter().any(|condition| {
            condition.status == "True" && condition.reason.as_deref() == Some("DeadlineExceeded")
        })
    }) {
        return serde_json::json!({"code": "action_deadline_exceeded"});
    }
    pods.iter()
        .find_map(container_failure)
        .unwrap_or_else(|| serde_json::json!({"code": "action_failed"}))
}

fn action_admission_failure(error: &ActionAdmissionError) -> BTreeMap<String, serde_json::Value> {
    let mut value = serde_json::json!({
        "code": error.code(),
        "message": error.to_string(),
    });
    if let ActionAdmissionError::PrerequisiteUnsatisfied {
        component,
        operation,
        prerequisite,
        condition,
        state,
        reason,
    } = error
    {
        value["component"] = serde_json::json!(component);
        value["operation"] = serde_json::json!(operation);
        value["prerequisite"] = serde_json::json!(prerequisite);
        if let Some(condition) = condition {
            value["condition"] = serde_json::json!(condition);
        }
        if let Some(state) = state {
            value["state"] = serde_json::json!(state);
        }
        if let Some(reason) = reason {
            value["reason"] = serde_json::json!(reason);
        }
    }
    status_object(value)
}

async fn patch_action_status(
    action: &ProofstormLabAction,
    context: &Context,
    status: ProofstormLabActionStatus,
) -> Result<(), Error> {
    if action.status.as_ref() == Some(&status) {
        return Ok(());
    }
    enforce_status_budget("ProofstormLabAction", &status, MAX_ACTION_STATUS_BYTES)?;
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    let actions = Api::<ProofstormLabAction>::namespaced(context.client.clone(), &namespace);
    let patch = serde_json::json!({
        "apiVersion": "proofstorm.dev/v1alpha1",
        "kind": "ProofstormLabAction",
        "status": status,
    });
    actions
        .patch_status(
            &action.name_any(),
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
    Ok(())
}

async fn reconcile(lab: Arc<ProofstormLab>, context: Arc<Context>) -> Result<Action, Error> {
    let namespace = lab
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(lab.name_any()))?;
    let labs = Api::<ProofstormLab>::namespaced(context.client.clone(), &namespace);
    finalizer(&labs, FINALIZER, lab, |event| async {
        match event {
            Event::Apply(lab) => apply(lab, &context).await,
            Event::Cleanup(lab) => cleanup(lab, &context).await,
        }
    })
    .await
    .map_err(|error| match error {
        kube::runtime::finalizer::Error::ApplyFailed(error)
        | kube::runtime::finalizer::Error::CleanupFailed(error) => error,
        kube::runtime::finalizer::Error::AddFinalizer(error)
        | kube::runtime::finalizer::Error::RemoveFinalizer(error) => Error::Kube(error),
        kube::runtime::finalizer::Error::UnnamedObject => {
            Error::ControllerInvariant("finalized object has no name")
        }
        kube::runtime::finalizer::Error::InvalidFinalizer => {
            Error::ControllerInvariant("invalid finalizer JSON patch")
        }
    })
}

async fn run_protocol_probe_scheduler(context: Arc<Context>) {
    loop {
        if let Err(error) = reconcile_protocol_probe_schedule(&context).await {
            eprintln!("protocol probe scheduler retryable error: {error}");
        }
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    eprintln!("protocol probe scheduler shutdown signal failed: {error}");
                }
                break;
            }
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn reconcile_protocol_probe_schedule(context: &Context) -> Result<(), Error> {
    let labs = Api::<ProofstormLab>::all(context.client.clone())
        .list(&ListParams::default())
        .await?;
    let backend_registry = default_backend_registry();
    let mut candidate_counts = BTreeMap::<String, usize>::new();
    for lab in &labs.items {
        if protocol_probe_candidate(lab, &backend_registry) {
            *candidate_counts
                .entry(lab.spec.instance_key.clone())
                .or_default() += 1;
        }
    }
    let candidates = candidate_counts
        .into_iter()
        .filter_map(|(instance_key, count)| (count == 1).then_some(instance_key));
    let schedule = schedule_protocol_probers(candidates, now_unix());
    for lab in &labs.items {
        let active = schedule
            .active_instance_keys
            .contains(&lab.spec.instance_key);
        if !active
            && (protocol_probe_candidate(lab, &backend_registry)
                || lab
                    .annotations()
                    .contains_key(PROTOCOL_PROBER_LEASE_ANNOTATION))
        {
            patch_lab_protocol_probe_lease(lab, "inactive", context).await?;
        }
    }
    let deployments = Api::<Deployment>::all(context.client.clone())
        .list(&ListParams::default().labels(&format!("{PROTOCOL_PROBER_LABEL}=true")))
        .await?;

    for deployment in &deployments.items {
        let instance_key = deployment
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(INSTANCE_LABEL));
        if !instance_key
            .is_some_and(|instance_key| schedule.active_instance_keys.contains(instance_key))
        {
            patch_protocol_prober_lease(deployment, 0, "inactive", context).await?;
        }
    }

    let observed_prober_pods = Api::<Pod>::all(context.client.clone())
        .list(&ListParams::default().labels(&format!("{PROTOCOL_PROBER_LABEL}=true")))
        .await?
        .items;
    if unscheduled_protocol_prober_exists(&observed_prober_pods, &schedule.active_instance_keys) {
        return Ok(());
    }

    for deployment in &deployments.items {
        let active = deployment
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(INSTANCE_LABEL))
            .is_some_and(|instance_key| schedule.active_instance_keys.contains(instance_key));
        if active {
            patch_protocol_prober_lease(deployment, 1, &schedule.lease_id, context).await?;
        }
    }
    for lab in &labs.items {
        if schedule
            .active_instance_keys
            .contains(&lab.spec.instance_key)
        {
            patch_lab_protocol_probe_lease(lab, &schedule.lease_id, context).await?;
        }
    }
    Ok(())
}

fn unscheduled_protocol_prober_exists(
    pods: &[Pod],
    active_instance_keys: &BTreeSet<String>,
) -> bool {
    pods.iter().any(|pod| {
        !pod.metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(INSTANCE_LABEL))
            .is_some_and(|instance_key| active_instance_keys.contains(instance_key))
    })
}

fn protocol_probe_candidate(
    lab: &ProofstormLab,
    backend_registry: &BackendContractRegistry,
) -> bool {
    if lab.metadata.deletion_timestamp.is_some()
        || lab
            .status
            .as_ref()
            .is_some_and(|status| status.phase == LabPhase::Closing)
    {
        return false;
    }
    let count = lab
        .spec
        .lab
        .components
        .iter()
        .try_fold(0_usize, |count, component| {
            let lock = lab
                .spec
                .lock
                .entries
                .iter()
                .find(|entry| entry.component_id == component.id)?;
            let backend = backend_registry.require(&lock.catalog_id).ok()?;
            (backend.kind == component.kind)
                .then_some(count + usize::from(backend.protocol_probe.is_some()))
        });
    count.is_some_and(|count| (1..=MAX_PROTOCOL_PROBES_PER_LAB).contains(&count))
}

async fn patch_protocol_prober_lease(
    deployment: &Deployment,
    replicas: i32,
    lease_id: &str,
    context: &Context,
) -> Result<(), Error> {
    let current_replicas = deployment.spec.as_ref().and_then(|spec| spec.replicas);
    let metadata_lease = deployment
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(PROTOCOL_PROBER_LEASE_ANNOTATION));
    let template_lease = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.template.metadata.as_ref())
        .and_then(|metadata| metadata.annotations.as_ref())
        .and_then(|annotations| annotations.get(PROTOCOL_PROBER_LEASE_ANNOTATION));
    if current_replicas == Some(replicas)
        && metadata_lease.is_some_and(|current| current == lease_id)
        && template_lease.is_some_and(|current| current == lease_id)
    {
        return Ok(());
    }
    let namespace = deployment
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(deployment.name_any()))?;
    let deployments = Api::<Deployment>::namespaced(context.client.clone(), &namespace);
    deployments
        .patch(
            &deployment.name_any(),
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "metadata": {
                    "annotations": {PROTOCOL_PROBER_LEASE_ANNOTATION: lease_id}
                },
                "spec": {
                    "replicas": replicas,
                    "template": {
                        "metadata": {
                            "annotations": {PROTOCOL_PROBER_LEASE_ANNOTATION: lease_id}
                        }
                    }
                }
            })),
        )
        .await?;
    Ok(())
}

async fn patch_lab_protocol_probe_lease(
    lab: &ProofstormLab,
    lease_id: &str,
    context: &Context,
) -> Result<(), Error> {
    if lab
        .annotations()
        .get(PROTOCOL_PROBER_LEASE_ANNOTATION)
        .is_some_and(|current| current == lease_id)
    {
        return Ok(());
    }
    let namespace = lab
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(lab.name_any()))?;
    Api::<ProofstormLab>::namespaced(context.client.clone(), &namespace)
        .patch(
            &lab.name_any(),
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "metadata": {
                    "annotations": {PROTOCOL_PROBER_LEASE_ANNOTATION: lease_id}
                }
            })),
        )
        .await?;
    Ok(())
}

async fn ensure_generated_postgres_secret(
    secrets: &Api<Secret>,
    template: &Secret,
    patch: &PatchParams,
) -> Result<(), Error> {
    let name = template.name_any();
    if let Some(existing) = secrets.get_opt(&name).await? {
        let data = existing.data.as_ref().ok_or_else(|| {
            Error::SecretContract(format!("Secret {name:?} has no generated data"))
        })?;
        for key in [
            "POSTGRES_USER",
            "POSTGRES_PASSWORD",
            "POSTGRES_DB",
            "database.toml",
        ] {
            if !data.contains_key(key) {
                return Err(Error::SecretContract(format!(
                    "Secret {name:?} is missing key {key:?}"
                )));
            }
        }
        return Ok(());
    }

    let template_data = template.string_data.as_ref().ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no stringData"))
    })?;
    let username = template_data.get("POSTGRES_USER").ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no POSTGRES_USER"))
    })?;
    let database = template_data.get("POSTGRES_DB").ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no POSTGRES_DB"))
    })?;
    let component = template
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("proofstorm.dev/component"))
        .ok_or_else(|| {
            Error::SecretContract(format!(
                "Secret template {name:?} has no component identity"
            ))
        })?;
    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy).map_err(|error| {
        Error::SecretContract(format!(
            "could not generate credentials for {name:?}: {error}"
        ))
    })?;
    let password = entropy
        .iter()
        .fold(String::with_capacity(64), |mut encoded, byte| {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
            encoded
        });
    let url = format!("postgresql://{username}:{password}@{component}:5432/{database}");
    let database_config = format!(
        "\n[database]\nengine = \"postgres\"\n\n[database.postgres]\nurl = {url:?}\ntls_mode = \"disable\"\nmax_connections = 20\nconnection_timeout_seconds = 10\n"
    );
    let mut desired = template.clone();
    desired.string_data.get_or_insert_default().extend([
        ("POSTGRES_PASSWORD".into(), password),
        ("database.toml".into(), database_config),
    ]);
    secrets.patch(&name, patch, &Patch::Apply(&desired)).await?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one reconciliation pass visibly applies the complete bounded instance inventory"
)]
async fn apply(lab: Arc<ProofstormLab>, context: &Context) -> Result<Action, Error> {
    validate_instance_key(&lab.spec.instance_key)?;
    let workloads = render_lab(
        &lab.spec.instance_key,
        &lab.spec.revision_digest,
        &lab.spec.lab,
        &lab.spec.lock,
    )?;
    let rendered = render_security_spine(&lab.spec.instance_key);
    let client = context.client.clone();
    let namespace_name = instance_namespace(&lab.spec.instance_key);
    let patch = PatchParams::apply(FIELD_MANAGER).force();

    Api::<Namespace>::all(client.clone())
        .patch(&namespace_name, &patch, &Patch::Apply(&rendered.namespace))
        .await?;
    Api::<ResourceQuota>::namespaced(client.clone(), &namespace_name)
        .patch(
            "proofstorm-instance-quota",
            &patch,
            &Patch::Apply(&rendered.quota),
        )
        .await?;
    Api::<LimitRange>::namespaced(client.clone(), &namespace_name)
        .patch(
            "proofstorm-container-limits",
            &patch,
            &Patch::Apply(&rendered.limits),
        )
        .await?;
    Api::<NetworkPolicy>::namespaced(client.clone(), &namespace_name)
        .patch(
            "default-deny-all",
            &patch,
            &Patch::Apply(&rendered.default_deny),
        )
        .await?;
    Api::<ServiceAccount>::namespaced(client.clone(), &namespace_name)
        .patch(
            "proofstorm-workload",
            &patch,
            &Patch::Apply(&rendered.service_account),
        )
        .await?;
    Api::<Role>::namespaced(client.clone(), &namespace_name)
        .patch("proofstorm-workload", &patch, &Patch::Apply(&rendered.role))
        .await?;
    Api::<RoleBinding>::namespaced(client, &namespace_name)
        .patch(
            "proofstorm-workload",
            &patch,
            &Patch::Apply(&rendered.role_binding),
        )
        .await?;

    let client = context.client.clone();
    let secrets = Api::<Secret>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.secrets {
        ensure_generated_postgres_secret(&secrets, resource, &patch).await?;
    }
    let configs = Api::<ConfigMap>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.config_maps {
        configs
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let services = Api::<Service>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.services {
        services
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let claims = Api::<PersistentVolumeClaim>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.persistent_volume_claims {
        claims
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let stateful_sets = Api::<StatefulSet>::namespaced(client.clone(), &namespace_name);
    let mut stopped_components = BTreeSet::new();
    for resource in &workloads.stateful_sets {
        let resource = preserve_lifecycle_state(&stateful_sets, resource).await?;
        if resource
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(LIFECYCLE_STATE_ANNOTATION))
            .is_some_and(|state| state == "stopped")
            && let Some(name) = resource.metadata.name.as_ref()
        {
            stopped_components.insert(name.clone());
        }
        stateful_sets
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(&resource),
            )
            .await?;
    }
    let deployments = Api::<Deployment>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.deployments {
        let name = resource.metadata.name.as_deref().unwrap_or_default();
        if name == PROTOCOL_PROBER_NAME && deployments.get_opt(name).await?.is_some() {
            let mut desired = resource.clone();
            if let Some(spec) = desired.spec.as_mut() {
                spec.replicas = None;
                if let Some(annotations) = spec
                    .template
                    .metadata
                    .as_mut()
                    .and_then(|metadata| metadata.annotations.as_mut())
                {
                    annotations.remove(PROTOCOL_PROBER_LEASE_ANNOTATION);
                }
            }
            if let Some(annotations) = desired.metadata.annotations.as_mut() {
                annotations.remove(PROTOCOL_PROBER_LEASE_ANNOTATION);
            }
            deployments
                .patch(name, &PatchParams::default(), &Patch::Merge(&desired))
                .await?;
        } else {
            deployments
                .patch(name, &patch, &Patch::Apply(resource))
                .await?;
        }
    }
    let policies = Api::<NetworkPolicy>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.network_policies {
        if lab
            .spec
            .lab
            .components
            .iter()
            .any(|component| resource.metadata.name.as_deref() == Some(&component.id))
        {
            continue;
        }
        policies
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let control_namespace = lab
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(lab.name_any()))?;
    let network_actions =
        Api::<ProofstormLabAction>::namespaced(context.client.clone(), &control_namespace)
            .list(&ListParams::default().labels(&format!(
                "proofstorm.dev/instance={}",
                lab.spec.instance_key
            )))
            .await?;
    apply_network_fault_policies(&lab, &network_actions.items, None, context).await?;

    let inventory = workloads.inventory();
    let inventory_digest = proofstorm_core::digest_json(&inventory);
    let inventory_name = format!("proofstorm-inventory-{}", lab.spec.instance_key);
    let inventory_resource = serde_json::json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": inventory_name, "namespace": namespace_name,
            "labels": {"proofstorm.dev/instance": lab.spec.instance_key, "proofstorm.dev/inventory": "true"}},
        "immutable": true,
        "data": {"inventory.json": serde_json::to_string(&inventory).expect("inventory serializes"),
            "inventoryDigest": inventory_digest}
    });
    configs
        .patch(&inventory_name, &patch, &Patch::Apply(inventory_resource))
        .await?;

    let instance_resources = ListParams::default().labels(&format!(
        "proofstorm.dev/instance={}",
        lab.spec.instance_key
    ));
    let observed_deployments = deployments.list(&instance_resources).await?;
    let observed_stateful_sets = stateful_sets.list(&instance_resources).await?;
    let observed_claims = claims.list(&instance_resources).await?;
    let observed_services = services.list(&instance_resources).await?;
    let observed_pods = Api::<Pod>::namespaced(client.clone(), &namespace_name)
        .list(&instance_resources)
        .await?;
    let endpoint_slices = Api::<EndpointSlice>::namespaced(client, &namespace_name)
        .list(&ListParams::default())
        .await?;
    let observed_resources = ComponentObservationResources {
        deployments: &observed_deployments.items,
        stateful_sets: &observed_stateful_sets.items,
        persistent_volume_claims: &observed_claims.items,
        services: &observed_services.items,
        endpoint_slices: &endpoint_slices.items,
        pods: &observed_pods.items,
    };
    let previous_components = lab
        .status
        .as_ref()
        .filter(|status| status.observed_revision_digest == lab.spec.revision_digest)
        .map_or(&[][..], |status| status.components.as_slice());
    let components = observe_component_statuses(
        &namespace_name,
        &workloads.plans,
        &observed_resources,
        previous_components,
        &stopped_components,
        now_unix(),
    );
    let ready = components
        .iter()
        .all(|component| component.ready || stopped_components.contains(&component.id));

    patch_status(
        lab.as_ref(),
        context,
        ProofstormLabStatus {
            phase: if ready {
                LabPhase::Ready
            } else {
                LabPhase::Pending
            },
            instance_namespace: Some(namespace_name),
            observed_generation: lab.metadata.generation,
            observed_revision_digest: lab.spec.revision_digest.clone(),
            observed_protocol_probe_lease: lab
                .annotations()
                .get(PROTOCOL_PROBER_LEASE_ANNOTATION)
                .cloned(),
            components,
            inventory,
            inventory_digest: Some(inventory_digest),
            teardown_receipt: None,
            message: (!ready).then(|| "waiting for protocol component readiness".to_owned()),
        },
    )
    .await?;
    Ok(if ready {
        jittered_requeue(&lab.spec.instance_key, 30, 10)
    } else {
        jittered_requeue(&lab.spec.instance_key, 3, 2)
    })
}

async fn preserve_lifecycle_state(
    workloads: &Api<StatefulSet>,
    desired: &StatefulSet,
) -> Result<StatefulSet, Error> {
    let mut desired = desired.clone();
    let Some(name) = desired.metadata.name.as_deref() else {
        return Ok(desired);
    };
    let Some(existing) = workloads.get_opt(name).await? else {
        return Ok(desired);
    };
    if !same_lifecycle_identity(&existing, &desired) {
        return Ok(desired);
    }
    for key in [LIFECYCLE_STATE_ANNOTATION, LIFECYCLE_SEQUENCE_ANNOTATION] {
        if let Some(value) = existing
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(key))
        {
            desired
                .metadata
                .annotations
                .get_or_insert_default()
                .insert(key.to_owned(), value.clone());
        }
    }
    if existing
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(LIFECYCLE_STATE_ANNOTATION))
        .is_some()
        && let (Some(existing_spec), Some(desired_spec)) =
            (existing.spec.as_ref(), desired.spec.as_mut())
    {
        desired_spec.replicas = existing_spec.replicas;
        if let Some(restart) = existing_spec
            .template
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.annotations.as_ref())
            .and_then(|annotations| annotations.get(LIFECYCLE_RESTART_ANNOTATION))
        {
            desired_spec
                .template
                .metadata
                .get_or_insert_default()
                .annotations
                .get_or_insert_default()
                .insert(LIFECYCLE_RESTART_ANNOTATION.to_owned(), restart.clone());
        }
    }
    Ok(desired)
}

fn same_lifecycle_identity(existing: &StatefulSet, desired: &StatefulSet) -> bool {
    let existing = existing.metadata.annotations.as_ref();
    let desired = desired.metadata.annotations.as_ref();
    [BACKEND_ID_ANNOTATION, EXECUTION_STATE_CONTRACT_ANNOTATION]
        .into_iter()
        .all(|key| {
            existing.and_then(|annotations| annotations.get(key))
                == desired.and_then(|annotations| annotations.get(key))
                && desired
                    .and_then(|annotations| annotations.get(key))
                    .is_some()
        })
}

fn stateful_set_matches_plan(
    workload: &StatefulSet,
    plan: &proofstorm_core::ComponentPlanContract,
) -> bool {
    let annotations = workload.metadata.annotations.as_ref();
    annotations.and_then(|values| values.get(BACKEND_ID_ANNOTATION)) == Some(&plan.backend_id)
        && annotations.and_then(|values| values.get(EXECUTION_STATE_CONTRACT_ANNOTATION))
            == Some(&plan.execution_context.state_contract)
        && workload
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.annotations.as_ref())
            .and_then(|values| values.get(proofstorm_kube::ROLLOUT_DIGEST_ANNOTATION))
            == Some(&plan.rollout_digest)
}

async fn cleanup(lab: Arc<ProofstormLab>, context: &Context) -> Result<Action, Error> {
    let instance_namespace = instance_namespace(&lab.spec.instance_key);
    patch_status(
        lab.as_ref(),
        context,
        ProofstormLabStatus {
            phase: LabPhase::Closing,
            instance_namespace: Some(instance_namespace.clone()),
            observed_generation: lab.metadata.generation,
            observed_revision_digest: lab.status.as_ref().map_or_else(String::new, |status| {
                status.observed_revision_digest.clone()
            }),
            observed_protocol_probe_lease: lab
                .status
                .as_ref()
                .and_then(|status| status.observed_protocol_probe_lease.clone()),
            components: lab
                .status
                .as_ref()
                .map_or_else(Vec::new, |status| status.components.clone()),
            inventory: lab
                .status
                .as_ref()
                .map_or_else(Vec::new, |status| status.inventory.clone()),
            inventory_digest: lab
                .status
                .as_ref()
                .and_then(|status| status.inventory_digest.clone()),
            teardown_receipt: None,
            message: Some("deleting instance namespace and verifying absence".to_owned()),
        },
    )
    .await?;

    let control_namespace = lab
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(lab.name_any()))?;
    let actions =
        Api::<ProofstormLabAction>::namespaced(context.client.clone(), &control_namespace);
    let runtime_actions = actions
        .list(&ListParams::default().labels(&format!(
            "proofstorm.dev/instance={}",
            lab.spec.instance_key
        )))
        .await?;
    if !runtime_actions.items.is_empty() {
        for action in runtime_actions.items {
            match actions
                .delete(&action.name_any(), &DeleteParams::default())
                .await
            {
                Ok(_) => {}
                Err(kube::Error::Api(error)) if error.code == 404 => {}
                Err(error) => return Err(error.into()),
            }
        }
        return Err(Error::ActionCleanupPending(lab.spec.instance_key.clone()));
    }

    let namespaces = Api::<Namespace>::all(context.client.clone());
    if namespaces.get_opt(&instance_namespace).await?.is_some() {
        match namespaces
            .delete(&instance_namespace, &DeleteParams::default())
            .await
        {
            Ok(_) => return Err(Error::CleanupPending(instance_namespace)),
            Err(kube::Error::Api(error)) if error.code == 404 => {}
            Err(error) => return Err(error.into()),
        }
    }

    write_teardown_receipt(lab.as_ref(), context, &instance_namespace).await?;
    Ok(Action::await_change())
}

async fn patch_status(
    lab: &ProofstormLab,
    context: &Context,
    status: ProofstormLabStatus,
) -> Result<(), Error> {
    if !lab_status_update_required(lab.status.as_ref(), &status) {
        return Ok(());
    }
    enforce_status_budget("ProofstormLab", &status, MAX_LAB_STATUS_BYTES)?;
    let namespace = lab
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(lab.name_any()))?;
    let labs = Api::<ProofstormLab>::namespaced(context.client.clone(), &namespace);
    let patch = serde_json::json!({"apiVersion": "proofstorm.dev/v1alpha1", "kind": "ProofstormLab", "status": status});
    labs.patch_status(
        &lab.name_any(),
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(())
}

fn lab_status_update_required(
    current: Option<&ProofstormLabStatus>,
    observed: &ProofstormLabStatus,
) -> bool {
    current != Some(observed)
}

fn enforce_status_budget<T: serde::Serialize>(
    kind: &'static str,
    status: &T,
    maximum: usize,
) -> Result<(), Error> {
    let actual = serde_json::to_vec(status)
        .map_err(|_| Error::ControllerInvariant("typed status did not serialize"))?
        .len();
    if actual <= maximum {
        Ok(())
    } else {
        Err(Error::StatusBudgetExceeded {
            kind,
            actual,
            maximum,
        })
    }
}

async fn write_teardown_receipt(
    lab: &ProofstormLab,
    context: &Context,
    instance_namespace: &str,
) -> Result<(), Error> {
    let namespace = lab
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(lab.name_any()))?;
    let receipts = Api::<ConfigMap>::namespaced(context.client.clone(), &namespace);
    let name = format!("proofstorm-teardown-{}", lab.spec.instance_key);
    let receipt = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "proofstorm.dev/receipt": "teardown",
                "proofstorm.dev/instance": lab.spec.instance_key
            }
        },
        "data": {
            "instanceId": lab.spec.instance_id,
            "labName": lab.name_any(),
            "revisionDigest": lab.spec.revision_digest,
            "lockDigest": lab.spec.lock.digest,
            "inventoryDigest": lab.status.as_ref().and_then(|status| status.inventory_digest.clone()).unwrap_or_default(),
            "instanceNamespace": instance_namespace,
            "verifiedAbsent": "true"
        }
    });
    receipts
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(receipt),
        )
        .await?;
    Ok(())
}

fn validate_instance_key(key: &str) -> Result<(), Error> {
    let valid = (12..=32).contains(&key.len())
        && key.starts_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && key.ends_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && key.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidInstanceKey(key.to_owned()))
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "kube runtime requires an owned Arc in the error-policy callback signature"
)]
fn error_policy(lab: Arc<ProofstormLab>, error: &Error, _context: Arc<Context>) -> Action {
    eprintln!("retryable controller error: {error}");
    jittered_requeue(&lab.spec.instance_key, 5, 4)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "kube runtime requires an owned Arc in the error-policy callback signature"
)]
fn action_error_policy(
    action: Arc<ProofstormLabAction>,
    error: &Error,
    _context: Arc<Context>,
) -> Action {
    eprintln!("retryable action controller error: {error}");
    jittered_requeue(&action.spec.operation_id, 5, 4)
}

fn jittered_requeue(identity: &str, base_seconds: u64, spread_seconds: u64) -> Action {
    Action::requeue(Duration::from_secs(
        base_seconds + deterministic_jitter_seconds(identity, spread_seconds),
    ))
}

fn deterministic_jitter_seconds(identity: &str, spread_seconds: u64) -> u64 {
    let digest = proofstorm_core::digest_json(&identity);
    u64::from_str_radix(&digest["sha256:".len().."sha256:".len() + 8], 16).unwrap_or_default()
        % (spread_seconds + 1)
}

fn termination_message(pod: &Pod, target: &str) -> Option<String> {
    pod.status
        .as_ref()?
        .container_statuses
        .as_ref()?
        .iter()
        .find(|status| status.name == target)?
        .state
        .as_ref()?
        .terminated
        .as_ref()?
        .message
        .clone()
}

async fn native_exec_artifact(
    pods: &Api<Pod>,
    action: &ProofstormLabAction,
    observed: &[Pod],
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, kube::Error> {
    const LOG_LIMIT_BYTES: i64 = 20 * 1024;
    const ARTIFACT_TARGET_BYTES: usize = 30 * 1024;

    let LabAction::NativeExec(request) = &action.spec.action else {
        return Ok(None);
    };
    let Some((pod, metadata)) = observed.iter().find_map(|pod| {
        termination_message(pod, "exec")
            .and_then(|message| serde_json::from_str::<serde_json::Value>(&message).ok())
            .map(|metadata| (pod, metadata))
    }) else {
        return Ok(None);
    };
    let mut output = pods
        .logs(
            &pod.name_any(),
            &LogParams {
                container: Some("exec".into()),
                limit_bytes: Some(LOG_LIMIT_BYTES),
                ..LogParams::default()
            },
        )
        .await?;
    let mut truncated = output.len() >= usize::try_from(LOG_LIMIT_BYTES).unwrap_or(usize::MAX);
    let exit_code = metadata
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1);

    loop {
        let artifact = status_object(serde_json::json!({
            "component": request.component,
            "target_component": request.target_component,
            "exit_code": exit_code,
            "combined_output": output,
            "output_truncated": truncated,
        }));
        if serde_json::to_vec(&artifact)
            .map_or(true, |encoded| encoded.len() <= ARTIFACT_TARGET_BYTES)
        {
            return Ok(Some(artifact));
        }
        truncated = true;
        let target = output.len().saturating_mul(3) / 4;
        let boundary = output
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= target)
            .last()
            .unwrap_or(0);
        output.truncate(boundary);
    }
}

fn container_failure(pod: &Pod) -> Option<serde_json::Value> {
    let status = pod.status.as_ref()?;
    status
        .init_container_statuses
        .iter()
        .chain(status.container_statuses.iter())
        .flatten()
        .find_map(|container| {
            let terminated = container.state.as_ref()?.terminated.as_ref()?;
            (terminated.exit_code != 0).then(|| {
                serde_json::json!({
                    "code": "container_failed",
                    "container": container.name,
                    "exit_code": terminated.exit_code,
                    "reason": terminated.reason,
                })
            })
        })
}

fn now_unix() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn status_object(
    value: serde_json::Value,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object.into_iter().collect(),
        value => std::collections::BTreeMap::from([("value".to_owned(), value)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_keys_are_bounded_dns_labels() {
        assert!(validate_instance_key("01abcxyz7890").is_ok());
        assert!(validate_instance_key("too-short").is_err());
        assert!(validate_instance_key("UPPERCASE-KEY-1").is_err());
        assert!(validate_instance_key("-invalid-start").is_err());
    }

    #[test]
    fn deadline_exceeded_has_a_stable_terminal_error() {
        let status = JobStatus {
            conditions: Some(vec![k8s_openapi::api::batch::v1::JobCondition {
                type_: "Failed".into(),
                status: "True".into(),
                reason: Some("DeadlineExceeded".into()),
                ..Default::default()
            }]),
            ..JobStatus::default()
        };
        assert_eq!(
            action_failure(&status, &[]),
            serde_json::json!({"code": "action_deadline_exceeded"})
        );
        assert!(is_terminal_action(ActionPhase::Cancelled));
    }

    #[test]
    fn execution_evidence_fences_missing_job_replay() {
        assert!(!action_execution_started(None));
        assert!(!action_execution_started(Some(
            &ProofstormLabActionStatus::default()
        )));

        let running = ProofstormLabActionStatus {
            phase: ActionPhase::Running,
            job_name: Some("action-1".into()),
            started_at_unix: Some(42),
            ..ProofstormLabActionStatus::default()
        };
        assert!(action_execution_started(Some(&running)));
    }

    #[test]
    fn controller_policy_is_bounded_deterministic_and_desynchronized() {
        assert_eq!(LAB_CONTROLLER_CONCURRENCY, 8);
        assert_eq!(ACTION_CONTROLLER_CONCURRENCY, 16);
        let first = deterministic_jitter_seconds("instance-one", 10);
        assert_eq!(first, deterministic_jitter_seconds("instance-one", 10));
        assert!(first <= 10);
        let offsets = (0..32)
            .map(|index| deterministic_jitter_seconds(&format!("instance-{index}"), 10))
            .collect::<BTreeSet<_>>();
        assert!(offsets.len() > 1, "lab requeues must not synchronize");
    }

    #[test]
    fn probe_rotation_drains_old_labs_before_activation() {
        let pod = |instance: &str| {
            let mut pod = Pod::default();
            pod.metadata.labels = Some(BTreeMap::from([
                (INSTANCE_LABEL.into(), instance.into()),
                (PROTOCOL_PROBER_LABEL.into(), "true".into()),
            ]));
            pod
        };
        let active = BTreeSet::from(["instance-new".to_owned()]);
        assert!(unscheduled_protocol_prober_exists(
            &[pod("instance-old")],
            &active
        ));
        assert!(!unscheduled_protocol_prober_exists(
            &[pod("instance-new")],
            &active
        ));
        assert!(unscheduled_protocol_prober_exists(
            &[Pod::default()],
            &active
        ));
    }

    #[test]
    fn lab_status_writes_are_semantic_and_budgeted_at_supported_scale() {
        use proofstorm_core::{
            ComponentCondition, ComponentConditionReason, ComponentConditionState,
            ComponentConditionType, ComponentKind, ComponentStatus, InventoryEntry,
        };

        let condition_types = [
            ComponentConditionType::WorkloadReady,
            ComponentConditionType::StorageReady,
            ComponentConditionType::CredentialsReady,
            ComponentConditionType::ServiceReady,
            ComponentConditionType::ProtocolReady,
            ComponentConditionType::DependenciesReady,
            ComponentConditionType::ComponentReady,
            ComponentConditionType::ExperimentControllable,
        ];
        let components = (0..64)
            .map(|index| ComponentStatus {
                id: format!("wallet-{index}"),
                kind: ComponentKind::Wallet,
                observed_revision_digest: format!("sha256:{:064x}", index + 1),
                observed_rollout_digest: format!("sha256:{:064x}", index + 65),
                conditions: condition_types
                    .iter()
                    .map(|condition_type| ComponentCondition {
                        condition_type: *condition_type,
                        state: ComponentConditionState::Unknown,
                        reason: ComponentConditionReason::NotObserved,
                        message: "bounded readiness observation awaiting controller evidence"
                            .into(),
                        last_transition_unix: 1,
                    })
                    .collect(),
                ready: false,
                service: format!("wallet-{index}.instance.svc"),
                ports: BTreeMap::from([("http".into(), 3_338)]),
            })
            .collect::<Vec<_>>();
        let inventory = (0..64)
            .flat_map(|index| {
                (0..6).map(move |resource| InventoryEntry {
                    api_version: "apps/v1".into(),
                    kind: "StatefulSet".into(),
                    namespace: "proofstorm-i0123456789012345678".into(),
                    name: format!("wallet-{index}-resource-{resource}"),
                })
            })
            .collect();
        let status = ProofstormLabStatus {
            phase: LabPhase::Pending,
            instance_namespace: Some("proofstorm-i0123456789012345678".into()),
            observed_generation: Some(1),
            observed_revision_digest: "sha256:revision".into(),
            observed_protocol_probe_lease: Some("lease-current".into()),
            components,
            inventory,
            inventory_digest: Some(format!("sha256:{}", "a".repeat(64))),
            message: Some("waiting for protocol component readiness".into()),
            ..ProofstormLabStatus::default()
        };

        enforce_status_budget("ProofstormLab", &status, MAX_LAB_STATUS_BYTES)
            .expect("maximum supported status remains within budget");
        assert!(!lab_status_update_required(Some(&status), &status));
        let mut changed = status.clone();
        changed.phase = LabPhase::Ready;
        assert!(lab_status_update_required(Some(&status), &changed));

        let oversized = "x".repeat(MAX_LAB_STATUS_BYTES);
        assert!(matches!(
            enforce_status_budget("ProofstormLab", &oversized, MAX_LAB_STATUS_BYTES),
            Err(Error::StatusBudgetExceeded { .. })
        ));
    }

    #[test]
    fn lifecycle_state_is_preserved_only_for_the_same_backend_state_identity() {
        let workload = |revision: &str, backend: &str, state_contract: &str| {
            serde_json::from_value::<StatefulSet>(serde_json::json!({
                "apiVersion": "apps/v1",
                "kind": "StatefulSet",
                "metadata": {
                    "name": "chain",
                    "annotations": {
                        BACKEND_ID_ANNOTATION: backend,
                        EXECUTION_STATE_CONTRACT_ANNOTATION: state_contract,
                        proofstorm_kube::REVISION_DIGEST_ANNOTATION: revision,
                    }
                },
                "spec": {
                    "selector": {"matchLabels": {"app": "chain"}},
                    "serviceName": "chain",
                    "template": {
                        "metadata": {"labels": {"app": "chain"}},
                        "spec": {"containers": [{"name": "component", "image": "example.invalid/image"}]}
                    }
                }
            }))
            .expect("stateful set")
        };
        let existing = workload("sha256:old", "bitcoin-core", "proofstorm/bitcoin-state/v1");
        let revised = workload("sha256:new", "bitcoin-core", "proofstorm/bitcoin-state/v1");
        assert!(same_lifecycle_identity(&existing, &revised));

        let replaced = workload("sha256:new", "bitcoin-core", "proofstorm/bitcoin-state/v2");
        assert!(!same_lifecycle_identity(&existing, &replaced));
        let backend_changed =
            workload("sha256:new", "other-bitcoin", "proofstorm/bitcoin-state/v1");
        assert!(!same_lifecycle_identity(&existing, &backend_changed));
    }

    fn network_action(
        sequence: u64,
        operation_id: &str,
        action: LabAction,
        phase: ActionPhase,
    ) -> ProofstormLabAction {
        let capability = match action {
            LabAction::NetworkPartition(_) => proofstorm_core::Capability::NetworkPartition,
            LabAction::NetworkHeal(_) => proofstorm_core::Capability::NetworkHeal,
            _ => panic!("network action"),
        };
        let mut resource = ProofstormLabAction::new(
            &format!("action-{sequence}"),
            proofstorm_kube::ProofstormLabActionSpec {
                lab_name: "lab-1".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                experiment_id: "experiment".into(),
                lease_id: "lease".into(),
                principal_id: "principal".into(),
                sequence,
                operation_id: operation_id.into(),
                request_digest: "sha256:request".into(),
                capability,
                accepted_at_unix: 1,
                action,
            },
        );
        resource.status = Some(ProofstormLabActionStatus {
            phase,
            ..ProofstormLabActionStatus::default()
        });
        resource
    }

    #[test]
    fn network_fault_state_is_ordered_composable_and_ignores_cancelled_actions() {
        let actions = vec![
            network_action(
                3,
                "partition-two",
                LabAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                    from_component: "mint".into(),
                    to_component: "wallet".into(),
                }),
                ActionPhase::Succeeded,
            ),
            network_action(
                1,
                "partition-one",
                LabAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                    from_component: "mint".into(),
                    to_component: "lightning".into(),
                }),
                ActionPhase::Succeeded,
            ),
            network_action(
                2,
                "cancelled-partition",
                LabAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                    from_component: "chain".into(),
                    to_component: "lightning".into(),
                }),
                ActionPhase::Cancelled,
            ),
            network_action(
                4,
                "heal-one",
                LabAction::NetworkHeal(proofstorm_kube::NetworkHealAction {
                    partition_operation_id: "partition-one".into(),
                }),
                ActionPhase::Succeeded,
            ),
        ];
        let before_heal = active_network_partitions(&actions, Some(3));
        assert!(before_heal.contains_key("partition-one"));
        assert!(before_heal.contains_key("partition-two"));
        assert!(!before_heal.contains_key("cancelled-partition"));
        let after_heal = active_network_partitions(&actions, None);
        assert!(!after_heal.contains_key("partition-one"));
        assert_eq!(
            after_heal.get("partition-two"),
            Some(&("mint".into(), "wallet".into()))
        );
    }
}
