//! Controller-owned task calls. No listener, credentials or cluster route in the workspace.
#![allow(
    clippy::similar_names,
    reason = "cell and call are distinct domain terms"
)]
use super::{
    ACTION_CANCEL_ANNOTATION, Action, Api, CellAction, Context, Duration, Error, ListParams, Pod,
    ProofstormCell, ProofstormCellAction, ResourceExt, instance_namespace,
};
use kube::api::{Patch, PatchParams, PostParams};
use proofstorm_core::{
    Capability,
    workspace::{
        TaskStart, WORKSPACE_RUNNER,
        control::{
            BridgeRequest, ControlCall, ControlOperation, GRANT_ANNOTATION, MAX_BRIDGE_BYTES,
        },
    },
};
use proofstorm_kube::ComponentExecLiveAction;
use serde_json::{Value, json};

pub(super) const PARENT_ANNOTATION: &str = "proofstorm.dev/workspace-parent";
const PARENT_LABEL: &str = "proofstorm.dev/workspace-owner";
const CALL_ANNOTATION: &str = "proofstorm.dev/workspace-call";
const POLL_SECONDS: u64 = 2;
pub(super) const DEADLINE_ANNOTATION: &str = "proofstorm.dev/workspace-call-deadline";

#[cfg(test)]
#[path = "workspace_bridge_tests.rs"]
mod tests;

fn requested_start(action: &ProofstormCellAction) -> Option<TaskStart> {
    if action.annotations().contains_key(PARENT_ANNOTATION)
        || action.spec.access_scope.is_some()
        || action.spec.capability != Capability::ComponentExecLive
    {
        return None;
    }
    let CellAction::ComponentExecLive(request) = &action.spec.action else {
        return None;
    };
    let start = proofstorm_core::workspace::control::start_request(&request.script, &request.argv)?;
    let scope = start.control.as_ref()?;
    if !scope.capabilities().is_empty()
        && action.annotations().get(GRANT_ANNOTATION) != Some(&proofstorm_core::digest_json(scope))
    {
        return None;
    }

    Some(start)
}

pub(super) fn is_controlled_start(action: &ProofstormCellAction) -> bool {
    requested_start(action).is_some()
}

pub(super) fn controlled_start(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
) -> Option<TaskStart> {
    let start = requested_start(action)?;
    let CellAction::ComponentExecLive(request) = &action.spec.action else {
        return None;
    };
    if !same_instance(action, cell)
        || !cell
            .spec
            .cell
            .components
            .iter()
            .any(|c| c.id == request.component && c.implementation == "workspace")
        || start
            .control
            .as_ref()?
            .targets()
            .any(|id| !cell.spec.cell.components.iter().any(|c| c.id == id))
    {
        return None;
    }
    Some(start)
}

fn same_instance(action: &ProofstormCellAction, cell: &ProofstormCell) -> bool {
    action.spec.workspace_id == cell.spec.workspace_id
        && action.spec.instance_id == cell.spec.instance_id
        && action.spec.instance_key == cell.spec.instance_key
        && action.spec.cell_name == cell.name_any()
}

fn current_revision(parent: &ProofstormCellAction, cell: &ProofstormCell) -> bool {
    parent.annotations().get("proofstorm.dev/action-revision") == Some(&cell.spec.revision_digest)
        && proofstorm_kube::require_open_cell(cell).is_ok()
        && !parent.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
}

fn owner_label(parent: &ProofstormCellAction) -> String {
    proofstorm_core::digest_json(&(parent.metadata.uid.as_deref(), &parent.spec.operation_id))
        [7..47]
        .into()
}

fn call_name(parent: &ProofstormCellAction, call: &ControlCall) -> String {
    call_name_id(parent, &call.call_id)
}

pub(super) fn call_name_id(parent: &ProofstormCellAction, call_id: &str) -> String {
    format!(
        "ws-call-{}",
        &proofstorm_core::digest_json(&(owner_label(parent), call_id))[7..47]
    )
}

