//! Give every container Proofstorm renders explicit CPU and memory.
//!
//! The namespace `LimitRange` only supplies default requests. Limits are set
//! here instead, because a `LimitRange` default limit applies to every container
//! that omits one, so a component could never opt out of it with `none`.
use std::collections::BTreeMap;

use k8s_openapi::{
    api::core::v1::{Container, PodSpec, ResourceRequirements},
    apimachinery::pkg::api::resource::Quantity,
};
use proofstorm_core::{Effective, Resources};

/// The container a component's `resources` apply to: `component`, else the first.
fn main_container(pod: &PodSpec) -> Option<usize> {
    pod.containers
        .iter()
        .position(|container| container.name == "component")
        .or_else(|| (!pod.containers.is_empty()).then_some(0))
}

/// Merge every container's own values with `defaults`. The main container then
/// takes `main` on top, where `none` removes a limit.
pub fn apply(pod: &mut PodSpec, defaults: &Effective, main: Option<&Resources>) {
    let main_index = main.and(main_container(pod));
    for (index, container) in pod.containers.iter_mut().enumerate() {
        let mut merged = current(container).or(defaults);
        if Some(index) == main_index
            && let Some(main) = main
        {
            merged = merged.overlaid(main);
        }
        write(container, merged.requests_within_limits());
    }
    for container in pod.init_containers.iter_mut().flatten() {
        let merged = current(container).or(defaults);
        write(container, merged.requests_within_limits());
    }
}

fn current(container: &Container) -> Effective {
    let resources = container.resources.as_ref();
    let get = |map: Option<&BTreeMap<String, Quantity>>, key: &str| {
        map.and_then(|map| map.get(key))
            .map(|quantity| quantity.0.clone())
    };
    let requests = resources.and_then(|r| r.requests.as_ref());
    let limits = resources.and_then(|r| r.limits.as_ref());
    Effective {
        cpu_request: get(requests, "cpu"),
        cpu_limit: get(limits, "cpu"),
        memory_request: get(requests, "memory"),
        memory_limit: get(limits, "memory"),
    }
}

/// A quantity map of the amounts that are set.
pub(crate) fn quantities<const N: usize>(
    amounts: [(&str, Option<String>); N],
) -> BTreeMap<String, Quantity> {
    amounts
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), Quantity(value))))
        .collect()
}

fn write(container: &mut Container, effective: Effective) {
    let map = |cpu: Option<String>, memory: Option<String>| {
        let map = quantities([("cpu", cpu), ("memory", memory)]);
        (!map.is_empty()).then_some(map)
    };
    let mut resources = container.resources.take().unwrap_or_default();
    resources.requests = map(effective.cpu_request, effective.memory_request);
    resources.limits = map(effective.cpu_limit, effective.memory_limit);
    container.resources = (resources != ResourceRequirements::default()).then_some(resources);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{NO_LIMIT, ResourceAmount, ResourcePolicy};
    use serde_json::json;

    fn pod(containers: &serde_json::Value, init: &serde_json::Value) -> PodSpec {
        serde_json::from_value(json!({"containers": containers, "initContainers": init})).unwrap()
    }

    fn resources(container: &Container) -> serde_json::Value {
        serde_json::to_value(container.resources.as_ref()).unwrap()
    }

    #[test]
    fn every_container_gets_defaults_without_a_cpu_limit() {
        let mut pod = pod(
            &json!([{"name": "component"}, {"name": "sidecar"}]),
            &json!([{"name": "init"}]),
        );
        apply(
            &mut pod,
            &ResourcePolicy::default().container_defaults(),
            None,
        );
        let expected =
            json!({"requests": {"cpu": "100m", "memory": "128Mi"}, "limits": {"memory": "512Mi"}});
        for container in pod
            .containers
            .iter()
            .chain(pod.init_containers.iter().flatten())
        {
            assert_eq!(resources(container), expected, "{}", container.name);
        }
    }

    #[test]
    fn the_component_override_reaches_only_the_main_container() {
        let mut pod = pod(
            &json!([{"name": "sidecar"}, {"name": "component",
                "resources": {"requests": {"memory": "512Mi"}, "limits": {"cpu": "2", "memory": "2Gi"}}}]),
            &json!([]),
        );
        let main = Resources {
            cpu: Some(ResourceAmount {
                request: None,
                limit: Some(NO_LIMIT.into()),
            }),
            memory: Some(ResourceAmount {
                request: None,
                limit: Some("256Mi".into()),
            }),
        };
        apply(
            &mut pod,
            &ResourcePolicy::default().container_defaults(),
            Some(&main),
        );
        assert_eq!(
            resources(&pod.containers[1]),
            json!({"requests": {"cpu": "100m", "memory": "256Mi"}, "limits": {"memory": "256Mi"}})
        );
        assert_eq!(
            resources(&pod.containers[0]),
            json!({"requests": {"cpu": "100m", "memory": "128Mi"}, "limits": {"memory": "512Mi"}})
        );
    }

    #[test]
    fn a_cell_without_limits_renders_requests_only() {
        let policy: ResourcePolicy = serde_json::from_value(json!({"defaults": {
            "memory": {"limit": "none"}}}))
        .unwrap();
        let mut pod = pod(&json!([{"name": "component"}]), &json!([]));
        apply(&mut pod, &policy.container_defaults(), None);
        assert_eq!(
            resources(&pod.containers[0]),
            json!({"requests": {"cpu": "100m", "memory": "128Mi"}})
        );
    }
}
