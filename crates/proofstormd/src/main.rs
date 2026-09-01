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
        ConfigMap, LimitRange, Namespace, PersistentVolumeClaim, Pod, ResourceQuota, Service,
        ServiceAccount,
    },
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
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionPhase, ActionRenderError, AdapterError,
    LIFECYCLE_RESTART_ANNOTATION, LIFECYCLE_SEQUENCE_ANNOTATION, LIFECYCLE_STATE_ANNOTATION,
    LabAction, LabPhase, ProofstormLab, ProofstormLabAction, ProofstormLabActionStatus,
    ProofstormLabStatus, action_result_container, component_ports, instance_namespace,
    render_component_network_policy, render_lab, render_lab_action_cleanup_job,
    render_lab_action_job, render_security_spine,
};
use thiserror::Error;

const FINALIZER: &str = "proofstorm.dev/lab-cleanup";
const FIELD_MANAGER: &str = "proofstormd";

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
    #[error("cleanup pending while instance namespace {0} still exists")]
    CleanupPending(String),
    #[error("cleanup pending while runtime actions for instance {0} are being removed")]
    ActionCleanupPending(String),
    #[error("component adapter failed: {0}")]
    Adapter(#[from] AdapterError),
    #[error("action adapter failed: {0}")]
    Action(#[from] ActionRenderError),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::try_default().await?;
    let labs = Api::<ProofstormLab>::all(client.clone());
    let actions = Api::<ProofstormLabAction>::all(client.clone());
    let context = Arc::new(Context { client });
    let lab_controller = Controller::new(labs, watcher::Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, context.clone())
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormLab {object:?}"),
                Err(error) => eprintln!("reconciliation failed: {error}"),
            }
        });
    let action_controller = Controller::new(actions, watcher::Config::default())
        .shutdown_on_signal()
        .run(reconcile_action, action_error_policy, context)
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormLabAction {object:?}"),
                Err(error) => eprintln!("action reconciliation failed: {error}"),
            }
        });
    tokio::join!(lab_controller, action_controller);
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
    if lab.status.as_ref().map(|status| status.phase) != Some(LabPhase::Ready) {
        return Ok(Action::requeue(Duration::from_secs(2)));
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
    let component = lab
        .spec
        .lab
        .components
        .iter()
        .find(|component| component.id == request.component);
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

async fn patch_action_status(
    action: &ProofstormLabAction,
    context: &Context,
    status: ProofstormLabActionStatus,
) -> Result<(), Error> {
    if action.status.as_ref() == Some(&status) {
        return Ok(());
    }
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

#[allow(
    clippy::too_many_lines,
    reason = "one reconciliation pass visibly applies the complete bounded instance inventory"
)]
async fn apply(lab: Arc<ProofstormLab>, context: &Context) -> Result<Action, Error> {
    validate_instance_key(&lab.spec.instance_key)?;
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

    let workloads = render_lab(&lab.spec.instance_key, &lab.spec.lab, &lab.spec.lock)?;
    let client = context.client.clone();
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
        deployments
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
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

    let pods = Api::<Pod>::namespaced(client, &namespace_name)
        .list(&ListParams::default().labels(&format!(
            "proofstorm.dev/instance={}",
            lab.spec.instance_key
        )))
        .await?;
    let components = lab
        .spec
        .lab
        .components
        .iter()
        .map(|component| {
            let ready = pods.items.iter().any(|pod| {
                pod.metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get("proofstorm.dev/component"))
                    .is_some_and(|id| id == &component.id)
                    && pod
                        .status
                        .as_ref()
                        .and_then(|status| status.conditions.as_ref())
                        .is_some_and(|conditions| {
                            conditions.iter().any(|condition| {
                                condition.type_ == "Ready" && condition.status == "True"
                            })
                        })
            });
            proofstorm_core::ComponentStatus {
                id: component.id.clone(),
                kind: component.kind,
                ready,
                service: format!("{}.{}.svc", component.id, namespace_name),
                ports: component_ports(component),
            }
        })
        .collect::<Vec<_>>();
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
            components,
            inventory,
            inventory_digest: Some(inventory_digest),
            teardown_receipt: None,
            message: (!ready).then(|| "waiting for protocol component readiness".to_owned()),
        },
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(if ready {
        30
    } else {
        3
    })))
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

async fn cleanup(lab: Arc<ProofstormLab>, context: &Context) -> Result<Action, Error> {
    let instance_namespace = instance_namespace(&lab.spec.instance_key);
    patch_status(
        lab.as_ref(),
        context,
        ProofstormLabStatus {
            phase: LabPhase::Closing,
            instance_namespace: Some(instance_namespace.clone()),
            observed_generation: lab.metadata.generation,
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

fn error_policy(_lab: Arc<ProofstormLab>, error: &Error, _context: Arc<Context>) -> Action {
    eprintln!("retryable controller error: {error}");
    Action::requeue(Duration::from_secs(5))
}

fn action_error_policy(
    _action: Arc<ProofstormLabAction>,
    error: &Error,
    _context: Arc<Context>,
) -> Action {
    eprintln!("retryable action controller error: {error}");
    Action::requeue(Duration::from_secs(5))
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