async fn exchange(
    parent: &ProofstormCellAction,
    request: &BridgeRequest,
    context: &Context,
) -> Result<Value, Error> {
    let reference = parent
        .status
        .as_ref()
        .and_then(|s| s.native_execution.as_ref())
        .ok_or(Error::ControllerInvariant(
            "workspace execution handle missing",
        ))?;
    let pods = Api::<Pod>::namespaced(
        context.client.clone(),
        &instance_namespace(&parent.spec.instance_key),
    );
    let original = pods.get_opt(&reference.pod).await?;
    let same_pod =
        original.as_ref().and_then(|pod| pod.metadata.uid.as_ref()) == Some(&reference.pod_uid);
    let pod = if same_pod {
        original.expect("observed original pod")
    } else {
        let CellAction::ComponentExecLive(start) = &parent.spec.action else {
            return Err(Error::ControllerInvariant("workspace start expected"));
        };
        pods.list(&ListParams::default().labels(&format!(
            "{}={},{}={}",
            super::INSTANCE_LABEL,
            parent.spec.instance_key,
            super::COMPONENT_LABEL,
            start.component
        )))
        .await?
        .items
        .into_iter()
        .find(|pod| {
            pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .as_ref()
                    .and_then(|status| status.phase.as_deref())
                    == Some("Running")
        })
        .ok_or_else(|| Error::LiveExec("workspace is unavailable".into()))?
    };
    let args = vec![
        WORKSPACE_RUNNER.into(),
        "workspace".into(),
        "bridge".into(),
        serde_json::to_string(request)
            .map_err(|_| Error::ControllerInvariant("bridge encoding failed"))?,
    ];
    let bytes = super::native_exec::exec_bounded(
        &pods,
        &pod.name_any(),
        &reference.container,
        args,
        None,
        usize::try_from(MAX_BRIDGE_BYTES).expect("bridge bound fits usize"),
    )
    .await?;
    let mut reply: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::LiveExec("invalid workspace bridge response".into()))?;
    if reply.get("error").is_some() {
        return Err(Error::LiveExec("workspace bridge request refused".into()));
    }
    if matches!(request, BridgeRequest::Poll { .. }) {
        reply["workspace_replaced"] = json!(!same_pod);
    }
    Ok(reply)
}

fn poll_request(parent: &ProofstormCellAction, start: &TaskStart) -> BridgeRequest {
    BridgeRequest::Poll {
        task_id: start.task_id.clone(),
        owner: parent.spec.operation_id.clone(),
    }
}

fn owns_state(parent: &ProofstormCellAction, start: &TaskStart, poll: &Value) -> bool {
    poll["state"]["control_owner"] == parent.spec.operation_id
        && poll["state"]["request_digest"] == proofstorm_core::digest_json(start)
}

fn child_action(parent: &ProofstormCellAction, call: &ControlCall) -> ProofstormCellAction {
    child_action_at(parent, call, super::now_unix())
}

fn child_action_at(
    parent: &ProofstormCellAction,
    call: &ControlCall,
    accepted: i64,
) -> ProofstormCellAction {
    let mut spec = parent.spec.clone();
    spec.operation_id = call_name(parent, call);
    spec.request_digest = proofstorm_core::digest_json(call);
    spec.accepted_at_unix = accepted;
    // These calls are cell-owned, independent of finite experiment completion.
    spec.experiment_id.clear();
    spec.session_id.clear();
    spec.action = if let Some(operation) = &call.operation {
        use proofstorm_kube::{ComponentControlAction, NetworkHealAction, NetworkPartitionAction};
        match operation {
            ControlOperation::ComponentStart { component } => {
                spec.capability = Capability::ComponentControl;
                CellAction::ComponentStart(ComponentControlAction {
                    component: component.clone(),
                })
            }
            ControlOperation::ComponentStop { component } => {
                spec.capability = Capability::ComponentControl;
                CellAction::ComponentStop(ComponentControlAction {
                    component: component.clone(),
                })
            }
            ControlOperation::ComponentRestart { component } => {
                spec.capability = Capability::ComponentControl;
                CellAction::ComponentRestart(ComponentControlAction {
                    component: component.clone(),
                })
            }
            ControlOperation::NetworkPartition {
                from_component,
                to_component,
                ..
            } => {
                spec.capability = Capability::NetworkPartition;
                CellAction::NetworkPartition(NetworkPartitionAction {
                    from_component: from_component.clone(),
                    to_component: to_component.clone(),
                })
            }
            ControlOperation::NetworkHeal { partition_call_id } => {
                spec.capability = Capability::NetworkHeal;
                CellAction::NetworkHeal(NetworkHealAction {
                    partition_operation_id: call_name_id(parent, partition_call_id),
                })
            }
        }
    } else {
        let command = call.command.as_ref().expect("validated native call");
        CellAction::ComponentExecLive(ComponentExecLiveAction {
            component: call.component.clone(),
            script: command.script.clone(),
            argv: command.argv.clone(),
            timeout_seconds: command.timeout_seconds,
            output: command.output.clone(),
            private_payload: None,
        })
    };
    let mut child = ProofstormCellAction::new(&spec.operation_id.clone(), spec);
    child
        .metadata
        .namespace
        .clone_from(&parent.metadata.namespace);
    child.metadata.labels.clone_from(&parent.metadata.labels);
    child
        .labels_mut()
        .insert(PARENT_LABEL.into(), owner_label(parent));
    child
        .annotations_mut()
        .insert(PARENT_ANNOTATION.into(), parent.name_any());
    child.annotations_mut().insert(
        CALL_ANNOTATION.into(),
        serde_json::to_string(call).expect("serializable call"),
    );
    if call.operation.is_some() {
        let seconds = requested_start(parent)
            .and_then(|start| start.control)
            .map_or(30, |scope| scope.max_timeout_seconds);
        child.annotations_mut().insert(
            DEADLINE_ANNOTATION.into(),
            (accepted + i64::from(seconds)).to_string(),
        );
    }
    if let Some(ControlOperation::NetworkPartition {
        duration_seconds, ..
    }) = call.operation
    {
        child.annotations_mut().insert(
            super::workspace_faults::EXPIRES_ANNOTATION.into(),
            (accepted + i64::from(duration_seconds)).to_string(),
        );
    }
    if let Some(revision) = parent.annotations().get("proofstorm.dev/action-revision") {
        child
            .annotations_mut()
            .insert("proofstorm.dev/action-revision".into(), revision.clone());
    }
    child
}

