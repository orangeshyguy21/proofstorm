//! Leased task faults. Release is durable before policies are healed; cleanup
//! remains reconcilable after the task, workspace, or initiating client exits.
use super::{
    ACTION_CANCEL_ANNOTATION, Action, ActionPhase, Api, CellAction, Context, Duration, Error,
    ProofstormCell, ProofstormCellAction, ProofstormCellActionStatus, ResourceExt, now_unix,
    patch_action_status, status_object,
};
use kube::api::{Patch, PatchParams};
use serde_json::json;

pub(super) const EXPIRES_ANNOTATION: &str = "proofstorm.dev/workspace-fault-expires";
const RELEASED_ANNOTATION: &str = "proofstorm.dev/workspace-fault-released";

#[cfg(test)]
#[path = "workspace_faults_tests.rs"]
mod tests;

pub(super) fn is_owned_network(action: &ProofstormCellAction) -> bool {
    action
        .annotations()
        .contains_key(super::workspace_bridge::PARENT_ANNOTATION)
        && matches!(
            action.spec.action,
            CellAction::NetworkPartition(_) | CellAction::NetworkHeal(_)
        )
}

pub(super) fn needs_cleanup(action: &ProofstormCellAction) -> bool {
    is_owned_network(action)
        && matches!(action.spec.action, CellAction::NetworkPartition(_))
        && !action
            .status
            .as_ref()
            .and_then(|s| s.artifact.as_ref())
            .is_some_and(|artifact| artifact.get("cleanup_verified") == Some(&json!(true)))
}

pub(super) fn is_active(action: &ProofstormCellAction, now: i64) -> bool {
    matches!(action.spec.action, CellAction::NetworkPartition(_))
        && action
            .status
            .as_ref()
            .is_some_and(|s| matches!(s.phase, ActionPhase::Running | ActionPhase::Succeeded))
        && action
            .annotations()
            .get(EXPIRES_ANNOTATION)
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|expiry| expiry > now)
        && !action.annotations().contains_key(RELEASED_ANNOTATION)
        && !action.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
}

fn actions(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Api<ProofstormCellAction>, Error> {
    Ok(Api::namespaced(
        context.client.clone(),
        &action
            .namespace()
            .ok_or_else(|| Error::MissingNamespace(action.name_any()))?,
    ))
}

/// May be retried after any partial policy write. Does not depend on a live workspace.
async fn release(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    context: &Context,
    reason: &str,
) -> Result<(), Error> {
    let api = actions(action, context)?;
    let released = if action.annotations().contains_key(RELEASED_ANNOTATION) {
        action.clone()
    } else {
        api.patch(
            &action.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{RELEASED_ANNOTATION:reason}}})),
        )
        .await?
    };
    let active = super::apply_network_fault_policies(cell, None, context).await?;
    if active.contains_key(&action.spec.operation_id) {
        return Err(Error::ControllerInvariant(
            "released task fault remains active",
        ));
    }
    let mut status = released.status.clone().unwrap_or_default();
    if !super::is_terminal_action(status.phase) {
        status.phase = ActionPhase::Cancelled;
    }
    status.completed_at_unix = status.completed_at_unix.or(Some(now_unix()));
    status.artifact = Some(status_object(
        json!({"partition_operation_id":action.spec.operation_id,"healed":true,"cleanup_verified":true,"release_reason":released.annotations().get(RELEASED_ANNOTATION),"expires_at_unix":action.annotations().get(EXPIRES_ANNOTATION).and_then(|value| value.parse::<i64>().ok()),"active_partition_count":active.len()}),
    ));
    patch_action_status(&released, context, status).await
}

fn release_reason<'a>(
    action: &'a ProofstormCellAction,
    parent: Option<&ProofstormCellAction>,
    cell: &ProofstormCell,
    now: i64,
) -> Option<&'a str> {
    if let Some(reason) = action.annotations().get(RELEASED_ANNOTATION) {
        return Some(reason);
    }
    if action.annotations().contains_key(ACTION_CANCEL_ANNOTATION) {
        return Some("task_ended");
    }
    if action
        .annotations()
        .get(EXPIRES_ANNOTATION)
        .and_then(|s| s.parse::<i64>().ok())
        .is_none_or(|expiry| expiry <= now)
    {
        return Some("expired");
    }
    let Some(parent) = parent else {
        return Some("owner_missing");
    };
    if parent.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
        || action.annotations().get("proofstorm.dev/action-revision")
            != Some(&cell.spec.revision_digest)
        || proofstorm_kube::require_open_cell(cell).is_err()
    {
        return Some("authority_closed");
    }
    if action
        .status
        .as_ref()
        .is_some_and(|s| matches!(s.phase, ActionPhase::Failed | ActionPhase::Cancelled))
    {
        return Some("action_failed");
    }
    None
}

