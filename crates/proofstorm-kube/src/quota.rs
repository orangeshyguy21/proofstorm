//! Refuse a cell whose own quota can't hold it.
//!
//! The namespace quota counts every pod there: each component's, the protocol
//! prober's and every running action's. Kubernetes refuses a pod over quota
//! without a scheduling event, so a too-small quota shows up only as a cell
//! that never gets ready. Planning checks it against the rendered cell first.
use k8s_openapi::api::core::v1::{PodSpec, ResourceRequirements};
use proofstorm_core::{CellSpec, ResolvedLock, quantity_value};

/// Rendering needs an instance key; the demand doesn't depend on it.
const PLANNING_KEY: &str = "i0000000000000000000";

/// Why `cell`'s quota can't hold it, or `None` when it can or sets none.
/// Every component's pod, the protocol prober and one action pod at the cell's
/// default requests must fit together.
#[must_use]
pub fn quota_shortfall(cell: &CellSpec, lock: &ResolvedLock) -> Option<String> {
    let quota = cell.policy.resources.quota.as_ref()?;
    // A cell that doesn't render is refused on its own terms elsewhere.
    let rendered = crate::render_cell(PLANNING_KEY, "planning", cell, lock).ok()?;
    let mut demand = Demand::default();
    for (replicas, pod) in rendered
        .deployments
        .iter()
        .filter_map(|deployment| deployment.spec.as_ref())
        .map(|spec| (spec.replicas, spec.template.spec.as_ref()))
        .chain(
            rendered
                .stateful_sets
                .iter()
                .filter_map(|set| set.spec.as_ref())
                .map(|spec| (spec.replicas, spec.template.spec.as_ref())),
        )
    {
        let replicas = u64::try_from(replicas.unwrap_or(1)).unwrap_or_default();
        demand.add_pod(pod, replicas);
    }
    demand.volumes += u64::try_from(rendered.persistent_volume_claims.len()).unwrap_or_default();
    for spec in rendered
        .stateful_sets
        .iter()
        .filter_map(|set| set.spec.as_ref())
    {
        let templates = spec.volume_claim_templates.as_ref().map_or(0, Vec::len);
        let replicas = usize::try_from(spec.replicas.unwrap_or(1)).unwrap_or_default();
        demand.volumes += u64::try_from(templates * replicas).unwrap_or_default();
    }
    if rendered
        .plans
        .iter()
        .any(|plan| plan.protocol_probe.is_some())
    {
        demand.add_requests(Some(&crate::protocol_prober_resources()));
        demand.pods += 1;
    }
    let defaults = cell.policy.resources.container_defaults();
    demand.cpu += defaults
        .cpu_request
        .as_deref()
        .and_then(quantity_value)
        .unwrap_or_default();
    demand.memory += defaults
        .memory_request
        .as_deref()
        .and_then(quantity_value)
        .unwrap_or_default();
    demand.pods += 1;

    let mut short = Vec::new();
    for (name, hard, needed) in [
        ("pods", quota.pods, demand.pods),
        ("volumes", quota.volumes, demand.volumes),
    ] {
        if let Some(hard) = hard
            && hard < needed
        {
            short.push(format!("{name} {hard} < {needed}"));
        }
    }
    let millicores = (demand.cpu * 1000.0).ceil();
    let mebibytes = (demand.memory / 1_048_576.0).ceil();
    for (name, hard, needed, shown) in [
        ("cpu", &quota.cpu, demand.cpu, format!("{millicores}m")),
        (
            "memory",
            &quota.memory,
            demand.memory,
            format!("{mebibytes}Mi"),
        ),
    ] {
        if let Some(hard) = hard
            && quantity_value(hard).is_some_and(|hard| hard < needed * (1.0 - 1e-9))
        {
            short.push(format!("{name} {hard} < {shown}"));
        }
    }
    (!short.is_empty()).then(|| {
        format!(
            "policy.resources.quota is too small for this cell: {}. The quota counts every \
             component's pod, the protocol prober and running actions; this check leaves room \
             for one action.",
            short.join(", ")
        )
    })
}

#[derive(Default)]
struct Demand {
    pods: u64,
    volumes: u64,
    cpu: f64,
    memory: f64,
}

impl Demand {
    /// A pod reserves the larger of its containers' sum and its largest init
    /// container, as the quota counts it.
    fn add_pod(&mut self, pod: Option<&PodSpec>, replicas: u64) {
        let Some(pod) = pod else {
            return;
        };
        #[allow(clippy::cast_precision_loss, reason = "replica counts are tiny")]
        let replicas_f = replicas as f64;
        for (key, total) in [("cpu", &mut self.cpu), ("memory", &mut self.memory)] {
            let containers: f64 = pod
                .containers
                .iter()
                .map(|container| request(container.resources.as_ref(), key))
                .sum();
            let init = pod
                .init_containers
                .iter()
                .flatten()
                .map(|container| request(container.resources.as_ref(), key))
                .fold(0.0, f64::max);
            *total += containers.max(init) * replicas_f;
        }
        self.pods += replicas;
    }

    fn add_requests(&mut self, resources: Option<&ResourceRequirements>) {
        self.cpu += request(resources, "cpu");
        self.memory += request(resources, "memory");
    }
}

fn request(resources: Option<&ResourceRequirements>, key: &str) -> f64 {
    resources
        .and_then(|resources| resources.requests.as_ref())
        .and_then(|requests| requests.get(key))
        .and_then(|quantity| quantity_value(&quantity.0))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cell(quota: serde_json::Value) -> (CellSpec, ResolvedLock) {
        let mut spec: CellSpec =
            serde_json::from_str(include_str!("../../../examples/developer-cell.json")).unwrap();
        spec.components.retain(|component| component.id == "chain");
        spec.links.clear();
        spec.policy.resources.quota = serde_json::from_value(quota).unwrap();
        let lock =
            proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).unwrap();
        (spec, lock)
    }

    #[test]
    fn a_quota_must_hold_every_pod_the_prober_and_one_action() {
        let (spec, lock) = cell(json!(null));
        assert_eq!(quota_shortfall(&spec, &lock), None);
        let (spec, lock) = cell(json!({"cpu":"20","memory":"64Gi","pods":500,"volumes":50}));
        assert_eq!(quota_shortfall(&spec, &lock), None);
        // One component: its pod, the prober and one action need three pods.
        let (spec, lock) = cell(json!({"pods":2,"memory":"1Mi"}));
        let message = quota_shortfall(&spec, &lock).expect("too small");
        assert!(message.contains("pods 2 < 3"), "{message}");
        assert!(message.contains("memory 1Mi < "), "{message}");
        assert!(!message.contains("cpu"), "{message}");
        let (spec, lock) = cell(json!({"pods":3}));
        assert_eq!(quota_shortfall(&spec, &lock), None);
    }
}
