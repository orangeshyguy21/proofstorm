//! One lifecycle implementation for every component workload. Workload annotations
//! persist intent; optimistic concurrency fences both actions and cell rendering.
use super::{
    Action, ActionPhase, Api, BACKEND_ID_ANNOTATION, BTreeSet, COMPONENT_LABEL, CellAction,
    ComponentObservationResources, Context, Deployment, Duration,
    EXECUTION_STATE_CONTRACT_ANNOTATION, Error, LIFECYCLE_RESTART_ANNOTATION,
    LIFECYCLE_SEQUENCE_ANNOTATION, LIFECYCLE_STATE_ANNOTATION, ListParams, Patch, PatchParams, Pod,
    ProofstormCell, ProofstormCellAction, ProofstormCellActionStatus, ResourceExt, StatefulSet,
    WorkloadControllerKind, compile_component_plans, instance_namespace, now_unix,
    patch_action_failure, patch_action_status, patch_invalid_action, status_object,
};
use k8s_openapi::{api::core::v1::PodTemplateSpec, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Start,
    Stop,
    Restart,
}

pub(super) async fn reconcile(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    context: &Context,
) -> Result<Action, Error> {
    use proofstorm_core::{Capability, ComponentKind};
    let (request, control, capability) = match &action.spec.action {
        CellAction::NodeStart(r) => (r, Control::Start, Capability::NodeControl),
        CellAction::NodeStop(r) => (r, Control::Stop, Capability::NodeControl),
        CellAction::NodeRestart(r) => (r, Control::Restart, Capability::NodeControl),
        CellAction::ComponentStart(r) => (r, Control::Start, Capability::ComponentControl),
        CellAction::ComponentStop(r) => (r, Control::Stop, Capability::ComponentControl),
        CellAction::ComponentRestart(r) => (r, Control::Restart, Capability::ComponentControl),
        _ => {
            return Err(Error::ControllerInvariant(
                "expected component lifecycle action",
            ));
        }
    };
    if action.spec.capability != capability {
        return patch_invalid_action(action, context, "component lifecycle capability mismatch")
            .await;
    }
    let plans = compile_component_plans(
        &cell.spec.instance_key,
        &cell.spec.revision_digest,
        &cell.spec.cell,
        &cell.spec.lock,
    )?;
    let Some(plan) = plans.iter().find(|p| p.component_id == request.component) else {
        return patch_invalid_action(action, context, "component is not in the accepted cell")
            .await;
    };
    if capability == Capability::NodeControl
        && !matches!(plan.kind, ComponentKind::Bitcoin | ComponentKind::Lightning)
    {
        return patch_invalid_action(
            action,
            context,
            "node controls support only Bitcoin and Lightning",
        )
        .await;
    }
    if action.status.is_none() {
        patch_action_status(
            action,
            context,
            ProofstormCellActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                started_at_unix: Some(now_unix()),
                ..Default::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    let namespace = instance_namespace(&action.spec.instance_key);
    match plan.workload.kind {
        WorkloadControllerKind::StatefulSet => {
            reconcile_workload(
                &Api::<StatefulSet>::namespaced(context.client.clone(), &namespace),
                action,
                plan,
                control,
                context,
            )
            .await
        }
        WorkloadControllerKind::Deployment => {
            reconcile_workload(
                &Api::<Deployment>::namespaced(context.client.clone(), &namespace),
                action,
                plan,
                control,
                context,
            )
            .await
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "mutation and completion share the same observed workload and sequence fence"
)]
async fn reconcile_workload<K: LifecycleWorkload>(
    api: &Api<K>,
    action: &ProofstormCellAction,
    plan: &proofstorm_core::ComponentPlanContract,
    control: Control,
    context: &Context,
) -> Result<Action, Error> {
    let Some(workload) = api.get_opt(&plan.workload.name).await? else {
        return Ok(Action::requeue(Duration::from_secs(1)));
    };
    if !workload.matches_plan(plan) {
        return patch_action_failure(
            action,
            context,
            "stale_component_plan",
            "component workload does not match the accepted plan",
        )
        .await;
    }
    let sequence = annotation(workload.meta(), LIFECYCLE_SEQUENCE_ANNOTATION)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_default();
    if sequence > action.spec.sequence {
        return patch_action_failure(
            action,
            context,
            "lifecycle_action_superseded",
            "a newer lifecycle action controls this component",
        )
        .await;
    }
    let state = if control == Control::Stop {
        "stopped"
    } else {
        "running"
    };
    let token = format!("{}-{}", action.spec.sequence, action.spec.operation_id);
    if control == Control::Restart && (workload.replicas() == Some(0) || is_stopped(&workload)) {
        return patch_action_failure(
            action,
            context,
            "component_not_running",
            "a deliberately stopped component must be started before it can be restarted",
        )
        .await;
    }
    let restart = workload
        .template()
        .and_then(|t| t.metadata.as_ref())
        .and_then(|m| annotation(m, LIFECYCLE_RESTART_ANNOTATION));
    if sequence < action.spec.sequence
        || annotation(workload.meta(), LIFECYCLE_STATE_ANNOTATION) != Some(state)
        || (control == Control::Restart && restart != Some(token.as_str()))
    {
        let mut patch = serde_json::json!({
            "metadata": {"resourceVersion": workload.meta().resource_version, "annotations": {
                LIFECYCLE_STATE_ANNOTATION: state, LIFECYCLE_SEQUENCE_ANNOTATION: action.spec.sequence.to_string()
            }},
            "spec": {"replicas": if control == Control::Stop {0} else {i32::from(plan.workload.desired_replicas)}}
        });
        if control == Control::Restart {
            patch["spec"]["template"]["metadata"]["annotations"] =
                serde_json::json!({LIFECYCLE_RESTART_ANNOTATION: token});
        }
        match api
            .patch(
                &plan.workload.name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
        {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 409 => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    if !workload.converged(
        control == Control::Stop,
        i32::from(plan.workload.desired_replicas),
    ) {
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    // Replica counters may reach zero while a pod is still terminating.
    if control == Control::Stop {
        let namespace = instance_namespace(&action.spec.instance_key);
        let pods = Api::<Pod>::namespaced(context.client.clone(), &namespace)
            .list(
                &ListParams::default().labels(&format!("{COMPONENT_LABEL}={}", plan.component_id)),
            )
            .await?;
        if !pods.items.is_empty() {
            return Ok(Action::requeue(Duration::from_secs(1)));
        }
    }
    patch_action_status(action, context, ProofstormCellActionStatus {
        phase: ActionPhase::Succeeded, observed_generation: action.metadata.generation,
        started_at_unix: action.status.as_ref().and_then(|s| s.started_at_unix), completed_at_unix: Some(now_unix()),
        artifact: Some(status_object(serde_json::json!({"component": plan.component_id, "kind": plan.kind, "workload_kind": plan.workload.kind, "state": state, "restarted": control == Control::Restart, "sequence": action.spec.sequence}))),
        ..Default::default()
    }).await?;
    Ok(Action::await_change())
}

fn annotation<'a>(metadata: &'a ObjectMeta, key: &str) -> Option<&'a str> {
    metadata.annotations.as_ref()?.get(key).map(String::as_str)
}

pub(super) fn is_stopped<K: LifecycleWorkload>(workload: &K) -> bool {
    annotation(workload.meta(), LIFECYCLE_STATE_ANNOTATION) == Some("stopped")
}

/// Publish "intentionally stopped" only once shutdown has actually completed.
pub(super) fn observed_stops(
    plans: &[proofstorm_core::ComponentPlanContract],
    resources: &ComponentObservationResources<'_>,
) -> BTreeSet<String> {
    plans
        .iter()
        .filter(|plan| {
            !resources
                .pods
                .iter()
                .any(|pod| pod.labels().get(COMPONENT_LABEL) == Some(&plan.component_id))
                && match plan.workload.kind {
                    WorkloadControllerKind::StatefulSet => {
                        resources.stateful_sets.iter().any(|w| {
                            w.name_any() == plan.workload.name
                                && w.matches_plan(plan)
                                && is_stopped(w)
                                && w.converged(true, 0)
                        })
                    }
                    WorkloadControllerKind::Deployment => resources.deployments.iter().any(|w| {
                        w.name_any() == plan.workload.name
                            && w.matches_plan(plan)
                            && is_stopped(w)
                            && w.converged(true, 0)
                    }),
                }
        })
        .map(|plan| plan.component_id.clone())
        .collect()
}

pub(super) fn same_lifecycle_identity<K: LifecycleWorkload>(existing: &K, desired: &K) -> bool {
    [BACKEND_ID_ANNOTATION, EXECUTION_STATE_CONTRACT_ANNOTATION]
        .into_iter()
        .all(|key| {
            annotation(desired.meta(), key).is_some()
                && annotation(existing.meta(), key) == annotation(desired.meta(), key)
        })
}

pub(super) async fn preserve<K: LifecycleWorkload>(api: &Api<K>, desired: &K) -> Result<K, Error> {
    let Some(existing) = api.get_opt(&desired.name_any()).await? else {
        return Ok(desired.clone());
    };
    Ok(preserve_observed(&existing, desired))
}

fn preserve_observed<K: LifecycleWorkload>(existing: &K, desired: &K) -> K {
    let mut desired = desired.clone();
    // SSA must not restore running replicas from a stale read while an action stops it.
    desired
        .meta_mut()
        .resource_version
        .clone_from(&existing.meta().resource_version);
    let compatible = same_lifecycle_identity(existing, &desired);
    for key in [LIFECYCLE_STATE_ANNOTATION, LIFECYCLE_SEQUENCE_ANNOTATION] {
        if let Some(value) = annotation(existing.meta(), key) {
            // Explicitly replace old intent; omission would leave annotations owned
            // by the action controller on an incompatible replacement.
            let value = if !compatible && key == LIFECYCLE_STATE_ANNOTATION {
                "running"
            } else {
                value
            };
            desired
                .meta_mut()
                .annotations
                .get_or_insert_default()
                .insert(key.into(), value.into());
        }
    }
    if compatible && annotation(existing.meta(), LIFECYCLE_STATE_ANNOTATION).is_some() {
        desired.set_replicas(existing.replicas());
    }
    // Preserve restart markers even during unrelated cell edits, avoiding a second rollout.
    if let Some(token) = existing
        .template()
        .and_then(|t| t.metadata.as_ref())
        .and_then(|m| annotation(m, LIFECYCLE_RESTART_ANNOTATION))
        && let Some(template) = desired.template_mut()
    {
        let token = if compatible { token } else { "" };
        template
            .metadata
            .get_or_insert_default()
            .annotations
            .get_or_insert_default()
            .insert(LIFECYCLE_RESTART_ANNOTATION.into(), token.into());
    }
    desired
}

pub(super) trait LifecycleWorkload:
    kube::Resource<DynamicType = ()> + Clone + std::fmt::Debug + Serialize + DeserializeOwned
{
    fn replicas(&self) -> Option<i32>;
    fn set_replicas(&mut self, replicas: Option<i32>);
    fn template(&self) -> Option<&PodTemplateSpec>;
    fn template_mut(&mut self) -> Option<&mut PodTemplateSpec>;
    fn matches_plan(&self, plan: &proofstorm_core::ComponentPlanContract) -> bool {
        annotation(self.meta(), BACKEND_ID_ANNOTATION) == Some(plan.backend_id.as_str())
            && annotation(self.meta(), EXECUTION_STATE_CONTRACT_ANNOTATION)
                == Some(plan.execution_context.state_contract.as_str())
            && self
                .template()
                .and_then(|t| t.metadata.as_ref())
                .and_then(|m| annotation(m, proofstorm_kube::ROLLOUT_DIGEST_ANNOTATION))
                == Some(plan.rollout_digest.as_str())
    }
    fn converged(&self, stopped: bool, replicas: i32) -> bool;
}

fn generation_observed(metadata: &ObjectMeta, observed: Option<i64>) -> bool {
    metadata
        .generation
        .zip(observed)
        .is_some_and(|(desired, observed)| observed >= desired)
}

macro_rules! workload_fields {
    () => {
        fn replicas(&self) -> Option<i32> {
            self.spec.as_ref()?.replicas
        }
        fn set_replicas(&mut self, replicas: Option<i32>) {
            if let Some(spec) = self.spec.as_mut() {
                spec.replicas = replicas;
            }
        }
        fn template(&self) -> Option<&PodTemplateSpec> {
            Some(&self.spec.as_ref()?.template)
        }
        fn template_mut(&mut self) -> Option<&mut PodTemplateSpec> {
            Some(&mut self.spec.as_mut()?.template)
        }
    };
}
impl LifecycleWorkload for StatefulSet {
    workload_fields!();
    fn converged(&self, stopped: bool, replicas: i32) -> bool {
        let Some(status) = &self.status else {
            return false;
        };
        if !generation_observed(&self.metadata, status.observed_generation) {
            return false;
        }
        if stopped {
            return self.replicas() == Some(0) && status.replicas == 0;
        }
        self.replicas() == Some(replicas)
            && status.replicas == replicas
            && status.ready_replicas.unwrap_or_default() == replicas
            && status
                .current_revision
                .as_ref()
                .zip(status.update_revision.as_ref())
                .is_some_and(|(current, updated)| current == updated)
    }
}
impl LifecycleWorkload for Deployment {
    workload_fields!();
    fn converged(&self, stopped: bool, replicas: i32) -> bool {
        let Some(status) = &self.status else {
            return false;
        };
        if !generation_observed(&self.metadata, status.observed_generation) {
            return false;
        }
        if stopped {
            return self.replicas() == Some(0) && status.replicas.unwrap_or_default() == 0;
        }
        self.replicas() == Some(replicas)
            && status.replicas == Some(replicas)
            && status.ready_replicas == Some(replicas)
            && status.updated_replicas == Some(replicas)
            && status.unavailable_replicas.unwrap_or_default() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn deployment() -> Deployment {
        serde_json::from_value(json!({
            "metadata":{"name":"mint","resourceVersion":"42","generation":4,"annotations":{
                BACKEND_ID_ANNOTATION:"mint-backend",EXECUTION_STATE_CONTRACT_ANNOTATION:"data-v1",
                LIFECYCLE_STATE_ANNOTATION:"stopped",LIFECYCLE_SEQUENCE_ANNOTATION:"9"}},
            "spec":{"replicas":0,"selector":{"matchLabels":{"app":"mint"}},"template":{
                "metadata":{"annotations":{LIFECYCLE_RESTART_ANNOTATION:"8-restart"}},"spec":{"containers":[]}}},
            "status":{"observedGeneration":4,"replicas":0}
        })).unwrap()
    }
    fn stateful() -> StatefulSet {
        let mut value = serde_json::to_value(deployment()).unwrap();
        value["kind"] = json!("StatefulSet");
        serde_json::from_value(value).unwrap()
    }
    fn preserves_edit<K: LifecycleWorkload>(existing: &K) {
        let mut edit = existing.clone();
        edit.meta_mut().resource_version = None;
        edit.meta_mut()
            .annotations
            .as_mut()
            .unwrap()
            .remove(LIFECYCLE_STATE_ANNOTATION);
        edit.meta_mut()
            .annotations
            .as_mut()
            .unwrap()
            .remove(LIFECYCLE_SEQUENCE_ANNOTATION);
        edit.set_replicas(Some(1));
        edit.template_mut()
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .annotations = Some(BTreeMap::from([("new-config".into(), "v2".into())]));
        let preserved = preserve_observed(existing, &edit);
        assert_eq!(preserved.replicas(), Some(0));
        assert!(is_stopped(&preserved));
        assert_eq!(
            annotation(preserved.meta(), LIFECYCLE_SEQUENCE_ANNOTATION),
            Some("9")
        );
        assert_eq!(preserved.meta().resource_version.as_deref(), Some("42"));
        let annotations = preserved
            .template()
            .unwrap()
            .metadata
            .as_ref()
            .unwrap()
            .annotations
            .as_ref()
            .unwrap();
        assert_eq!(annotations[LIFECYCLE_RESTART_ANNOTATION], "8-restart");
        assert_eq!(annotations["new-config"], "v2");
        // Repeating after a controller restart leaves intent unchanged.
        assert_eq!(
            serde_json::to_value(preserve_observed(&preserved, &edit)).unwrap(),
            serde_json::to_value(&preserved).unwrap()
        );
        edit.meta_mut().annotations.as_mut().unwrap().insert(
            EXECUTION_STATE_CONTRACT_ANNOTATION.into(),
            "replacement".into(),
        );
        let replacement = preserve_observed(existing, &edit);
        assert_eq!(replacement.replicas(), Some(1));
        assert!(!is_stopped(&replacement));
        assert_eq!(
            annotation(replacement.meta(), LIFECYCLE_SEQUENCE_ANNOTATION),
            Some("9")
        );
        assert_eq!(
            replacement
                .template()
                .unwrap()
                .metadata
                .as_ref()
                .unwrap()
                .annotations
                .as_ref()
                .unwrap()[LIFECYCLE_RESTART_ANNOTATION],
            ""
        );
    }
    #[test]
    fn deliberate_stops_and_restart_markers_survive_edits_for_both_workloads() {
        preserves_edit(&deployment());
        preserves_edit(&stateful());
    }
    #[test]
    fn stops_need_observed_scale_down_not_missing_or_stale_status() {
        let mut deploy = deployment();
        let mut node = stateful();
        assert!(deploy.converged(true, 0));
        assert!(node.converged(true, 0));
        deploy.status.as_mut().unwrap().observed_generation = Some(3);
        node.status.as_mut().unwrap().observed_generation = Some(3);
        assert!(!deploy.converged(true, 0));
        assert!(!node.converged(true, 0));
        deploy.status = None;
        node.status = None;
        assert!(!deploy.converged(true, 0));
        assert!(!node.converged(true, 0));
    }
    #[test]
    fn starts_and_restarts_cannot_complete_using_old_ready_replicas() {
        let mut deploy = deployment();
        deploy.set_replicas(Some(1));
        deploy.status = Some(
            serde_json::from_value(
                json!({"observedGeneration":4,"replicas":1,"updatedReplicas":1,"readyReplicas":1}),
            )
            .unwrap(),
        );
        assert!(deploy.converged(false, 1));
        deploy.status.as_mut().unwrap().replicas = Some(2);
        assert!(!deploy.converged(false, 1));
        deploy.status.as_mut().unwrap().replicas = Some(1);
        deploy.status.as_mut().unwrap().observed_generation = Some(3);
        assert!(!deploy.converged(false, 1));
        let mut node = stateful();
        node.set_replicas(Some(1));
        node.status = Some(
            serde_json::from_value(json!({"observedGeneration":4,"replicas":1,"readyReplicas":1}))
                .unwrap(),
        );
        assert!(
            !node.converged(false, 1),
            "missing rollout revisions are not proof"
        );
        node.status.as_mut().unwrap().current_revision = Some("old".into());
        node.status.as_mut().unwrap().update_revision = Some("new".into());
        assert!(!node.converged(false, 1));
        node.status.as_mut().unwrap().current_revision = Some("new".into());
        assert!(node.converged(false, 1));
    }
}

#[cfg(test)]
#[path = "component_lifecycle_tests.rs"]
mod reconciliation_tests;
