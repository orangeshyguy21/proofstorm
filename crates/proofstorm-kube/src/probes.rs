//! Bind protocol checks to observed runtime identity, not merely desired revision labels.
use std::collections::{BTreeMap, BTreeSet};

use kube::ResourceExt;
use proofstorm_core::{
    ComponentConditionState, ComponentPlanContract, ProtocolObservation, ProtocolProbePlan,
};
use proofstorm_prober::{Outcome, Target, scheduler::ScheduledTarget};

use crate::{
    COMPONENT_LABEL, ComponentObservationResources, INSTANCE_LABEL, ROLLOUT_DIGEST_ANNOTATION,
    observation::ObservationIndex,
};

#[derive(Debug, Clone)]
pub struct ProbeObservation {
    pub rollout_digest: String,
    pub runtime_digest: String,
    pub worker_uid: String,
    pub outcome: Outcome,
    pub timing: ProtocolObservation,
}

/// Expire success on reads even when the controller has stopped.
pub fn expire_cell_status(cell: &mut crate::ProofstormCell, now_unix: i64) {
    let Some(status) = cell.status.as_mut() else {
        return;
    };
    if let Ok(plans) = crate::compile_component_plans(
        &cell.spec.instance_key,
        &cell.spec.revision_digest,
        &cell.spec.cell,
        &cell.spec.lock,
    ) {
        crate::expire_protocol_status(&plans, &mut status.components, now_unix);
        let by_id: BTreeMap<_, _> = status
            .components
            .iter()
            .map(|component| (component.id.as_str(), component))
            .collect();
        let complete = by_id.len() == status.components.len()
            && by_id.len() == plans.len()
            && plans.iter().all(|plan| {
                by_id
                    .get(plan.component_id.as_str())
                    .is_some_and(|component| {
                        component.observed_rollout_digest == plan.rollout_digest
                            && component.observed_revision_digest == cell.spec.revision_digest
                    })
            });
        if status.phase == crate::CellPhase::Ready && !complete {
            status.phase = crate::CellPhase::Pending;
            status.message = Some("waiting for current component observations".into());
        }
    } else {
        status.components.clear();
        if status.phase == crate::CellPhase::Ready {
            status.phase = crate::CellPhase::Pending;
        }
    }
    if status.phase == crate::CellPhase::Ready
        && status.components.iter().any(|component| {
            !component.ready
                && !component.conditions.iter().any(|condition| {
                    condition.reason
                        == proofstorm_core::ComponentConditionReason::IntentionallyStopped
                })
        })
    {
        status.phase = crate::CellPhase::Pending;
        status.message = Some("waiting for fresh protocol observations".into());
    }
}

/// Only check a current workload with ready endpoints belonging to its current Pods.
#[must_use]
pub fn eligible_target(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
) -> Option<ScheduledTarget> {
    eligible_target_indexed(plan, &ObservationIndex::new(resources))
}

/// Compute the cell's eligible checks with one shared resource index.
#[must_use]
pub fn eligible_targets(
    plans: &[ComponentPlanContract],
    resources: &ComponentObservationResources<'_>,
) -> Vec<ScheduledTarget> {
    let resources = ObservationIndex::new(resources);
    plans
        .iter()
        .filter_map(|plan| eligible_target_indexed(plan, &resources))
        .collect()
}