fn child_matches(
    parent: &ProofstormCellAction,
    call: &ControlCall,
    child: &ProofstormCellAction,
) -> bool {
    let expected = child_action_at(parent, call, child.spec.accepted_at_unix);
    child.spec == expected.spec
        && child.name_any() == expected.name_any()
        && expected
            .annotations()
            .iter()
            .all(|(key, value)| child.annotations().get(key) == Some(value))
        && expected
            .labels()
            .iter()
            .all(|(key, value)| child.labels().get(key) == Some(value))
}

/// Check again at the point of execution; a queued command cannot outlive its task grant.
pub(super) async fn admit_child(
    child: &ProofstormCellAction,
    cell: &ProofstormCell,
    context: &Context,
) -> Result<bool, Error> {
    let namespace = child
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(child.name_any()))?;
    let Some(parent_name) = child.annotations().get(PARENT_ANNOTATION) else {
        return Ok(false);
    };
    let parents = Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace);
    let Some(parent) = parents.get_opt(parent_name).await? else {
        return Ok(false);
    };
    let Some(start) = controlled_start(&parent, cell) else {
        return Ok(false);
    };
    let Some(call) = child
        .annotations()
        .get(CALL_ANNOTATION)
        .and_then(|value| serde_json::from_str::<ControlCall>(value).ok())
    else {
        return Ok(false);
    };
    if !current_revision(&parent, cell)
        || start
            .control
            .as_ref()
            .is_none_or(|scope| scope.permits(&call).is_err())
        || !child_matches(&parent, &call, child)
        || child
            .annotations()
            .get(DEADLINE_ANNOTATION)
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|deadline| deadline <= super::now_unix())
    {
        return Ok(false);
    }
    let Ok(poll) = exchange(&parent, &poll_request(&parent, &start), context).await else {
        return Ok(false);
    };
    Ok(owns_state(&parent, &start, &poll)
        && poll["workspace_replaced"] != true
        && poll["state"]["phase"] == "running"
        && poll["pending"]["digest"] == proofstorm_core::digest_json(&call)
        && poll["pending"]["claimed"] == true)
}

async fn cancel_children(
    children: &[ProofstormCellAction],
    actions: &Api<ProofstormCellAction>,
) -> Result<bool, Error> {
    let mut pending = false;
    for child in children {
        if !super::workspace_faults::needs_cleanup(child)
            && child
                .status
                .as_ref()
                .is_some_and(|status| super::is_terminal_action(status.phase))
        {
            continue;
        }
        pending = true;
        actions.patch(&child.name_any(), &PatchParams::default(), &Patch::Merge(json!({"metadata":{"annotations":{ACTION_CANCEL_ANNOTATION:"workspace-task-ended"}}}))).await?;
    }
    Ok(pending)
}

fn completion(
    parent: &ProofstormCellAction,
    start: &TaskStart,
    call: &ControlCall,
    receipt: Value,
) -> BridgeRequest {
    BridgeRequest::Complete {
        task_id: start.task_id.clone(),
        owner: parent.spec.operation_id.clone(),
        call_id: call.call_id.clone(),
        receipt,
    }
}

pub(super) async fn reconcile(
    parent: &ProofstormCellAction,
    context: &Context,
) -> Result<Action, Error> {
    if parent
        .status
        .as_ref()
        .is_none_or(|status| status.native_execution.is_none())
    {
        return Ok(Action::await_change());
    }
    reconcile_with(parent, context, |request| async move {
        exchange(parent, &request, context).await
    })
    .await
}