pub(super) async fn reconcile(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Action, Error> {
    let api = actions(action, context)?;
    // Refresh release/cancel markers before any activation or replay of a policy write.
    let action = api.get(&action.name_any()).await?;
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    let cells = Api::<ProofstormCell>::namespaced(context.client.clone(), &namespace);
    let Some(cell) = cells.get_opt(&action.spec.cell_name).await? else {
        return Ok(Action::await_change());
    };
    if cell.spec.instance_id != action.spec.instance_id
        || cell.spec.instance_key != action.spec.instance_key
        || cell.spec.workspace_id != action.spec.workspace_id
    {
        return super::patch_action_failure(
            &action,
            context,
            "workspace_fault_identity_mismatch",
            "fault does not belong to the current cell",
        )
        .await;
    }
    match &action.spec.action {
        CellAction::NetworkPartition(_) => partition(&action, &cell, context).await,
        CellAction::NetworkHeal(_) => heal(&action, &cell, context).await,
        _ => Err(Error::ControllerInvariant("task network action expected")),
    }
}

async fn partition(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    context: &Context,
) -> Result<Action, Error> {
    if !needs_cleanup(action) {
        return Ok(Action::await_change());
    }
    let api = actions(action, context)?;
    let parent = api
        .get_opt(&action.annotations()[super::workspace_bridge::PARENT_ANNOTATION])
        .await?;
    if let Some(reason) = release_reason(action, parent.as_ref(), cell, now_unix()) {
        release(action, cell, context, reason).await?;
        return Ok(Action::await_change());
    }
    if action
        .status
        .as_ref()
        .is_some_and(|status| status.phase == ActionPhase::Succeeded)
    {
        return Ok(Action::requeue(Duration::from_secs(2)));
    }
    if !super::workspace_bridge::admit_child(action, cell, context).await?
        || super::evaluate_action_admission(action, cell).is_err()
    {
        release(action, cell, context, "admission_closed").await?;
        return Ok(Action::await_change());
    }
    if action.status.is_none() {
        patch_action_status(
            action,
            context,
            ProofstormCellActionStatus {
                phase: ActionPhase::Running,
                started_at_unix: Some(now_unix()),
                ..Default::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    let active = super::apply_network_fault_policies(cell, None, context).await?;
    if !active.contains_key(&action.spec.operation_id) {
        release(action, cell, context, "activation_closed").await?;
        return Ok(Action::await_change());
    }
    patch_action_status(action, context, ProofstormCellActionStatus {
        phase:ActionPhase::Succeeded, started_at_unix:action.status.as_ref().and_then(|s| s.started_at_unix), completed_at_unix:Some(now_unix()),
        artifact:Some(status_object(json!({"partition_operation_id":action.spec.operation_id,"partitioned":true,"expires_at_unix":action.annotations().get(EXPIRES_ANNOTATION).and_then(|value| value.parse::<i64>().ok()),"cleanup_verified":false}))), ..Default::default()
    }).await?;
    Ok(Action::requeue(Duration::from_secs(2)))
}

async fn heal(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    context: &Context,
) -> Result<Action, Error> {
    if action
        .status
        .as_ref()
        .is_some_and(|status| super::is_terminal_action(status.phase))
    {
        return Ok(Action::await_change());
    }
    if action.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
        || !super::workspace_bridge::admit_child(action, cell, context).await?
    {
        return super::patch_action_failure(
            action,
            context,
            "workspace_control_closed",
            "task authority is no longer active",
        )
        .await;
    }
    let CellAction::NetworkHeal(request) = &action.spec.action else {
        unreachable!()
    };
    let Some(partition) = actions(action, context)?
        .get_opt(&request.partition_operation_id)
        .await?
    else {
        return super::patch_invalid_action(action, context, "partition call was not found").await;
    };
    if !is_owned_network(&partition)
        || !matches!(partition.spec.action, CellAction::NetworkPartition(_))
        || partition
            .annotations()
            .get(super::workspace_bridge::PARENT_ANNOTATION)
            != action
                .annotations()
                .get(super::workspace_bridge::PARENT_ANNOTATION)
        || partition.spec.instance_id != action.spec.instance_id
        || partition.spec.instance_key != action.spec.instance_key
        || partition.spec.workspace_id != action.spec.workspace_id
        || partition.spec.principal_id != action.spec.principal_id
    {
        return super::patch_invalid_action(
            action,
            context,
            "heal may release only this task's own partition",
        )
        .await;
    }
    release(&partition, cell, context, "explicit_heal").await?;
    patch_action_status(action, context, ProofstormCellActionStatus { phase:ActionPhase::Succeeded, completed_at_unix:Some(now_unix()), artifact:Some(status_object(json!({"partition_operation_id":partition.spec.operation_id,"healed":true,"cleanup_verified":true}))), ..Default::default() }).await?;
    Ok(Action::await_change())
}