fn eligible_target_indexed(
    plan: &ComponentPlanContract,
    resources: &ObservationIndex<'_>,
) -> Option<ScheduledTarget> {
    let probe = plan.protocol_probe.as_ref()?;
    if crate::adapter::workload_observation(plan, resources).0 != ComponentConditionState::True
        || crate::adapter::service_observation(plan, resources).0 != ComponentConditionState::True
    {
        return None;
    }
    let namespace = crate::instance_namespace(&plan.instance_key);
    let pods: BTreeMap<_, _> = resources
        .pods
        .all(&plan.component_id)
        .filter(|pod| {
            pod.namespace().as_deref() == Some(&namespace)
                && pod.labels().get(INSTANCE_LABEL) == Some(&plan.instance_key)
                && pod.labels().get(COMPONENT_LABEL) == Some(&plan.component_id)
                && pod.annotations().get(ROLLOUT_DIGEST_ANNOTATION) == Some(&plan.rollout_digest)
                && pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .as_ref()
                    .and_then(|status| status.conditions.as_ref())
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition.type_ == "Ready" && condition.status == "True"
                        })
                    })
        })
        .filter_map(|pod| Some((pod.uid()?, container_incarnation(pod))))
        .collect();
    if pods.is_empty() {
        return None;
    }
    let service = resources
        .services
        .all(&plan.component_id)
        .find(|service| service.namespace().as_deref() == Some(&namespace))?;
    let service_uid = service.uid()?;
    let mut endpoints = BTreeSet::new();
    for slice in resources.endpoints.all(&plan.component_id) {
        if !slice
            .owner_references()
            .iter()
            .any(|owner| owner.kind == "Service" && owner.uid == service_uid)
        {
            // Until every advertised slice belongs to this Service incarnation,
            // DNS traffic may still be routed to a previous workload.
            return None;
        }
        for endpoint in &slice.endpoints {
            if endpoint
                .conditions
                .as_ref()
                .and_then(|conditions| conditions.ready)
                == Some(false)
            {
                continue;
            }
            let uid = endpoint
                .target_ref
                .as_ref()
                .and_then(|target| target.uid.as_ref())?;
            // A mixed old/new Service cannot supply proof about the accepted workload yet.
            let incarnation = pods.get(uid)?;
            for address in &endpoint.addresses {
                endpoints.insert((uid.clone(), address.clone(), incarnation.clone()));
            }
        }
    }
    if endpoints.is_empty() {
        return None;
    }
    let (port, http_path) = match probe {
        ProtocolProbePlan::Tcp { port } => (*port, None),
        ProtocolProbePlan::HttpGet { port, path } => (*port, Some(path.clone())),
    };
    Some(ScheduledTarget {
        probe: Target {
            component: plan.component_id.clone(),
            rollout_digest: plan.rollout_digest.clone(),
            port,
            http_path,
        },
        runtime_digest: proofstorm_core::digest_json(&(service_uid, endpoints)),
    })
}

pub(crate) fn current_observation<'a>(
    plan: &ComponentPlanContract,
    resources: &'a ObservationIndex<'_>,
    now_unix: i64,
) -> Option<&'a ProbeObservation> {
    let observation = resources.protocol.get(&plan.component_id)?;
    let current = eligible_target_indexed(plan, resources)?;
    (observation.rollout_digest == plan.rollout_digest
        && observation.runtime_digest == current.runtime_digest
        && observation.timing.is_fresh(now_unix)
        && resources
            .pods_by_uid
            .all(&observation.worker_uid)
            .any(|pod| {
                pod.metadata.deletion_timestamp.is_none()
                    && pod.namespace().as_deref()
                        == Some(&crate::instance_namespace(&plan.instance_key))
                    && pod.labels().get(INSTANCE_LABEL) == Some(&plan.instance_key)
                    && pod.status.as_ref().is_some_and(|status| {
                        status.phase.as_deref() == Some("Running")
                            && status.conditions.as_ref().is_some_and(|conditions| {
                                conditions.iter().any(|condition| {
                                    condition.type_ == "Ready" && condition.status == "True"
                                })
                            })
                    })
                    && pod
                        .labels()
                        .get(crate::PROTOCOL_PROBER_LABEL)
                        .is_some_and(|value| value == "true")
            }))
    .then_some(observation)
}

/// A container restart keeps its Pod UID but invalidates earlier reachability evidence.
#[must_use]
pub fn container_incarnation(pod: &k8s_openapi::api::core::v1::Pod) -> String {
    let containers: BTreeMap<_, _> = pod
        .status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref())
        .into_iter()
        .flatten()
        .map(|container| {
            (
                container.name.as_str(),
                (
                    container.restart_count,
                    container
                        .state
                        .as_ref()
                        .and_then(|state| state.running.as_ref())
                        .and_then(|running| running.started_at.as_ref()),
                ),
            )
        })
        .collect();
    proofstorm_core::digest_json(&containers)
}