#[allow(
    clippy::too_many_lines,
    reason = "keep the dispatch fence and recovery branches together"
)]
async fn reconcile_with<F, Fut>(
    parent: &ProofstormCellAction,
    context: &Context,
    mut bridge: F,
) -> Result<Action, Error>
where
    F: FnMut(BridgeRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Error>>,
{
    let namespace = parent
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(parent.name_any()))?;
    let actions = Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace);
    let children = actions
        .list(&ListParams::default().labels(&format!("{PARENT_LABEL}={}", owner_label(parent))))
        .await?
        .items;
    let cells = Api::<ProofstormCell>::namespaced(context.client.clone(), &namespace);
    let cell = cells.get_opt(&parent.spec.cell_name).await?;
    let start = cell
        .as_ref()
        .filter(|cell| same_instance(parent, cell))
        .and_then(|_| requested_start(parent));
    let Some(start) = start else {
        return Ok(if cancel_children(&children, &actions).await? {
            Action::requeue(Duration::from_secs(POLL_SECONDS))
        } else {
            Action::await_change()
        });
    };
    // A duplicate task start returns the original owner. It must never attach another grant.
    let poll = bridge(poll_request(parent, &start)).await;
    let valid = poll
        .as_ref()
        .is_ok_and(|poll| owns_state(parent, &start, poll));
    let active = valid
        && poll.as_ref().is_ok_and(|p| p["workspace_replaced"] != true)
        && poll
            .as_ref()
            .is_ok_and(|p| p["state"]["phase"] == "running")
        && cell
            .as_ref()
            .is_some_and(|cell| current_revision(parent, cell));
    if valid
        && (cell
            .as_ref()
            .is_some_and(|cell| !current_revision(parent, cell))
            || poll.as_ref().is_ok_and(|p| p["workspace_replaced"] == true))
    {
        let _ = bridge(BridgeRequest::Close {
            task_id: start.task_id.clone(),
            owner: parent.spec.operation_id.clone(),
        })
        .await;
    }
    if !active {
        cancel_children(&children, &actions).await?;
    }
    let pending_faults = children
        .iter()
        .filter(|child| super::workspace_faults::needs_cleanup(child))
        .count();
    if valid
        && start
            .control
            .as_ref()
            .is_some_and(|scope| !scope.network.is_empty())
    {
        bridge(BridgeRequest::Cleanup {
            task_id: start.task_id.clone(),
            owner: parent.spec.operation_id.clone(),
            pending_faults: u32::try_from(pending_faults).unwrap_or(u32::MAX),
        })
        .await?;
    }
    let Ok(poll) = poll else {
        // Transport loss closes new authority and cancels outstanding calls. Retry collection.
        return Ok(Action::requeue(Duration::from_secs(5)));
    };
    if !valid {
        return Ok(Action::await_change());
    }
    if poll["pending"].is_null() {
        // Keep observing a stop until cleanup can be reported against a terminal
        // task phase; task-state changes do not wake the Kubernetes watch.
        return Ok(
            if active || pending_faults > 0 || poll["state"]["phase"] == "stopping" {
                Action::requeue(Duration::from_secs(POLL_SECONDS))
            } else {
                Action::await_change()
            },
        );
    }
    let call: ControlCall = serde_json::from_value(poll["pending"]["call"].clone())
        .map_err(|_| Error::LiveExec("invalid task call".into()))?;
    let permitted = start
        .control
        .as_ref()
        .is_some_and(|scope| scope.permits(&call).is_ok())
        && poll["call_count"].as_u64().is_some_and(|count| {
            count <= u64::from(start.control.as_ref().map_or(0, |s| s.max_calls))
        });
    if !permitted {
        return Err(Error::LiveExec(
            "workspace call exceeds its controller grant".into(),
        ));
    }
    let name = call_name(parent, &call);
    if let Some(child) = actions.get_opt(&name).await? {
        if !child_matches(parent, &call, &child) {
            return Err(Error::ControllerInvariant(
                "workspace child identity mismatch",
            ));
        }
        if let Some(status) = child
            .status
            .as_ref()
            .filter(|s| super::is_terminal_action(s.phase))
        {
            bridge(completion(parent, &start, &call, json!({"call_id":call.call_id,"action_id":name,"phase":status.phase,"artifact":status.artifact,"error":status.error}))).await?;
        }
    } else if !active || poll["pending"]["claimed"] == true {
        bridge(completion(parent, &start, &call, json!({"call_id":call.call_id,"action_id":name,"phase":"Failed","error":{"code":if active {"dispatch_outcome_unknown"} else {"workspace_control_closed"},"message":"command was not replayed; reconcile effects before using a new call ID"}}))).await?;
    } else {
        let claim = bridge(BridgeRequest::Claim {
            task_id: start.task_id.clone(),
            owner: parent.spec.operation_id.clone(),
            call_id: call.call_id.clone(),
        })
        .await?;
        if claim["fresh"] == true && claim["digest"] == proofstorm_core::digest_json(&call) {
            // Only this transition may create. A crash between claim and create is uncertain, never replayed.
            actions
                .create(&PostParams::default(), &child_action(parent, &call))
                .await?;
        }
    }
    Ok(Action::requeue(Duration::from_secs(POLL_SECONDS)))
}
