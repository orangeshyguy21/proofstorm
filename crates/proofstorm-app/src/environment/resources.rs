//! Project only endpoint and resource fields from the same renderer the controller uses.
use crate::{Error, connections};
use k8s_openapi::{
    api::core::v1::{PodSpec, ResourceRequirements},
    apimachinery::pkg::api::resource::Quantity,
};
use proofstorm_core::{CellInstance, PublishedRevision};
use proofstorm_kube::{COMPONENT_LABEL, PROTOCOL_PROBER_NAME, render_cell, render_security_spine};
use proofstorm_view::ReplicaPolicy;
use std::collections::BTreeMap;

pub use proofstorm_view::{
    ContainerDemand, Endpoint, Quantities, ResourceDemand, StorageDemand, WorkloadDemand,
};

pub(super) fn include_runtime(
    resources: &mut Option<ResourceDemand>,
    resource: Option<&proofstorm_kube::ProofstormCell>,
    prober: Option<(i32, proofstorm_view::WorkloadObservation)>,
) {
    let Some(resources) = resources else {
        return;
    };
    if let Some((replicas, observation)) = prober {
        if let Some(workload) = resources.workloads.iter_mut().find(|w| {
            w.name == PROTOCOL_PROBER_NAME && w.replica_policy == ReplicaPolicy::ControllerScheduled
        }) {
            workload.replicas = Some(replicas);
            workload.observation = Some(observation);
        }
    }
    resources.retained_storage = resource
        .and_then(|r| r.status.as_ref())
        .map(|s| s.retained_storage.clone())
        .unwrap_or_default();
}

#[allow(
    clippy::too_many_lines,
    reason = "one resource projection keeps workload, storage and endpoint demand consistent"
)]
pub(super) fn project(
    instance: &CellInstance,
    revision: &PublishedRevision,
) -> Result<(ResourceDemand, Vec<Endpoint>), Error> {
    let rendered = render_cell(
        &instance.instance_key,
        &revision.digest,
        &revision.cell,
        &revision.lock,
    )
    .map_err(|_| {
        Error::problem(
            "render_unavailable",
            "desired resource projection unavailable",
        )
    })?;
    let spine = render_security_spine(&instance.instance_key);
    let defaults = spine.limits.spec.as_ref().and_then(|s| s.limits.first());
    let request_defaults = quantities(defaults.and_then(|d| d.default_request.as_ref()));
    let limit_defaults = quantities(defaults.and_then(|d| d.default.as_ref()));
    let mut workloads = Vec::new();
    for deployment in &rendered.deployments {
        if let Some(spec) = &deployment.spec {
            workloads.push(workload(
                "Deployment",
                &deployment.metadata,
                spec.replicas,
                spec.template.spec.as_ref(),
                &request_defaults,
                &limit_defaults,
            ));
        }
    }
    for set in &rendered.stateful_sets {
        if let Some(spec) = &set.spec {
            workloads.push(workload(
                "StatefulSet",
                &set.metadata,
                spec.replicas,
                spec.template.spec.as_ref(),
                &request_defaults,
                &limit_defaults,
            ));
        }
    }
    workloads.sort_by(|a, b| a.name.cmp(&b.name));
    let mut storage: Vec<StorageDemand> = rendered
        .persistent_volume_claims
        .iter()
        .map(|p| StorageDemand {
            workload: None,
            replicas: 1,
            name: p.metadata.name.clone().unwrap_or_default(),
            component: component(&p.metadata),
            requests: quantities(
                p.spec
                    .as_ref()
                    .and_then(|s| s.resources.as_ref())
                    .and_then(|r| r.requests.as_ref()),
            ),
        })
        .collect();
    for set in &rendered.stateful_sets {
        if let Some(spec) = &set.spec {
            for claim in spec.volume_claim_templates.iter().flatten() {
                storage.push(StorageDemand {
                    name: claim.metadata.name.clone().unwrap_or_default(),
                    workload: set.metadata.name.clone(),
                    replicas: spec.replicas.unwrap_or(1),
                    component: component(&set.metadata),
                    requests: quantities(
                        claim
                            .spec
                            .as_ref()
                            .and_then(|s| s.resources.as_ref())
                            .and_then(|r| r.requests.as_ref()),
                    ),
                });
            }
        }
    }
    let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
    let mut endpoints = Vec::new();
    for service in &rendered.services {
        let Some(component) = component(&service.metadata) else {
            continue;
        };
        for port in service
            .spec
            .as_ref()
            .and_then(|s| s.ports.as_ref())
            .into_iter()
            .flatten()
        {
            let name = port.name.clone().unwrap_or_default();
            let local_authentication = connections::endpoint(revision, &component, &name)
                .ok()
                .map(|(_, a)| a);
            endpoints.push(Endpoint {component:component.clone(),name,transport:port.protocol.clone().unwrap_or_else(||"TCP".into()),cluster_host:format!("{}.{}.svc",service.metadata.name.as_deref().unwrap_or_default(),namespace),port:port.port,local_connection_supported:local_authentication.is_some(),local_authentication,access_context:format!("cluster DNS; use {} connect for supported loopback access; this descriptor does not create a tunnel or assert readiness", crate::command_name())});
        }
    }
    endpoints.sort_by(|a, b| (&a.component, &a.name).cmp(&(&b.component, &b.name)));
    Ok((
        ResourceDemand {
            retained_storage: std::collections::BTreeMap::new(),
            workloads,
            storage,
        },
        endpoints,
    ))
}
fn component(meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta) -> Option<String> {
    meta.labels
        .as_ref()
        .and_then(|l| l.get(COMPONENT_LABEL))
        .cloned()
}
fn quantities(values: Option<&BTreeMap<String, Quantity>>) -> Quantities {
    values
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.0.clone()))
        .collect()
}
fn demands(
    resources: Option<&ResourceRequirements>,
    defaults: &Quantities,
    limits: bool,
) -> Quantities {
    let mut values = defaults.clone();
    values.extend(quantities(resources.and_then(|r| {
        if limits {
            r.limits.as_ref()
        } else {
            r.requests.as_ref()
        }
    })));
    values
}
fn workload(
    kind: &str,
    meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    replicas: Option<i32>,
    pod: Option<&PodSpec>,
    requests: &Quantities,
    limits: &Quantities,
) -> WorkloadDemand {
    let mut containers = Vec::new();
    if let Some(pod) = pod {
        for (init, c) in pod
            .containers
            .iter()
            .map(|c| (false, c))
            .chain(pod.init_containers.iter().flatten().map(|c| (true, c)))
        {
            containers.push(ContainerDemand {
                name: c.name.clone(),
                init,
                requests: demands(c.resources.as_ref(), requests, false),
                limits: demands(c.resources.as_ref(), limits, true),
            });
        }
    }
    WorkloadDemand {
        name: meta.name.clone().unwrap_or_default(),
        component: component(meta),
        kind: kind.into(),
        replicas: (meta.name.as_deref() != Some(PROTOCOL_PROBER_NAME))
            .then_some(replicas.unwrap_or(1)),
        replica_policy: if meta.name.as_deref() == Some(PROTOCOL_PROBER_NAME) {
            ReplicaPolicy::ControllerScheduled
        } else {
            ReplicaPolicy::Fixed
        },
        observation: None,
        omitted_container_count: 0,
        containers,
    }
}
