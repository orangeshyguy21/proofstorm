use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    core::v1::{ConfigMap, PersistentVolumeClaim, Pod, Service},
    discovery::v1::EndpointSlice,
    networking::v1::NetworkPolicy,
};
use proofstorm_core::{
    ComponentCondition, ComponentConditionReason, ComponentConditionState, ComponentConditionType,
    ComponentKind, ComponentPlanContract, ComponentPlanInput, ComponentSpec, ComponentStatus,
    InventoryEntry, LabSpec, LinkKind, LinkedStateObservationContract, MAX_COMPONENT_CONDITIONS,
    MAX_CONDITION_MESSAGE_BYTES, ProtocolProbePlan, ResolvedLock, TargetDescriptorContract,
    WorkloadControllerKind, default_backend_registry,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    BACKEND_ID_ANNOTATION, EXECUTION_STATE_CONTRACT_ANNOTATION, INSTANCE_LABEL,
    REVISION_DIGEST_ANNOTATION, ROLLOUT_DIGEST_ANNOTATION, instance_namespace,
};

const COMPONENT_LABEL: &str = "proofstorm.dev/component";
const NETWORK_IDENTITY_LABEL: &str = "proofstorm.dev/network-identity";
const RPC_USER: &str = "proofstorm";
const RPC_PASSWORD: &str = "proofstorm-regtest-only";
pub const PROTOCOL_PROBER_NAME: &str = "proofstorm-protocol-prober";
pub const PROTOCOL_PROBER_LABEL: &str = "proofstorm.dev/prober";
pub const PROTOCOL_PROBER_DIGEST_ANNOTATION: &str = "proofstorm.dev/prober-digest";
pub const PROTOCOL_PROBER_LEASE_ANNOTATION: &str = "proofstorm.dev/prober-lease";
const INACTIVE_PROBER_LEASE: &str = "inactive";
const PROBER_IMAGE: &str = "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662";

type ComponentRenderer = fn(&ComponentPlanContract) -> Result<RenderedComponent, AdapterError>;

static COMPONENT_RENDERERS: LazyLock<BTreeMap<&'static str, ComponentRenderer>> =
    LazyLock::new(|| {
        BTreeMap::from([
            (
                "attacker-workspace",
                render_attacker_component as ComponentRenderer,
            ),
            ("bitcoin-core", render_bitcoin_component),
            ("cdk", render_cdk_component),
            ("cln", render_cln_component),
            ("lnd", render_lnd_component),
            ("nutshell-wallet", render_wallet_component),
        ])
    });

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("component {component:?} has no resolved lock entry")]
    MissingLock { component: String },
    #[error("component {component:?} requires a {link:?} link")]
    MissingLink { component: String, link: LinkKind },
    #[error("component {component:?} references missing component {target:?}")]
    MissingTarget { component: String, target: String },
    #[error("adapter {adapter:?} is not installed")]
    UnsupportedAdapter { adapter: String },
    #[error("component plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("adapter rendered an invalid Kubernetes resource: {0}")]
    InvalidResource(#[from] serde_json::Error),
}

#[derive(Debug, Default)]
pub struct RenderedLab {
    pub plans: Vec<ComponentPlanContract>,
    pub config_maps: Vec<ConfigMap>,
    pub services: Vec<Service>,
    pub stateful_sets: Vec<StatefulSet>,
    pub deployments: Vec<Deployment>,
    pub persistent_volume_claims: Vec<PersistentVolumeClaim>,
    pub network_policies: Vec<NetworkPolicy>,
}

/// Pure, controller-safe resource output for one compiled component plan.
#[derive(Debug, Default)]
pub struct RenderedComponent {
    pub config_maps: Vec<ConfigMap>,
    pub services: Vec<Service>,
    pub stateful_sets: Vec<StatefulSet>,
    pub deployments: Vec<Deployment>,
    pub persistent_volume_claims: Vec<PersistentVolumeClaim>,
}

/// Cluster observations supplied to the pure component status projector.
pub struct ComponentObservationResources<'a> {
    pub deployments: &'a [Deployment],
    pub stateful_sets: &'a [StatefulSet],
    pub persistent_volume_claims: &'a [PersistentVolumeClaim],
    pub services: &'a [Service],
    pub endpoint_slices: &'a [EndpointSlice],
    pub pods: &'a [Pod],
}

impl RenderedLab {
    #[must_use]
    pub fn inventory(&self) -> Vec<InventoryEntry> {
        let mut inventory = Vec::new();
        append_inventory(&mut inventory, "v1", "ConfigMap", &self.config_maps);
        append_inventory(&mut inventory, "v1", "Service", &self.services);
        append_inventory(
            &mut inventory,
            "apps/v1",
            "StatefulSet",
            &self.stateful_sets,
        );
        append_inventory(&mut inventory, "apps/v1", "Deployment", &self.deployments);
        append_inventory(
            &mut inventory,
            "v1",
            "PersistentVolumeClaim",
            &self.persistent_volume_claims,
        );
        append_inventory(
            &mut inventory,
            "networking.k8s.io/v1",
            "NetworkPolicy",
            &self.network_policies,
        );
        inventory.sort_by(|left, right| {
            (&left.api_version, &left.kind, &left.namespace, &left.name).cmp(&(
                &right.api_version,
                &right.kind,
                &right.namespace,
                &right.name,
            ))
        });
        inventory
    }

    fn append_component(&mut self, mut component: RenderedComponent) {
        self.config_maps.append(&mut component.config_maps);
        self.services.append(&mut component.services);
        self.stateful_sets.append(&mut component.stateful_sets);
        self.deployments.append(&mut component.deployments);
        self.persistent_volume_claims
            .append(&mut component.persistent_volume_claims);
    }

    fn sort_resources(&mut self) {
        sort_by_name(&mut self.config_maps);
        sort_by_name(&mut self.services);
        sort_by_name(&mut self.stateful_sets);
        sort_by_name(&mut self.deployments);
        sort_by_name(&mut self.persistent_volume_claims);
        sort_by_name(&mut self.network_policies);
    }
}

/// Compile one immutable, cluster-free plan per effective lab component.
///
/// # Errors
///
/// Returns an error when a component has no lock entry or its backend contract
/// cannot compile the locked component identity.
pub fn compile_component_plans(
    instance_key: &str,
    revision_digest: &str,
    lab: &LabSpec,
    lock: &ResolvedLock,
) -> Result<Vec<ComponentPlanContract>, AdapterError> {
    let registry = default_backend_registry();
    let mut plans = lab
        .components
        .iter()
        .map(|component| {
            let entry = lock
                .entries
                .iter()
                .find(|entry| entry.component_id == component.id)
                .ok_or_else(|| AdapterError::MissingLock {
                    component: component.id.clone(),
                })?;
            let relevant_links = lab
                .links
                .iter()
                .filter(|link| link.from == component.id)
                .cloned()
                .collect::<Vec<_>>();
            let mut linked_targets = BTreeMap::new();
            let mut linked_state = BTreeMap::new();
            for link in &relevant_links {
                let target = lab
                    .components
                    .iter()
                    .find(|target| target.id == link.to)
                    .ok_or_else(|| AdapterError::MissingTarget {
                        component: component.id.clone(),
                        target: link.to.clone(),
                    })?;
                let target_lock = lock
                    .entries
                    .iter()
                    .find(|entry| entry.component_id == target.id)
                    .ok_or_else(|| AdapterError::MissingLock {
                        component: target.id.clone(),
                    })?;
                let target_backend = registry
                    .require(&target_lock.catalog_id)
                    .map_err(AdapterError::InvalidPlan)?;
                linked_targets.insert(
                    target.id.clone(),
                    TargetDescriptorContract {
                        component_id: target.id.clone(),
                        kind: target.kind,
                        ports: target_backend.service_ports.clone(),
                    },
                );
                linked_state.insert(
                    target.id.clone(),
                    LinkedStateObservationContract {
                        component_id: target.id.clone(),
                        state_contract: target_backend.execution_state_contract.clone(),
                        storage: target_backend
                            .storage_requirements
                            .iter()
                            .map(|requirement| requirement.resolve(&target.id))
                            .collect(),
                    },
                );
            }
            registry
                .compile_contract(&ComponentPlanInput {
                    instance_key: instance_key.to_owned(),
                    revision_digest: revision_digest.to_owned(),
                    component: component.clone(),
                    lock: entry.clone(),
                    relevant_links,
                    linked_targets,
                    linked_state,
                })
                .map_err(AdapterError::InvalidPlan)
        })
        .collect::<Result<Vec<_>, _>>()?;
    plans.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    Ok(plans)
}

/// Render a resolved lab into bounded Kubernetes protocol workloads.
///
/// # Errors
///
/// Returns an error when a component is unresolved, an adapter is unsupported,
/// a required topology link is absent, or an internal resource contract is
/// invalid.
pub fn render_lab(
    instance_key: &str,
    revision_digest: &str,
    lab: &LabSpec,
    lock: &ResolvedLock,
) -> Result<RenderedLab, AdapterError> {
    let namespace = instance_namespace(instance_key);
    let plans = compile_component_plans(instance_key, revision_digest, lab, lock)?;
    let mut rendered = RenderedLab {
        plans: plans.clone(),
        ..RenderedLab::default()
    };
    rendered
        .network_policies
        .push(resource(action_network_policy(instance_key, &namespace))?);

    for plan in &plans {
        rendered
            .network_policies
            .push(render_component_network_policy(
                instance_key,
                &plan.component_id,
                &[],
            )?);
        let generator = COMPONENT_RENDERERS
            .get(plan.backend_id.as_str())
            .ok_or_else(|| AdapterError::UnsupportedAdapter {
                adapter: plan.backend_id.clone(),
            })?;
        rendered.append_component(generator(plan)?);
    }
    if let Some(prober) = render_protocol_prober(&plans)? {
        rendered.deployments.push(prober);
    }
    rendered.sort_resources();
    Ok(rendered)
}

/// Render one bounded, credential-free protocol prober for the complete lab.
///
/// # Errors
///
/// Returns an error only if the fixed restricted Deployment is invalid.
pub fn render_protocol_prober(
    plans: &[ComponentPlanContract],
) -> Result<Option<Deployment>, AdapterError> {
    let Some(first) = plans.iter().find(|plan| plan.protocol_probe.is_some()) else {
        return Ok(None);
    };
    let probe_count = plans
        .iter()
        .filter(|plan| plan.protocol_probe.is_some())
        .count();
    if probe_count > crate::MAX_PROTOCOL_PROBES_PER_LAB {
        return Err(AdapterError::InvalidPlan(format!(
            "protocol probe count {probe_count} exceeds per-lab maximum {}",
            crate::MAX_PROTOCOL_PROBES_PER_LAB
        )));
    }
    let namespace = instance_namespace(&first.instance_key);
    let digest = protocol_probe_digest(plans);
    let mut prober_labels = labels(&first.instance_key, None);
    prober_labels.insert(PROTOCOL_PROBER_LABEL.into(), "true".into());
    prober_labels.insert("proofstorm.dev/operation".into(), "protocol-prober".into());
    prober_labels.insert(
        NETWORK_IDENTITY_LABEL.into(),
        "proofstorm-protocol-prober".into(),
    );
    let containers = plans
        .iter()
        .filter_map(|plan| {
            let probe = plan.protocol_probe.as_ref()?;
            let readiness_probe = match probe {
                ProtocolProbePlan::Tcp { port } => json!({
                    "exec": {"command": ["nc", "-z", "-w", "2", plan.component_id, port.to_string()]},
                    "initialDelaySeconds": 1, "timeoutSeconds": 2, "periodSeconds": 5,
                    "failureThreshold": 3, "successThreshold": 1
                }),
                ProtocolProbePlan::HttpGet { port, path } => json!({
                    "exec": {"command": ["wget", "-q", "-T", "2", "-O", "/dev/null",
                        format!("http://{}:{port}{path}", plan.component_id)]},
                    "initialDelaySeconds": 1, "timeoutSeconds": 2, "periodSeconds": 5,
                    "failureThreshold": 3, "successThreshold": 1
                }),
            };
            Some(json!({
                "name": protocol_probe_container_name(&plan.component_id),
                "image": PROBER_IMAGE,
                "imagePullPolicy": "IfNotPresent",
                "command": ["sh", "-c", "trap 'exit 0' TERM INT; while :; do sleep 3600; done"],
                "securityContext": container_security(),
                "resources": {
                    "requests": {"cpu": "5m", "memory": "4Mi"},
                    "limits": {"cpu": "25m", "memory": "16Mi"}
                },
                "readinessProbe": readiness_probe
            }))
        })
        .collect::<Vec<_>>();
    let deployment = resource(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": PROTOCOL_PROBER_NAME,
            "namespace": namespace,
            "labels": prober_labels.clone(),
            "annotations": {
                PROTOCOL_PROBER_DIGEST_ANNOTATION: digest,
                PROTOCOL_PROBER_LEASE_ANNOTATION: INACTIVE_PROBER_LEASE
            }
        },
        "spec": {
            "replicas": 0,
            "strategy": {"type": "Recreate"},
            "selector": {"matchLabels": {INSTANCE_LABEL: first.instance_key, PROTOCOL_PROBER_LABEL: "true"}},
            "template": {
                "metadata": {
                    "labels": prober_labels,
                    "annotations": {
                        PROTOCOL_PROBER_DIGEST_ANNOTATION: digest,
                        PROTOCOL_PROBER_LEASE_ANNOTATION: INACTIVE_PROBER_LEASE
                    }
                },
                "spec": {
                    "serviceAccountName": "proofstorm-workload",
                    "automountServiceAccountToken": false,
                    "enableServiceLinks": false,
                    "terminationGracePeriodSeconds": 1,
                    "securityContext": pod_security(),
                    "affinity": instance_affinity(&first.instance_key),
                    "containers": containers
                }
            }
        }
    }))?;
    Ok(Some(deployment))
}

fn protocol_probe_digest(plans: &[ComponentPlanContract]) -> String {
    let probes = plans
        .iter()
        .filter_map(|plan| {
            plan.protocol_probe
                .as_ref()
                .map(|probe| (&plan.component_id, &plan.rollout_digest, probe))
        })
        .collect::<Vec<_>>();
    proofstorm_core::digest_json(&probes)
}

fn protocol_probe_container_name(component_id: &str) -> String {
    let digest = proofstorm_core::digest_json(&component_id);
    let short_digest = &digest["sha256:".len().."sha256:".len() + 8];
    let prefix = &component_id[..component_id.len().min(48)];
    format!("probe-{prefix}-{short_digest}")
}

/// Observe workload readiness against the exact compiled rollout identity.
#[must_use]
pub fn observe_component_statuses(
    namespace: &str,
    plans: &[ComponentPlanContract],
    resources: &ComponentObservationResources<'_>,
    previous: &[ComponentStatus],
    intentionally_stopped: &BTreeSet<String>,
    observed_at_unix: i64,
) -> Vec<ComponentStatus> {
    let prober_digest = protocol_probe_digest(plans);
    let mut statuses = plans
        .iter()
        .map(|plan| {
            let stopped = intentionally_stopped.contains(&plan.component_id);
            let mut conditions = observe_atomic_conditions(
                plan,
                resources,
                &prober_digest,
                stopped,
                observed_at_unix,
            );
            for aggregation in &plan.condition_aggregation {
                let state = aggregate_condition_state(&aggregation.all_of, &conditions);
                let (reason, message) =
                    if stopped && aggregation.condition == ComponentConditionType::ComponentReady {
                        (
                            ComponentConditionReason::IntentionallyStopped,
                            "component is intentionally stopped",
                        )
                    } else if state == ComponentConditionState::True {
                        (
                            ComponentConditionReason::ComponentOperational,
                            "all applicable readiness conditions are true",
                        )
                    } else {
                        (
                            ComponentConditionReason::ComponentNotOperational,
                            "one or more applicable readiness conditions are not true",
                        )
                    };
                conditions.push(component_condition(
                    aggregation.condition,
                    state,
                    reason,
                    message,
                    observed_at_unix,
                ));
            }
            conditions.sort_by_key(|condition| condition.condition_type);
            conditions.dedup_by_key(|condition| condition.condition_type);
            debug_assert!(conditions.len() <= MAX_COMPONENT_CONDITIONS);
            if let Some(previous) = previous.iter().find(|status| {
                status.id == plan.component_id
                    && status.observed_revision_digest == plan.revision_digest
                    && status.observed_rollout_digest == plan.rollout_digest
            }) {
                preserve_condition_transitions(&mut conditions, &previous.conditions);
            }
            debug_assert!(conditions.iter().all(|condition| {
                plan.condition_reasons
                    .get(&condition.condition_type)
                    .is_some_and(|reasons| reasons.contains(&condition.reason))
            }));
            let mut status = ComponentStatus {
                id: plan.component_id.clone(),
                kind: plan.kind,
                observed_revision_digest: plan.revision_digest.clone(),
                observed_rollout_digest: plan.rollout_digest.clone(),
                conditions,
                ready: false,
                service: format!("{}.{namespace}.svc", plan.component_id),
                ports: plan.target_descriptor.ports.clone(),
            };
            status.derive_ready();
            status
        })
        .collect::<Vec<_>>();
    resolve_dependency_conditions(
        plans,
        &mut statuses,
        previous,
        intentionally_stopped,
        observed_at_unix,
    );
    statuses
}

fn resolve_dependency_conditions(
    plans: &[ComponentPlanContract],
    statuses: &mut [ComponentStatus],
    previous: &[ComponentStatus],
    intentionally_stopped: &BTreeSet<String>,
    observed_at_unix: i64,
) {
    for _ in 0..plans.len() {
        let prior_pass = statuses.to_vec();
        for (plan, status) in plans.iter().zip(statuses.iter_mut()) {
            if !plan
                .applicable_conditions
                .contains(&ComponentConditionType::DependenciesReady)
            {
                continue;
            }
            let dependency_states = plan.relevant_links.iter().map(|link| {
                prior_pass
                    .iter()
                    .find(|candidate| candidate.id == link.to)
                    .and_then(|candidate| {
                        candidate.conditions.iter().find(|condition| {
                            condition.condition_type == ComponentConditionType::ComponentReady
                        })
                    })
                    .map_or(ComponentConditionState::Unknown, |condition| {
                        condition.state
                    })
            });
            let state = aggregate_states(dependency_states);
            let (reason, message) = if state == ComponentConditionState::True {
                (
                    ComponentConditionReason::DependenciesSatisfied,
                    "all linked component dependencies are ready",
                )
            } else {
                (
                    ComponentConditionReason::DependenciesUnsatisfied,
                    "one or more linked component dependencies are not ready",
                )
            };
            replace_condition(
                status,
                component_condition(
                    ComponentConditionType::DependenciesReady,
                    state,
                    reason,
                    message,
                    observed_at_unix,
                ),
            );
            recompute_component_ready(
                plan,
                status,
                intentionally_stopped.contains(&plan.component_id),
                observed_at_unix,
            );
        }
    }
    for status in statuses {
        if let Some(previous) = previous.iter().find(|candidate| {
            candidate.id == status.id
                && candidate.observed_revision_digest == status.observed_revision_digest
                && candidate.observed_rollout_digest == status.observed_rollout_digest
        }) {
            preserve_condition_transitions(&mut status.conditions, &previous.conditions);
        }
        status.derive_ready();
    }
}

fn aggregate_states(
    states: impl IntoIterator<Item = ComponentConditionState>,
) -> ComponentConditionState {
    let mut saw_unknown = false;
    for state in states {
        match state {
            ComponentConditionState::False => return ComponentConditionState::False,
            ComponentConditionState::Unknown => saw_unknown = true,
            ComponentConditionState::True => {}
        }
    }
    if saw_unknown {
        ComponentConditionState::Unknown
    } else {
        ComponentConditionState::True
    }
}

fn replace_condition(status: &mut ComponentStatus, replacement: ComponentCondition) {
    if let Some(condition) = status
        .conditions
        .iter_mut()
        .find(|condition| condition.condition_type == replacement.condition_type)
    {
        *condition = replacement;
    } else {
        status.conditions.push(replacement);
        status
            .conditions
            .sort_by_key(|condition| condition.condition_type);
    }
}

fn recompute_component_ready(
    plan: &ComponentPlanContract,
    status: &mut ComponentStatus,
    stopped: bool,
    observed_at_unix: i64,
) {
    let Some(aggregation) = plan
        .condition_aggregation
        .iter()
        .find(|contract| contract.condition == ComponentConditionType::ComponentReady)
    else {
        return;
    };
    let state = aggregate_condition_state(&aggregation.all_of, &status.conditions);
    let (reason, message) = if stopped {
        (
            ComponentConditionReason::IntentionallyStopped,
            "component is intentionally stopped",
        )
    } else if state == ComponentConditionState::True {
        (
            ComponentConditionReason::ComponentOperational,
            "all applicable readiness conditions are true",
        )
    } else {
        (
            ComponentConditionReason::ComponentNotOperational,
            "one or more applicable readiness conditions are not true",
        )
    };
    replace_condition(
        status,
        component_condition(
            ComponentConditionType::ComponentReady,
            state,
            reason,
            message,
            observed_at_unix,
        ),
    );
}

fn observe_atomic_conditions(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
    prober_digest: &str,
    stopped: bool,
    observed_at_unix: i64,
) -> Vec<ComponentCondition> {
    plan.applicable_conditions
        .iter()
        .filter_map(|condition_type| {
            let (state, reason, message) = match condition_type {
                ComponentConditionType::WorkloadReady | ComponentConditionType::ProtocolReady
                    if stopped =>
                {
                    (
                        ComponentConditionState::False,
                        ComponentConditionReason::IntentionallyStopped,
                        "component is intentionally stopped",
                    )
                }
                ComponentConditionType::WorkloadReady => workload_observation(plan, resources),
                ComponentConditionType::StorageReady => storage_observation(plan, resources),
                ComponentConditionType::CredentialsReady => credential_observation(plan, resources),
                ComponentConditionType::ServiceReady => service_observation(plan, resources),
                ComponentConditionType::ProtocolReady => {
                    protocol_observation(plan, resources, prober_digest)
                }
                ComponentConditionType::DependenciesReady => {
                    not_observed("component dependencies have not been observed")
                }
                ComponentConditionType::ExperimentControllable => (
                    ComponentConditionState::True,
                    ComponentConditionReason::ControlAvailable,
                    "typed control surfaces are available",
                ),
                ComponentConditionType::ComponentReady => return None,
            };
            Some(component_condition(
                *condition_type,
                state,
                reason,
                message,
                observed_at_unix,
            ))
        })
        .collect()
}

fn workload_observation(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    match plan.workload.kind {
        WorkloadControllerKind::Deployment => {
            let Some(workload) = resources
                .deployments
                .iter()
                .find(|workload| workload.metadata.name.as_deref() == Some(&plan.workload.name))
            else {
                return not_observed("owning Deployment has not been observed");
            };
            let Some(spec) = workload.spec.as_ref() else {
                return workload_unavailable("owning Deployment has no specification");
            };
            if !workload_identity_matches(
                plan,
                workload.metadata.annotations.as_ref(),
                spec.template
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.annotations.as_ref()),
            ) {
                return stale_workload();
            }
            let status = workload.status.as_ref();
            workload_replica_observation(
                workload.metadata.generation,
                status.and_then(|status| status.observed_generation),
                status.and_then(|status| status.available_replicas),
                plan.workload.desired_replicas,
            )
        }
        WorkloadControllerKind::StatefulSet => {
            let Some(workload) = resources
                .stateful_sets
                .iter()
                .find(|workload| workload.metadata.name.as_deref() == Some(&plan.workload.name))
            else {
                return not_observed("owning StatefulSet has not been observed");
            };
            let Some(spec) = workload.spec.as_ref() else {
                return workload_unavailable("owning StatefulSet has no specification");
            };
            if !workload_identity_matches(
                plan,
                workload.metadata.annotations.as_ref(),
                spec.template
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.annotations.as_ref()),
            ) {
                return stale_workload();
            }
            let status = workload.status.as_ref();
            workload_replica_observation(
                workload.metadata.generation,
                status.and_then(|status| status.observed_generation),
                status.and_then(|status| status.ready_replicas),
                plan.workload.desired_replicas,
            )
        }
    }
}

fn workload_identity_matches(
    plan: &ComponentPlanContract,
    annotations: Option<&BTreeMap<String, String>>,
    pod_annotations: Option<&BTreeMap<String, String>>,
) -> bool {
    annotations.and_then(|values| values.get(BACKEND_ID_ANNOTATION)) == Some(&plan.backend_id)
        && annotations.and_then(|values| values.get(REVISION_DIGEST_ANNOTATION))
            == Some(&plan.revision_digest)
        && pod_annotations.and_then(|values| values.get(ROLLOUT_DIGEST_ANNOTATION))
            == Some(&plan.rollout_digest)
}

fn workload_replica_observation(
    generation: Option<i64>,
    observed_generation: Option<i64>,
    ready_replicas: Option<i32>,
    desired_replicas: u16,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    if generation.is_none() || observed_generation != generation {
        return workload_unavailable("owning workload has not observed its current generation");
    }
    if ready_replicas.unwrap_or_default() >= i32::from(desired_replicas) {
        (
            ComponentConditionState::True,
            ComponentConditionReason::WorkloadAvailable,
            "owning workload has the required ready replicas",
        )
    } else {
        workload_unavailable("owning workload does not have the required ready replicas")
    }
}

const fn workload_unavailable(
    message: &'static str,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    (
        ComponentConditionState::False,
        ComponentConditionReason::WorkloadUnavailable,
        message,
    )
}

const fn stale_workload() -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    (
        ComponentConditionState::Unknown,
        ComponentConditionReason::StaleRevision,
        "owning workload does not match the accepted plan identity",
    )
}

fn protocol_observation(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
    prober_digest: &str,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    let Some(lease) = resources.deployments.iter().find_map(|deployment| {
        if deployment.metadata.name.as_deref() != Some(PROTOCOL_PROBER_NAME)
            || deployment.spec.as_ref().and_then(|spec| spec.replicas) != Some(1)
            || deployment
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(PROTOCOL_PROBER_DIGEST_ANNOTATION))
                .is_none_or(|digest| digest != prober_digest)
        {
            return None;
        }
        let metadata_lease = deployment
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(PROTOCOL_PROBER_LEASE_ANNOTATION))?;
        let template_lease = deployment
            .spec
            .as_ref()?
            .template
            .metadata
            .as_ref()?
            .annotations
            .as_ref()?
            .get(PROTOCOL_PROBER_LEASE_ANNOTATION)?;
        (metadata_lease == template_lease && metadata_lease != INACTIVE_PROBER_LEASE)
            .then_some(metadata_lease.as_str())
    }) else {
        return (
            ComponentConditionState::Unknown,
            ComponentConditionReason::ProtocolProbePending,
            "protocol probing is waiting for a current scheduler lease",
        );
    };
    let Some(pod) = resources.pods.iter().find(|pod| {
        pod.metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PROTOCOL_PROBER_LABEL))
            .is_some_and(|value| value == "true")
            && pod
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(PROTOCOL_PROBER_DIGEST_ANNOTATION))
                .is_some_and(|digest| digest == prober_digest)
            && pod
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(PROTOCOL_PROBER_LEASE_ANNOTATION))
                .is_some_and(|pod_lease| pod_lease == lease)
    }) else {
        return (
            ComponentConditionState::Unknown,
            ComponentConditionReason::ProtocolProbePending,
            "current protocol prober Pod has not been observed",
        );
    };
    let container_name = protocol_probe_container_name(&plan.component_id);
    let status = pod
        .status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref())
        .and_then(|statuses| statuses.iter().find(|status| status.name == container_name));
    match status {
        Some(status) if status.ready => (
            ComponentConditionState::True,
            ComponentConditionReason::ProtocolResponding,
            "bounded Service-DNS protocol probe is responding",
        ),
        Some(_) => (
            ComponentConditionState::False,
            ComponentConditionReason::ProtocolProbeFailed,
            "bounded Service-DNS protocol probe is not responding",
        ),
        None => (
            ComponentConditionState::Unknown,
            ComponentConditionReason::ProtocolProbePending,
            "protocol probe container has not reported status",
        ),
    }
}

fn storage_observation(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    let claims = plan.storage.iter().map(|required| {
        resources
            .persistent_volume_claims
            .iter()
            .find(|claim| claim.metadata.name.as_deref() == Some(&required.claim_name))
    });
    let mut saw_claim = false;
    for claim in claims {
        let Some(claim) = claim else {
            return not_observed("required persistent storage has not been observed");
        };
        saw_claim = true;
        if claim
            .status
            .as_ref()
            .and_then(|status| status.phase.as_deref())
            != Some("Bound")
        {
            return (
                ComponentConditionState::False,
                ComponentConditionReason::StoragePending,
                "required persistent storage is not bound",
            );
        }
    }
    if saw_claim {
        (
            ComponentConditionState::True,
            ComponentConditionReason::StorageBound,
            "all required persistent storage is bound",
        )
    } else {
        not_observed("backend declared no persistent storage observation")
    }
}

fn credential_observation(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    let Some(pod_spec) = observed_pod_spec(plan, resources) else {
        return not_observed("credential projection workload has not been observed");
    };
    for requirement in &plan.credentials {
        let claim_bound = resources.persistent_volume_claims.iter().any(|claim| {
            claim.metadata.name.as_deref() == Some(&requirement.claim_name)
                && claim
                    .status
                    .as_ref()
                    .and_then(|status| status.phase.as_deref())
                    == Some("Bound")
        });
        let source_identity_matches =
            observed_state_contract(&requirement.source_component_id, resources)
                == Some(requirement.source_state_contract.as_str());
        let volume_matches = pod_spec.volumes.as_ref().is_some_and(|volumes| {
            volumes.iter().any(|volume| {
                volume.name == requirement.mount_name
                    && volume
                        .persistent_volume_claim
                        .as_ref()
                        .is_some_and(|claim| claim.claim_name == requirement.claim_name)
            })
        });
        let mount_matches = pod_spec.containers.iter().any(|container| {
            container.volume_mounts.as_ref().is_some_and(|mounts| {
                mounts.iter().any(|mount| {
                    mount.name == requirement.mount_name
                        && mount.mount_path == requirement.mount_path
                        && mount.read_only.unwrap_or(false) == requirement.read_only
                })
            })
        });
        if !claim_bound || !source_identity_matches || !volume_matches || !mount_matches {
            return (
                ComponentConditionState::False,
                ComponentConditionReason::CredentialsMissing,
                "required credential projection is absent, stale, or unavailable",
            );
        }
    }
    if plan.credentials.is_empty() {
        not_observed("backend declared no credential projection requirement")
    } else {
        (
            ComponentConditionState::True,
            ComponentConditionReason::CredentialsProjected,
            "all required credential projections match the accepted plan",
        )
    }
}

fn observed_pod_spec<'a>(
    plan: &ComponentPlanContract,
    resources: &'a ComponentObservationResources<'_>,
) -> Option<&'a k8s_openapi::api::core::v1::PodSpec> {
    match plan.workload.kind {
        WorkloadControllerKind::Deployment => resources
            .deployments
            .iter()
            .find(|workload| workload.metadata.name.as_deref() == Some(&plan.workload.name))
            .and_then(|workload| workload.spec.as_ref())
            .and_then(|spec| spec.template.spec.as_ref()),
        WorkloadControllerKind::StatefulSet => resources
            .stateful_sets
            .iter()
            .find(|workload| workload.metadata.name.as_deref() == Some(&plan.workload.name))
            .and_then(|workload| workload.spec.as_ref())
            .and_then(|spec| spec.template.spec.as_ref()),
    }
}

fn observed_state_contract<'a>(
    component_id: &str,
    resources: &'a ComponentObservationResources<'_>,
) -> Option<&'a str> {
    resources
        .deployments
        .iter()
        .find(|workload| workload.metadata.name.as_deref() == Some(component_id))
        .and_then(|workload| workload.metadata.annotations.as_ref())
        .and_then(|annotations| annotations.get(EXECUTION_STATE_CONTRACT_ANNOTATION))
        .or_else(|| {
            resources
                .stateful_sets
                .iter()
                .find(|workload| workload.metadata.name.as_deref() == Some(component_id))
                .and_then(|workload| workload.metadata.annotations.as_ref())
                .and_then(|annotations| annotations.get(EXECUTION_STATE_CONTRACT_ANNOTATION))
        })
        .map(String::as_str)
}

fn service_observation(
    plan: &ComponentPlanContract,
    resources: &ComponentObservationResources<'_>,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    let Some(service) = resources
        .services
        .iter()
        .find(|service| service.metadata.name.as_deref() == Some(&plan.component_id))
    else {
        return not_observed("required Service has not been observed");
    };
    let service_ports = service
        .spec
        .as_ref()
        .and_then(|spec| spec.ports.as_deref())
        .unwrap_or_default();
    let service_matches = plan.target_descriptor.ports.iter().all(|(name, port)| {
        let expected_name = name.replace('_', "-");
        service_ports.iter().any(|candidate| {
            candidate.name.as_deref() == Some(&expected_name) && candidate.port == i32::from(*port)
        })
    });
    if !service_matches {
        return (
            ComponentConditionState::False,
            ComponentConditionReason::EndpointsMissing,
            "Service does not publish every required port",
        );
    }
    let endpoints_ready = resources.endpoint_slices.iter().any(|slice| {
        slice
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kubernetes.io/service-name"))
            == Some(&plan.component_id)
            && endpoint_slice_ports_match(plan, slice)
            && slice.endpoints.iter().any(|endpoint| {
                !endpoint.addresses.is_empty()
                    && endpoint
                        .conditions
                        .as_ref()
                        .and_then(|conditions| conditions.ready)
                        != Some(false)
            })
    });
    if endpoints_ready {
        (
            ComponentConditionState::True,
            ComponentConditionReason::EndpointsReady,
            "Service has a ready endpoint for every required port",
        )
    } else {
        (
            ComponentConditionState::False,
            ComponentConditionReason::EndpointsMissing,
            "Service has no ready EndpointSlice with every required port",
        )
    }
}

fn endpoint_slice_ports_match(plan: &ComponentPlanContract, slice: &EndpointSlice) -> bool {
    plan.target_descriptor.ports.iter().all(|(name, port)| {
        let expected_name = name.replace('_', "-");
        slice.ports.as_ref().is_some_and(|ports| {
            ports.iter().any(|candidate| {
                candidate.name.as_deref() == Some(&expected_name)
                    && candidate.port == Some(i32::from(*port))
            })
        })
    })
}

const fn not_observed(
    message: &'static str,
) -> (
    ComponentConditionState,
    ComponentConditionReason,
    &'static str,
) {
    (
        ComponentConditionState::Unknown,
        ComponentConditionReason::NotObserved,
        message,
    )
}

fn component_condition(
    condition_type: ComponentConditionType,
    state: ComponentConditionState,
    reason: ComponentConditionReason,
    message: &str,
    last_transition_unix: i64,
) -> ComponentCondition {
    ComponentCondition {
        condition_type,
        state,
        reason,
        message: bounded_condition_message(message),
        last_transition_unix,
    }
}

fn bounded_condition_message(message: &str) -> String {
    let mut bounded = String::with_capacity(message.len().min(MAX_CONDITION_MESSAGE_BYTES));
    for character in message.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len() + character.len_utf8() > MAX_CONDITION_MESSAGE_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn aggregate_condition_state(
    required: &BTreeSet<ComponentConditionType>,
    conditions: &[ComponentCondition],
) -> ComponentConditionState {
    let states = required.iter().map(|required| {
        conditions
            .iter()
            .find(|condition| condition.condition_type == *required)
            .map_or(ComponentConditionState::Unknown, |condition| {
                condition.state
            })
    });
    let mut saw_unknown = false;
    for state in states {
        match state {
            ComponentConditionState::False => return ComponentConditionState::False,
            ComponentConditionState::Unknown => saw_unknown = true,
            ComponentConditionState::True => {}
        }
    }
    if saw_unknown {
        ComponentConditionState::Unknown
    } else {
        ComponentConditionState::True
    }
}

fn preserve_condition_transitions(
    current: &mut [ComponentCondition],
    previous: &[ComponentCondition],
) {
    for condition in current {
        if let Some(previous) = previous.iter().find(|candidate| {
            candidate.condition_type == condition.condition_type
                && candidate.state == condition.state
                && candidate.reason == condition.reason
                && candidate.message == condition.message
        }) {
            condition.last_transition_unix = previous.last_transition_unix;
        }
    }
}

/// Render Bitcoin Core resources from a compiled plan without cluster I/O.
///
/// # Errors
///
/// Returns an error if the plan is not a Bitcoin Core plan or if its typed
/// configuration cannot be rendered into the fixed resource contract.
pub fn render_bitcoin_component(
    plan: &ComponentPlanContract,
) -> Result<RenderedComponent, AdapterError> {
    if plan.backend_id != "bitcoin-core" || plan.kind != ComponentKind::Bitcoin {
        return Err(AdapterError::InvalidPlan(format!(
            "backend {:?} with kind {:?} is not bitcoin-core",
            plan.backend_id, plan.kind
        )));
    }
    let namespace = instance_namespace(&plan.instance_key);
    let txindex = plan
        .effective_config
        .get("txindex")
        .and_then(Value::as_bool)
        .ok_or_else(|| AdapterError::InvalidPlan("bitcoin txindex is not boolean".into()))?;
    let fallback_fee = plan
        .effective_config
        .get("fallback_fee")
        .and_then(Value::as_f64)
        .ok_or_else(|| AdapterError::InvalidPlan("bitcoin fallback_fee is not numeric".into()))?;
    let args = vec![
        "-regtest".to_owned(),
        "-datadir=/home/bitcoin/.bitcoin".to_owned(),
        "-server=1".to_owned(),
        "-rpcbind=0.0.0.0".to_owned(),
        "-rpcallowip=0.0.0.0/0".to_owned(),
        format!("-rpcuser={RPC_USER}"),
        format!("-rpcpassword={RPC_PASSWORD}"),
        "-rpcport=18443".to_owned(),
        format!("-txindex={}", u8::from(txindex)),
        "-zmqpubrawblock=tcp://0.0.0.0:28334".to_owned(),
        "-zmqpubrawtx=tcp://0.0.0.0:28335".to_owned(),
        format!("-fallbackfee={fallback_fee}"),
        "-debug=0".to_owned(),
    ];
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.stateful_sets.push(resource(stateful_set(
        &plan.instance_key,
        &namespace,
        &plan.component_id,
        &plan.execution_context.image,
        Some(vec!["bitcoind".to_owned()]),
        &args,
        "/home/bitcoin/.bitcoin",
        &json!({
            "exec": {"command": ["bitcoin-cli", "-regtest", format!("-rpcuser={RPC_USER}"), format!("-rpcpassword={RPC_PASSWORD}"), "getblockchaininfo"]}
        }),
        Some(plan),
    ))?);
    Ok(rendered)
}

/// Render LND resources from a compiled plan without cluster I/O.
///
/// # Errors
///
/// Returns an error when the plan has the wrong backend, lacks its chain link,
/// or contains invalid effective configuration.
pub fn render_lnd_component(
    plan: &ComponentPlanContract,
) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, "lnd", ComponentKind::Lightning)?;
    let chain = plan_linked_target(plan, LinkKind::ChainBackend)?;
    let chain_rpc = target_port(chain, "rpc")?;
    let chain_zmq_block = target_port(chain, "zmq_block")?;
    let chain_zmq_tx = target_port(chain, "zmq_tx")?;
    let alias = plan
        .effective_config
        .get("alias")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::InvalidPlan("lnd alias is not a string".into()))?;
    let args = vec![
        "--lnddir=/home/lnd/.lnd".to_owned(),
        "--noseedbackup".to_owned(),
        format!("--alias={alias}"),
        format!("--externalip={}", plan.component_id),
        "--listen=0.0.0.0:9735".to_owned(),
        "--rpclisten=0.0.0.0:10009".to_owned(),
        "--restlisten=0.0.0.0:8080".to_owned(),
        "--bitcoin.active".to_owned(),
        "--bitcoin.regtest".to_owned(),
        "--bitcoin.node=bitcoind".to_owned(),
        format!("--bitcoind.rpchost={}:{chain_rpc}", chain.component_id),
        format!("--bitcoind.rpcuser={RPC_USER}"),
        format!("--bitcoind.rpcpass={RPC_PASSWORD}"),
        format!(
            "--bitcoind.zmqpubrawblock=tcp://{}:{chain_zmq_block}",
            chain.component_id
        ),
        format!(
            "--bitcoind.zmqpubrawtx=tcp://{}:{chain_zmq_tx}",
            chain.component_id
        ),
        format!("--tlsextradomain={}", plan.component_id),
        "--accept-keysend".to_owned(),
        "--debuglevel=info".to_owned(),
    ];
    let namespace = instance_namespace(&plan.instance_key);
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.stateful_sets.push(resource(stateful_set(
        &plan.instance_key,
        &namespace,
        &plan.component_id,
        &plan.execution_context.image,
        Some(vec!["lnd".to_owned()]),
        &args,
        "/home/lnd/.lnd",
        &json!({
            "exec": {"command": ["lncli", "--lnddir=/home/lnd/.lnd", "--network=regtest", "getinfo"]}
        }),
        Some(plan),
    ))?);
    Ok(rendered)
}

/// Render Core Lightning resources from a compiled plan without cluster I/O.
///
/// # Errors
///
/// Returns an error when the plan has the wrong backend, lacks its chain link,
/// or contains invalid effective configuration.
pub fn render_cln_component(
    plan: &ComponentPlanContract,
) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, "cln", ComponentKind::Lightning)?;
    let chain = plan_linked_target(plan, LinkKind::ChainBackend)?;
    let chain_rpc = target_port(chain, "rpc")?;
    let alias = plan
        .effective_config
        .get("alias")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::InvalidPlan("cln alias is not a string".into()))?;
    let args = vec![
        "--lightning-dir=/home/cln/.lightning".to_owned(),
        "--network=regtest".to_owned(),
        format!("--alias={alias}"),
        "--developer".to_owned(),
        "--dev-no-reconnect".to_owned(),
        "--autoconnect-seeker-peers=0".to_owned(),
        "--bind-addr=0.0.0.0:9735".to_owned(),
        format!("--announce-addr={}:9735", plan.component_id),
        format!("--bitcoin-rpcconnect={}", chain.component_id),
        format!("--bitcoin-rpcport={chain_rpc}"),
        format!("--bitcoin-rpcuser={RPC_USER}"),
        format!("--bitcoin-rpcpassword={RPC_PASSWORD}"),
        "--bitcoin-retry-timeout=60".to_owned(),
        "--log-level=info".to_owned(),
    ];
    let namespace = instance_namespace(&plan.instance_key);
    let mut rendered = RenderedComponent::default();
    rendered.services.push(resource(service_from_plan(plan))?);
    rendered.stateful_sets.push(resource(stateful_set(
        &plan.instance_key,
        &namespace,
        &plan.component_id,
        &plan.execution_context.image,
        Some(vec!["lightningd".to_owned()]),
        &args,
        "/home/cln/.lightning",
        &json!({
            "exec": {"command": ["lightning-cli", "--lightning-dir=/home/cln/.lightning", "--network=regtest", "getinfo"]}
        }),
        Some(plan),
    ))?);
    Ok(rendered)
}

/// Render CDK mint resources from a compiled plan without cluster I/O.
///
/// # Errors
///
/// Returns an error when the plan has the wrong backend, lacks its Lightning
/// dependency, or contains an incomplete configuration or service contract.
pub fn render_cdk_component(
    plan: &ComponentPlanContract,
) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, "cdk", ComponentKind::Mint)?;
    let lightning = plan_linked_target(plan, LinkKind::LightningBackend)?;
    let lightning_rpc = target_port(lightning, "rpc")?;
    let http_port = plan
        .target_descriptor
        .ports
        .get("http")
        .copied()
        .ok_or_else(|| AdapterError::InvalidPlan("cdk has no HTTP service port".into()))?;
    let name = plan
        .effective_config
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::InvalidPlan("cdk name is not a string".into()))?;
    let description = plan
        .effective_config
        .get("description")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::InvalidPlan("cdk description is not a string".into()))?;
    let namespace = instance_namespace(&plan.instance_key);
    let config_name = format!("{}-config", plan.component_id);
    let data_name = format!("{}-data", plan.component_id);
    let config = mint_config(
        &plan.component_id,
        http_port,
        &lightning.component_id,
        lightning_rpc,
        name,
        description,
    );
    let mut rendered = RenderedComponent::default();
    rendered.config_maps.push(resource(json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": metadata(&config_name, &plan.instance_key, &namespace, Some(&plan.component_id)),
        "data": {"config.toml": config}
    }))?);
    rendered.persistent_volume_claims.push(resource(json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": metadata(&data_name, &plan.instance_key, &namespace, Some(&plan.component_id)),
        "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}
    }))?);
    rendered.services.push(resource(service_from_plan(plan))?);
    let labels = labels(&plan.instance_key, Some(&plan.component_id));
    rendered.deployments.push(resource(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": plan_workload_metadata(plan),
        "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
            "metadata": plan_pod_metadata(plan, &labels), "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(&plan.instance_key), "containers": [{
                    "name": "component", "image": plan.execution_context.image, "imagePullPolicy": "IfNotPresent",
                    "command": ["cdk-mintd"], "args": ["--config", "/config/config.toml"],
                    "env": [{"name": "CDK_MINTD_WORK_DIR", "value": "/app/data"}],
                    "ports": [{"name": "http", "containerPort": http_port}],
                    "securityContext": container_security(),
                    "readinessProbe": {"httpGet": {"path": "/v1/info", "port": http_port}, "periodSeconds": 3, "failureThreshold": 40},
                    "volumeMounts": [
                        {"name": "config", "mountPath": "/config", "readOnly": true},
                        {"name": "data", "mountPath": "/app/data"},
                        {"name": "lnd", "mountPath": "/lnd", "readOnly": true}
                    ]
                }], "volumes": [
                    {"name": "config", "configMap": {"name": config_name}},
                    {"name": "data", "persistentVolumeClaim": {"claimName": data_name}},
                    {"name": "lnd", "persistentVolumeClaim": {"claimName": format!("data-{}-0", lightning.component_id)}}
                ]
            }
        }}
    }))?);
    Ok(rendered)
}

/// Render a disposable attacker workspace from a compiled plan without
/// cluster I/O.
///
/// # Errors
///
/// Returns an error when the plan does not select the attacker backend.
pub fn render_attacker_component(
    plan: &ComponentPlanContract,
) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, "attacker-workspace", ComponentKind::Attacker)?;
    let labels = labels(&plan.instance_key, Some(&plan.component_id));
    let mut rendered = RenderedComponent::default();
    rendered.deployments.push(resource(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": plan_workload_metadata(plan),
        "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
            "metadata": plan_pod_metadata(plan, &labels), "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(&plan.instance_key), "containers": [{
                    "name": "component", "image": plan.execution_context.image, "imagePullPolicy": "IfNotPresent",
                    "command": ["sh", "-c", "trap : TERM INT; sleep infinity & wait"],
                    "securityContext": container_security()
                }]
            }
        }}
    }))?);
    Ok(rendered)
}

/// Render a persistent Nutshell wallet workspace from a compiled plan without
/// cluster I/O.
///
/// # Errors
///
/// Returns an error when the plan does not select the Nutshell wallet backend.
pub fn render_wallet_component(
    plan: &ComponentPlanContract,
) -> Result<RenderedComponent, AdapterError> {
    require_plan_backend(plan, "nutshell-wallet", ComponentKind::Wallet)?;
    let namespace = instance_namespace(&plan.instance_key);
    let data_name = format!("{}-data", plan.component_id);
    let mut rendered = RenderedComponent::default();
    rendered.persistent_volume_claims.push(resource(json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": metadata(&data_name, &plan.instance_key, &namespace, Some(&plan.component_id)),
        "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}
    }))?);
    let labels = labels(&plan.instance_key, Some(&plan.component_id));
    rendered.deployments.push(resource(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": plan_workload_metadata(plan),
        "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
            "metadata": plan_pod_metadata(plan, &labels), "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(&plan.instance_key), "containers": [{
                    "name": "component", "image": plan.execution_context.image, "imagePullPolicy": "IfNotPresent",
                    "command": ["/bin/sh", "-c", "trap 'exit 0' TERM INT; while :; do sleep 3600; done"],
                    "env": [{"name": "HOME", "value": "/wallet"}, {"name": "PROOFSTORM_WALLET", "value": plan.component_id}],
                    "securityContext": container_security(),
                    "volumeMounts": [{"name": "wallet", "mountPath": "/wallet"}]
                }], "volumes": [{"name": "wallet", "persistentVolumeClaim": {"claimName": data_name}}]
            }
        }}
    }))?);
    Ok(rendered)
}

#[allow(
    clippy::too_many_arguments,
    reason = "uniform adapter resource contract"
)]
fn stateful_set(
    instance_key: &str,
    namespace: &str,
    component: &str,
    image: &str,
    command: Option<Vec<String>>,
    args: &[String],
    data_mount: &str,
    readiness_probe: &Value,
    plan_identity: Option<&ComponentPlanContract>,
) -> Value {
    let labels = labels(instance_key, Some(component));
    let mut container = json!({
        "name": "component", "image": image, "imagePullPolicy": "IfNotPresent", "args": args,
        "securityContext": container_security(),
        "readinessProbe": {"periodSeconds": 3, "failureThreshold": 40},
        "volumeMounts": [{"name": "data", "mountPath": data_mount}]
    });
    container["readinessProbe"]
        .as_object_mut()
        .expect("probe object")
        .extend(readiness_probe.as_object().expect("probe object").clone());
    if let Some(command) = command {
        container["command"] = json!(command);
    }
    let template_metadata = json!({
        "labels": labels,
        "annotations": plan_identity.map(rollout_annotations)
    });
    let mut workload_metadata = metadata(component, instance_key, namespace, Some(component));
    workload_metadata["annotations"] = json!(plan_identity.map(revision_annotations));
    json!({
        "apiVersion": "apps/v1", "kind": "StatefulSet",
        "metadata": workload_metadata,
        "spec": {"serviceName": component, "replicas": 1, "selector": {"matchLabels": labels},
            "template": {"metadata": template_metadata, "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(instance_key), "containers": [container]
            }},
            "volumeClaimTemplates": [{"metadata": {"name": "data", "labels": labels},
                "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}}]
        }
    })
}

fn service_from_plan(plan: &ComponentPlanContract) -> Value {
    let namespace = instance_namespace(&plan.instance_key);
    let ports = plan
        .target_descriptor
        .ports
        .iter()
        .map(|(name, port)| {
            let service_name = name.replace('_', "-");
            json!({"name": service_name, "port": port, "targetPort": port})
        })
        .collect::<Vec<_>>();
    json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": metadata(
            &plan.component_id,
            &plan.instance_key,
            &namespace,
            Some(&plan.component_id),
        ),
        "spec": {
            "selector": labels(&plan.instance_key, Some(&plan.component_id)),
            "ports": ports,
        }
    })
}

fn action_network_policy(instance_key: &str, namespace: &str) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
        "metadata": metadata("allow-controller-actions", instance_key, namespace, None),
        "spec": {"podSelector": {"matchExpressions": [
                {"key": "proofstorm.dev/operation", "operator": "Exists"}
            ]},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [{"podSelector": {"matchLabels": {INSTANCE_LABEL: instance_key}}}]}],
            "egress": [
                {"to": [{"podSelector": {"matchLabels": {INSTANCE_LABEL: instance_key}}}]},
                {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
                    "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}
            ]}
    })
}

/// Render the complete allow-list for one protocol component.
///
/// Components in `excluded_components` are removed from both ingress and
/// egress peers. Controller action Pods remain reachable because they carry no
/// component label and are governed by their own policy.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_component_network_policy(
    instance_key: &str,
    component: &str,
    excluded_components: &[String],
) -> Result<NetworkPolicy, AdapterError> {
    let namespace = instance_namespace(instance_key);
    let mut peer_selector = json!({"matchLabels": {INSTANCE_LABEL: instance_key}});
    if !excluded_components.is_empty() {
        peer_selector["matchExpressions"] = json!([{
            "key": NETWORK_IDENTITY_LABEL,
            "operator": "NotIn",
            "values": excluded_components
        }]);
    }
    resource(json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
        "metadata": metadata(component, instance_key, &namespace, Some(component)),
        "spec": {"podSelector": {"matchLabels": {NETWORK_IDENTITY_LABEL: component}},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [{"podSelector": peer_selector.clone()}]}],
            "egress": [
                {"to": [{"podSelector": peer_selector}]},
                {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
                    "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}
            ]}
    }))
}

fn require_plan_backend(
    plan: &ComponentPlanContract,
    backend: &str,
    kind: ComponentKind,
) -> Result<(), AdapterError> {
    if plan.backend_id == backend && plan.kind == kind {
        return Ok(());
    }
    Err(AdapterError::InvalidPlan(format!(
        "backend {:?} with kind {:?} is not {backend}",
        plan.backend_id, plan.kind
    )))
}

fn plan_linked_target(
    plan: &ComponentPlanContract,
    kind: LinkKind,
) -> Result<&TargetDescriptorContract, AdapterError> {
    let link = plan
        .relevant_links
        .iter()
        .find(|link| link.kind == kind && link.from == plan.component_id)
        .ok_or_else(|| AdapterError::MissingLink {
            component: plan.component_id.clone(),
            link: kind,
        })?;
    plan.linked_targets
        .get(&link.to)
        .ok_or_else(|| AdapterError::MissingTarget {
            component: plan.component_id.clone(),
            target: link.to.clone(),
        })
}

fn target_port(target: &TargetDescriptorContract, name: &str) -> Result<u16, AdapterError> {
    target.ports.get(name).copied().ok_or_else(|| {
        AdapterError::InvalidPlan(format!(
            "target {:?} has no {name:?} service port",
            target.component_id
        ))
    })
}

fn mint_config(
    component: &str,
    http_port: u16,
    lightning: &str,
    lightning_rpc: u16,
    name: &str,
    description: &str,
) -> String {
    format!(
        "[info]\nurl = \"http://{component}:{http_port}\"\nlisten_host = \"0.0.0.0\"\nlisten_port = {http_port}\nmnemonic = \"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about\"\ninput_fee_ppk = 100\n\n[mint_info]\nname = {name:?}\ndescription = {description:?}\nurls = [\"http://{component}:{http_port}\"]\n\n[ln]\nln_backend = \"lnd\"\nunit = \"sat\"\nmin_mint = 1\nmax_mint = 500000\nmin_melt = 1\nmax_melt = 500000\n\n[lnd]\naddress = \"https://{lightning}:{lightning_rpc}\"\ncert_file = \"/lnd/tls.cert\"\nmacaroon_file = \"/lnd/data/chain/bitcoin/regtest/admin.macaroon\"\n\n[database]\nengine = \"sqlite\"\n"
    )
}

fn metadata(name: &str, instance_key: &str, namespace: &str, component: Option<&str>) -> Value {
    json!({"name": name, "namespace": namespace, "labels": labels(instance_key, component)})
}

fn labels(instance_key: &str, component: Option<&str>) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        (INSTANCE_LABEL.to_owned(), instance_key.to_owned()),
        (
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstormd".to_owned(),
        ),
    ]);
    if let Some(component) = component {
        labels.insert(COMPONENT_LABEL.to_owned(), component.to_owned());
        labels.insert(NETWORK_IDENTITY_LABEL.to_owned(), component.to_owned());
    }
    labels
}

fn revision_annotations(plan: &ComponentPlanContract) -> BTreeMap<String, String> {
    BTreeMap::from([
        (BACKEND_ID_ANNOTATION.to_owned(), plan.backend_id.clone()),
        (
            EXECUTION_STATE_CONTRACT_ANNOTATION.to_owned(),
            plan.execution_context.state_contract.clone(),
        ),
        (
            REVISION_DIGEST_ANNOTATION.to_owned(),
            plan.revision_digest.clone(),
        ),
    ])
}

fn rollout_annotations(plan: &ComponentPlanContract) -> BTreeMap<String, String> {
    BTreeMap::from([(
        ROLLOUT_DIGEST_ANNOTATION.to_owned(),
        plan.rollout_digest.clone(),
    )])
}

fn plan_workload_metadata(plan: &ComponentPlanContract) -> Value {
    let namespace = instance_namespace(&plan.instance_key);
    let mut workload = metadata(
        &plan.component_id,
        &plan.instance_key,
        &namespace,
        Some(&plan.component_id),
    );
    workload["annotations"] = json!(revision_annotations(plan));
    workload
}

fn plan_pod_metadata(plan: &ComponentPlanContract, labels: &BTreeMap<String, String>) -> Value {
    json!({"labels": labels, "annotations": rollout_annotations(plan)})
}

fn pod_security() -> Value {
    json!({"runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000, "fsGroup": 1000,
        "seccompProfile": {"type": "RuntimeDefault"}})
}

fn instance_affinity(instance_key: &str) -> Value {
    json!({"podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
        "labelSelector": {"matchLabels": {INSTANCE_LABEL: instance_key}},
        "topologyKey": "kubernetes.io/hostname"
    }]}})
}

fn container_security() -> Value {
    json!({"allowPrivilegeEscalation": false, "capabilities": {"drop": ["ALL"]}})
}

fn resource<T: DeserializeOwned>(value: Value) -> Result<T, AdapterError> {
    Ok(serde_json::from_value(value)?)
}

fn append_inventory<T>(
    output: &mut Vec<InventoryEntry>,
    api_version: &str,
    kind: &str,
    resources: &[T],
) where
    T: k8s_openapi::Metadata<Ty = k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta>,
{
    output.extend(resources.iter().map(|resource| InventoryEntry {
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        namespace: resource.metadata().namespace.clone().unwrap_or_default(),
        name: resource.metadata().name.clone().unwrap_or_default(),
    }));
}

fn sort_by_name<T>(resources: &mut [T])
where
    T: k8s_openapi::Metadata<Ty = k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta>,
{
    resources.sort_by(|left, right| left.metadata().name.cmp(&right.metadata().name));
}

#[must_use]
pub fn component_ports(component: &ComponentSpec) -> BTreeMap<String, u16> {
    match component.kind {
        ComponentKind::Bitcoin => BTreeMap::from([
            ("p2p".into(), 18_444),
            ("rpc".into(), 18_443),
            ("zmq_block".into(), 28_334),
            ("zmq_tx".into(), 28_335),
        ]),
        ComponentKind::Lightning if component.implementation == "cln" => {
            BTreeMap::from([("p2p".into(), 9_735)])
        }
        ComponentKind::Lightning => BTreeMap::from([
            ("p2p".into(), 9_735),
            ("rest".into(), 8_080),
            ("rpc".into(), 10_009),
        ]),
        ComponentKind::Mint => BTreeMap::from([("http".into(), 3_338)]),
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use proofstorm_core::{
        API_VERSION, ComponentSpec, ControlClass, LabPolicy, LinkSpec, default_catalog,
        resolve_lock,
    };

    use super::*;

    fn component(
        id: &str,
        kind: ComponentKind,
        implementation: &str,
        control: ControlClass,
    ) -> ComponentSpec {
        ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: "v1alpha1".into(),
            control,
            config: BTreeMap::new(),
        }
    }

    type LightningRenderer = fn(&ComponentPlanContract) -> Result<RenderedComponent, AdapterError>;

    fn lightning_lab() -> LabSpec {
        LabSpec {
            api_version: API_VERSION.into(),
            name: "lightning-plans".into(),
            components: vec![
                component(
                    "chain-a",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component(
                    "chain-b",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component(
                    "alice",
                    ComponentKind::Lightning,
                    "lnd",
                    ControlClass::Laboratory,
                ),
                component(
                    "bob",
                    ComponentKind::Lightning,
                    "cln",
                    ControlClass::Laboratory,
                ),
            ],
            links: vec![
                LinkSpec {
                    kind: LinkKind::ChainBackend,
                    from: "alice".into(),
                    to: "chain-a".into(),
                },
                LinkSpec {
                    kind: LinkKind::ChainBackend,
                    from: "bob".into(),
                    to: "chain-a".into(),
                },
            ],
            policy: LabPolicy::default(),
        }
    }

    fn cdk_lab() -> LabSpec {
        LabSpec {
            api_version: API_VERSION.into(),
            name: "cdk-plan".into(),
            components: vec![
                component(
                    "mint-lnd-a",
                    ComponentKind::Lightning,
                    "lnd",
                    ControlClass::Laboratory,
                ),
                component(
                    "mint-lnd-b",
                    ComponentKind::Lightning,
                    "lnd",
                    ControlClass::Laboratory,
                ),
                component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
            ],
            links: vec![LinkSpec {
                kind: LinkKind::LightningBackend,
                from: "mint".into(),
                to: "mint-lnd-a".into(),
            }],
            policy: LabPolicy::default(),
        }
    }

    fn workspace_lab() -> LabSpec {
        LabSpec {
            api_version: API_VERSION.into(),
            name: "workspace-plans".into(),
            components: vec![
                component(
                    "wallet-a",
                    ComponentKind::Wallet,
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                ),
                component(
                    "wallet-b",
                    ComponentKind::Wallet,
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                ),
                component(
                    "attacker",
                    ComponentKind::Attacker,
                    "attacker-workspace",
                    ControlClass::Attacker,
                ),
            ],
            links: vec![],
            policy: LabPolicy::default(),
        }
    }

    fn available_deployment(mut workload: Deployment) -> Deployment {
        workload.metadata.generation = Some(1);
        workload.status = Some(k8s_openapi::api::apps::v1::DeploymentStatus {
            observed_generation: Some(1),
            available_replicas: Some(1),
            ..Default::default()
        });
        workload
    }

    fn set_deployment_rollout(workload: &mut Deployment, rollout_digest: &str) {
        workload
            .spec
            .as_mut()
            .expect("Deployment spec")
            .template
            .metadata
            .as_mut()
            .expect("Pod metadata")
            .annotations
            .as_mut()
            .expect("Pod annotations")
            .insert(
                ROLLOUT_DIGEST_ANNOTATION.to_owned(),
                rollout_digest.to_owned(),
            );
    }

    fn status_condition(
        status: &ComponentStatus,
        condition_type: ComponentConditionType,
    ) -> &ComponentCondition {
        status
            .conditions
            .iter()
            .find(|condition| condition.condition_type == condition_type)
            .expect("component condition")
    }

    struct ChainObservationFixture {
        plans: Vec<ComponentPlanContract>,
        workload: StatefulSet,
        claim: PersistentVolumeClaim,
        service: Service,
        endpoint_slice: EndpointSlice,
    }

    fn chain_observation_fixture() -> ChainObservationFixture {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "observed-chain".into(),
            components: vec![component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            )],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("plans");
        let rendered = render_bitcoin_component(&plans[0]).expect("render chain");
        let mut workload = rendered.stateful_sets[0].clone();
        workload.metadata.generation = Some(2);
        workload.status = Some(k8s_openapi::api::apps::v1::StatefulSetStatus {
            observed_generation: Some(1),
            ready_replicas: Some(1),
            replicas: 1,
            ..Default::default()
        });
        let mut claim = PersistentVolumeClaim::default();
        claim.metadata.name = Some("data-chain-0".into());
        claim.status = Some(k8s_openapi::api::core::v1::PersistentVolumeClaimStatus {
            phase: Some("Pending".into()),
            ..Default::default()
        });
        let endpoint_ports = plans[0]
            .target_descriptor
            .ports
            .iter()
            .map(|(name, port)| {
                json!({"name": name.replace('_', "-"), "port": port, "protocol": "TCP"})
            })
            .collect::<Vec<_>>();
        let endpoint_slice = resource(json!({
            "apiVersion": "discovery.k8s.io/v1",
            "kind": "EndpointSlice",
            "metadata": {
                "name": "chain-abc",
                "labels": {"kubernetes.io/service-name": "chain"}
            },
            "addressType": "IPv4",
            "ports": endpoint_ports,
            "endpoints": [{"addresses": ["10.0.0.2"], "conditions": {"ready": false}}]
        }))
        .expect("EndpointSlice");
        ChainObservationFixture {
            plans,
            workload,
            claim,
            service: rendered.services[0].clone(),
            endpoint_slice,
        }
    }

    fn make_chain_fixture_ready(fixture: &mut ChainObservationFixture) {
        fixture
            .workload
            .status
            .as_mut()
            .expect("StatefulSet status")
            .observed_generation = fixture.workload.metadata.generation;
        fixture.claim.status.as_mut().expect("PVC status").phase = Some("Bound".into());
        fixture.endpoint_slice.endpoints[0]
            .conditions
            .as_mut()
            .expect("endpoint conditions")
            .ready = Some(true);
    }

    fn protocol_prober_pod(
        plans: &[ComponentPlanContract],
        component_id: &str,
        ready: bool,
    ) -> Pod {
        let mut pod = Pod::default();
        pod.metadata.labels = Some(BTreeMap::from([(
            PROTOCOL_PROBER_LABEL.into(),
            "true".into(),
        )]));
        pod.metadata.annotations = Some(BTreeMap::from([
            (
                PROTOCOL_PROBER_DIGEST_ANNOTATION.into(),
                protocol_probe_digest(plans),
            ),
            (
                PROTOCOL_PROBER_LEASE_ANNOTATION.into(),
                "lease-current".into(),
            ),
        ]));
        pod.status = Some(k8s_openapi::api::core::v1::PodStatus {
            container_statuses: Some(vec![k8s_openapi::api::core::v1::ContainerStatus {
                name: protocol_probe_container_name(component_id),
                ready,
                ..Default::default()
            }]),
            ..Default::default()
        });
        pod
    }

    fn active_protocol_prober(plans: &[ComponentPlanContract]) -> Deployment {
        let mut deployment = render_protocol_prober(plans)
            .expect("prober render")
            .expect("protocol probes");
        let spec = deployment.spec.as_mut().expect("Deployment spec");
        spec.replicas = Some(1);
        deployment
            .metadata
            .annotations
            .as_mut()
            .expect("Deployment annotations")
            .insert(
                PROTOCOL_PROBER_LEASE_ANNOTATION.into(),
                "lease-current".into(),
            );
        spec.template
            .metadata
            .as_mut()
            .expect("Pod metadata")
            .annotations
            .as_mut()
            .expect("Pod annotations")
            .insert(
                PROTOCOL_PROBER_LEASE_ANNOTATION.into(),
                "lease-current".into(),
            );
        deployment
    }

    #[test]
    fn renders_pinned_three_component_lab_and_stable_inventory() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "static-lab".into(),
            components: vec![
                component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component(
                    "lightning",
                    ComponentKind::Lightning,
                    "lnd",
                    ControlClass::Laboratory,
                ),
                component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
                component(
                    "wallet",
                    ComponentKind::Wallet,
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                ),
            ],
            links: vec![
                LinkSpec {
                    kind: LinkKind::ChainBackend,
                    from: "lightning".into(),
                    to: "chain".into(),
                },
                LinkSpec {
                    kind: LinkKind::LightningBackend,
                    from: "mint".into(),
                    to: "lightning".into(),
                },
            ],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let rendered =
            render_lab("i0123456789012345678", "sha256:revision", &lab, &lock).expect("render");
        assert_eq!(rendered.services.len(), 3);
        assert_eq!(rendered.stateful_sets.len(), 2);
        assert_eq!(rendered.deployments.len(), 3);
        assert!(rendered.deployments.iter().any(|deployment| {
            deployment.metadata.name.as_deref() == Some("proofstorm-protocol-prober")
        }));
        assert_eq!(rendered.persistent_volume_claims.len(), 2);
        assert_eq!(rendered.network_policies.len(), lab.components.len() + 1);
        assert!(
            rendered.network_policies.iter().any(|policy| {
                policy.metadata.name.as_deref() == Some("allow-controller-actions")
            })
        );
        assert_eq!(rendered.inventory(), rendered.inventory());
        assert!(
            lock.entries
                .iter()
                .all(|entry| entry.image.contains("@sha256:"))
        );
    }

    #[test]
    fn bitcoin_plan_rendering_is_pure_and_rollout_scoped() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "bitcoin-plan".into(),
            components: vec![component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            )],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let first_plan =
            compile_component_plans("i0123456789012345678", "sha256:first-revision", &lab, &lock)
                .expect("compile first plan")
                .remove(0);
        assert_eq!(first_plan.effective_config["txindex"], json!(true));
        assert_eq!(first_plan.target_descriptor.ports["rpc"], 18_443);

        let first = render_bitcoin_component(&first_plan).expect("first render");
        let repeated = render_bitcoin_component(&first_plan).expect("repeat render");
        assert_eq!(
            serde_json::to_value(&first.stateful_sets).expect("first JSON"),
            serde_json::to_value(&repeated.stateful_sets).expect("repeat JSON")
        );
        let first_stateful_set = &first.stateful_sets[0];
        assert_eq!(
            first_stateful_set
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(REVISION_DIGEST_ANNOTATION))
                .map(String::as_str),
            Some("sha256:first-revision")
        );
        let first_template = first_stateful_set
            .spec
            .as_ref()
            .expect("stateful set spec")
            .template
            .clone();
        assert_eq!(
            first_template
                .metadata
                .as_ref()
                .expect("template metadata")
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(ROLLOUT_DIGEST_ANNOTATION)),
            Some(&first_plan.rollout_digest)
        );
        assert!(
            first_template
                .metadata
                .as_ref()
                .expect("template metadata")
                .annotations
                .as_ref()
                .is_none_or(|annotations| !annotations.contains_key(REVISION_DIGEST_ANNOTATION))
        );

        let revised_plan = compile_component_plans(
            "i0123456789012345678",
            "sha256:metadata-only-revision",
            &lab,
            &lock,
        )
        .expect("compile revised plan")
        .remove(0);
        let revised = render_bitcoin_component(&revised_plan).expect("revised render");
        assert_eq!(
            first_template,
            revised.stateful_sets[0]
                .spec
                .as_ref()
                .expect("revised stateful set spec")
                .template
        );
        assert_ne!(
            first_stateful_set.metadata.annotations,
            revised.stateful_sets[0].metadata.annotations
        );
    }

    #[test]
    fn lightning_plans_are_dependency_complete_and_rollout_scoped() {
        let lab = lightning_lab();
        let catalog = default_catalog();
        let lock = resolve_lock(&lab, &catalog).expect("initial lock");
        let plans =
            compile_component_plans("i0123456789012345678", "sha256:first-revision", &lab, &lock)
                .expect("initial plans");
        let plan = |id: &str| {
            plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("component plan")
        };
        let renderers: [(&str, LightningRenderer); 2] = [
            ("alice", render_lnd_component),
            ("bob", render_cln_component),
        ];
        let mut initial_templates = BTreeMap::new();
        for (id, renderer) in renderers {
            let component_plan = plan(id);
            assert_eq!(component_plan.effective_config["alias"], json!(id));
            let rendered = renderer(component_plan).expect("render lightning plan");
            let repeated = renderer(component_plan).expect("repeat lightning render");
            assert_eq!(
                serde_json::to_value(&rendered.stateful_sets).expect("rendered JSON"),
                serde_json::to_value(&repeated.stateful_sets).expect("repeated JSON")
            );
            let service_ports = rendered.services[0]
                .spec
                .as_ref()
                .expect("service spec")
                .ports
                .as_ref()
                .expect("service ports");
            for (name, port) in &component_plan.target_descriptor.ports {
                let service_name = name.replace('_', "-");
                assert!(service_ports.iter().any(|service_port| {
                    service_port.name.as_deref() == Some(service_name.as_str())
                        && service_port.port == i32::from(*port)
                }));
            }
            let stateful_set = &rendered.stateful_sets[0];
            assert_eq!(
                stateful_set
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get(REVISION_DIGEST_ANNOTATION))
                    .map(String::as_str),
                Some("sha256:first-revision")
            );
            let template = stateful_set
                .spec
                .as_ref()
                .expect("stateful set spec")
                .template
                .clone();
            assert_eq!(
                template
                    .metadata
                    .as_ref()
                    .expect("template metadata")
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get(ROLLOUT_DIGEST_ANNOTATION)),
                Some(&component_plan.rollout_digest)
            );
            initial_templates.insert(id.to_owned(), template);
        }

        let revised_plans = compile_component_plans(
            "i0123456789012345678",
            "sha256:metadata-only-revision",
            &lab,
            &lock,
        )
        .expect("revised plans");
        for (id, renderer) in renderers {
            let revised_plan = revised_plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("revised component plan");
            let revised = renderer(revised_plan).expect("revised render");
            assert_eq!(
                initial_templates[id],
                revised.stateful_sets[0]
                    .spec
                    .as_ref()
                    .expect("revised stateful set spec")
                    .template
            );
        }
    }

    #[test]
    fn lightning_relinking_is_component_scoped_and_missing_links_refuse() {
        let mut lab = lightning_lab();
        let catalog = default_catalog();
        let lock = resolve_lock(&lab, &catalog).expect("initial lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("initial plans");
        let plan = |id: &str| {
            plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("component plan")
        };
        lab.links
            .iter_mut()
            .find(|link| link.from == "alice")
            .expect("alice chain link")
            .to = "chain-b".into();
        let relinked_lock = resolve_lock(&lab, &catalog).expect("relinked lock");
        let relinked_plans = compile_component_plans(
            "i0123456789012345678",
            "sha256:relinked-revision",
            &lab,
            &relinked_lock,
        )
        .expect("relinked plans");
        let relinked = |id: &str| {
            relinked_plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("relinked component plan")
        };
        assert_ne!(
            plan("alice").rollout_digest,
            relinked("alice").rollout_digest
        );
        assert_eq!(plan("bob").rollout_digest, relinked("bob").rollout_digest);
        let relinked_lnd = render_lnd_component(relinked("alice")).expect("relinked LND");
        assert!(
            relinked_lnd.stateful_sets[0]
                .spec
                .as_ref()
                .expect("LND spec")
                .template
                .spec
                .as_ref()
                .expect("LND Pod")
                .containers[0]
                .args
                .as_ref()
                .expect("LND args")
                .contains(&"--bitcoind.rpchost=chain-b:18443".to_owned())
        );

        let mut incomplete = plan("bob").clone();
        incomplete.relevant_links.clear();
        assert!(matches!(
            render_cln_component(&incomplete),
            Err(AdapterError::MissingLink {
                link: LinkKind::ChainBackend,
                ..
            })
        ));
    }

    #[test]
    fn cdk_plan_rendering_is_deterministic_private_and_rollout_scoped() {
        let lab = cdk_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("CDK lock");
        let plans =
            compile_component_plans("i0123456789012345678", "sha256:first-revision", &lab, &lock)
                .expect("CDK plans");
        let plan = plans
            .iter()
            .find(|plan| plan.component_id == "mint")
            .expect("mint plan");
        assert_eq!(plan.effective_config["name"], json!("Proofstorm CDK mint"));
        assert_eq!(plan.linked_targets["mint-lnd-a"].ports["rpc"], 10_009);

        let rendered = render_cdk_component(plan).expect("render CDK");
        let repeated = render_cdk_component(plan).expect("repeat CDK render");
        assert_eq!(
            serde_json::to_value(&rendered.config_maps).expect("config JSON"),
            serde_json::to_value(&repeated.config_maps).expect("repeat config JSON")
        );
        assert_eq!(
            serde_json::to_value(&rendered.deployments).expect("deployment JSON"),
            serde_json::to_value(&repeated.deployments).expect("repeat deployment JSON")
        );
        let config = rendered.config_maps[0]
            .data
            .as_ref()
            .and_then(|data| data.get("config.toml"))
            .expect("mint config");
        assert!(config.contains("address = \"https://mint-lnd-a:10009\""));
        let deployment = serde_json::to_value(&rendered.deployments[0]).expect("deployment value");
        assert_eq!(
            deployment["metadata"]["annotations"][REVISION_DIGEST_ANNOTATION],
            "sha256:first-revision"
        );
        assert_eq!(
            deployment["spec"]["template"]["metadata"]["annotations"][ROLLOUT_DIGEST_ANNOTATION],
            plan.rollout_digest
        );
        assert_eq!(
            deployment["spec"]["template"]["spec"]["volumes"][2]["persistentVolumeClaim"]["claimName"],
            "data-mint-lnd-a-0"
        );
        assert_eq!(
            deployment["spec"]["template"]["spec"]["containers"][0]["volumeMounts"][2]["readOnly"],
            true
        );

        let revised_plans = compile_component_plans(
            "i0123456789012345678",
            "sha256:metadata-only-revision",
            &lab,
            &lock,
        )
        .expect("revised CDK plans");
        let revised_plan = revised_plans
            .iter()
            .find(|plan| plan.component_id == "mint")
            .expect("revised mint plan");
        let revised = serde_json::to_value(
            &render_cdk_component(revised_plan)
                .expect("revised CDK")
                .deployments[0],
        )
        .expect("revised deployment value");
        assert_eq!(deployment["spec"]["template"], revised["spec"]["template"]);
        assert_ne!(
            deployment["metadata"]["annotations"][REVISION_DIGEST_ANNOTATION],
            revised["metadata"]["annotations"][REVISION_DIGEST_ANNOTATION]
        );
    }

    #[test]
    fn cdk_relinking_updates_only_the_mint_and_incomplete_plans_refuse() {
        let mut lab = cdk_lab();
        let catalog = default_catalog();
        let lock = resolve_lock(&lab, &catalog).expect("initial CDK lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("initial CDK plans");
        let plan = |id: &str| {
            plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("component plan")
        };

        lab.links[0].to = "mint-lnd-b".into();
        let relinked_lock = resolve_lock(&lab, &catalog).expect("relinked CDK lock");
        let relinked_plans = compile_component_plans(
            "i0123456789012345678",
            "sha256:relinked-revision",
            &lab,
            &relinked_lock,
        )
        .expect("relinked CDK plans");
        let relinked = |id: &str| {
            relinked_plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("relinked component plan")
        };
        assert_ne!(plan("mint").rollout_digest, relinked("mint").rollout_digest);
        assert_eq!(
            plan("mint-lnd-a").rollout_digest,
            relinked("mint-lnd-a").rollout_digest
        );
        let rendered = render_cdk_component(relinked("mint")).expect("relinked CDK render");
        let config = rendered.config_maps[0]
            .data
            .as_ref()
            .and_then(|data| data.get("config.toml"))
            .expect("relinked config");
        assert!(config.contains("address = \"https://mint-lnd-b:10009\""));

        let mut missing_link = plan("mint").clone();
        missing_link.relevant_links.clear();
        assert!(matches!(
            render_cdk_component(&missing_link),
            Err(AdapterError::MissingLink {
                link: LinkKind::LightningBackend,
                ..
            })
        ));
        let mut missing_target = plan("mint").clone();
        missing_target.linked_targets.clear();
        assert!(matches!(
            render_cdk_component(&missing_target),
            Err(AdapterError::MissingTarget { .. })
        ));
    }

    #[test]
    fn renderer_registry_covers_every_installed_backend() {
        let contract_ids = default_backend_registry()
            .ids()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        let renderer_ids = COMPONENT_RENDERERS
            .keys()
            .map(|id| (*id).to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(contract_ids, renderer_ids);
    }

    #[test]
    fn wallet_plans_are_deterministic_persistent_and_multi_wallet_isolated() {
        let lab = workspace_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("workspace lock");
        let plans =
            compile_component_plans("i0123456789012345678", "sha256:first-revision", &lab, &lock)
                .expect("workspace plans");
        for id in ["wallet-a", "wallet-b"] {
            let plan = plans
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("wallet plan");
            let rendered = render_wallet_component(plan).expect("wallet render");
            let repeated = render_wallet_component(plan).expect("repeat wallet render");
            assert_eq!(rendered.persistent_volume_claims.len(), 1);
            assert_eq!(rendered.deployments.len(), 1);
            assert_eq!(
                serde_json::to_value(&rendered.deployments).expect("wallet deployments"),
                serde_json::to_value(&repeated.deployments).expect("repeat wallet deployments")
            );
            assert_eq!(
                rendered.persistent_volume_claims[0]
                    .metadata
                    .name
                    .as_deref(),
                Some(format!("{id}-data").as_str())
            );
            let deployment =
                serde_json::to_value(&rendered.deployments[0]).expect("wallet deployment");
            assert_eq!(
                deployment["metadata"]["annotations"][REVISION_DIGEST_ANNOTATION],
                "sha256:first-revision"
            );
            assert_eq!(
                deployment["spec"]["template"]["metadata"]["annotations"]
                    [ROLLOUT_DIGEST_ANNOTATION],
                plan.rollout_digest
            );
            assert_eq!(
                deployment["spec"]["template"]["spec"]["volumes"][0]["persistentVolumeClaim"]["claimName"],
                format!("{id}-data")
            );
            assert!(
                deployment["spec"]["template"]["spec"]["containers"][0]["env"]
                    .as_array()
                    .expect("wallet env")
                    .iter()
                    .any(|entry| entry["name"] == "PROOFSTORM_WALLET" && entry["value"] == id)
            );
        }
    }

    #[test]
    fn wallet_and_attacker_metadata_revisions_do_not_churn_pods() {
        let lab = workspace_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("workspace lock");
        let compile = |revision| {
            compile_component_plans("i0123456789012345678", revision, &lab, &lock)
                .expect("workspace plans")
        };
        let first = compile("sha256:first-revision");
        let revised = compile("sha256:metadata-only-revision");
        for id in ["wallet-a", "attacker"] {
            let first_plan = first
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("first component plan");
            let revised_plan = revised
                .iter()
                .find(|plan| plan.component_id == id)
                .expect("revised component plan");
            let render = |plan| {
                if id == "attacker" {
                    render_attacker_component(plan)
                } else {
                    render_wallet_component(plan)
                }
                .expect("workspace render")
            };
            let first_render = render(first_plan);
            let revised_render = render(revised_plan);
            let value = |rendered: &RenderedComponent| {
                serde_json::to_value(&rendered.deployments[0]).expect("deployment")
            };
            let first_value = value(&first_render);
            let revised_value = value(&revised_render);
            assert_eq!(
                first_value["spec"]["template"],
                revised_value["spec"]["template"]
            );
            assert_ne!(
                first_value["metadata"]["annotations"][REVISION_DIGEST_ANNOTATION],
                revised_value["metadata"]["annotations"][REVISION_DIGEST_ANNOTATION]
            );
        }
    }

    #[test]
    fn attacker_plan_is_disposable_locked_and_restricted() {
        let lab = workspace_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("workspace lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("workspace plans");
        let plan = plans
            .iter()
            .find(|plan| plan.component_id == "attacker")
            .expect("attacker plan");
        let rendered = render_attacker_component(plan).expect("attacker render");
        assert!(rendered.config_maps.is_empty());
        assert!(rendered.services.is_empty());
        assert!(rendered.stateful_sets.is_empty());
        assert!(rendered.persistent_volume_claims.is_empty());
        let deployment =
            serde_json::to_value(&rendered.deployments[0]).expect("attacker deployment");
        assert_eq!(
            deployment["spec"]["template"]["spec"]["automountServiceAccountToken"],
            false
        );
        assert!(deployment["spec"]["template"]["spec"]["volumes"].is_null());
        assert_eq!(
            deployment["spec"]["template"]["spec"]["containers"][0]["image"],
            plan.execution_context.image
        );
        assert_eq!(
            deployment["spec"]["template"]["spec"]["containers"][0]["securityContext"]["allowPrivilegeEscalation"],
            false
        );
    }

    #[test]
    fn component_plans_and_resource_order_ignore_lab_component_order() {
        let mut lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "ordered-plans".into(),
            components: vec![
                component(
                    "chain-b",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component(
                    "chain-a",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
            ],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let first = render_lab("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("first render");
        lab.components.reverse();
        let second = render_lab("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("second render");
        assert_eq!(
            serde_json::to_value(&first.stateful_sets).expect("first resources"),
            serde_json::to_value(&second.stateful_sets).expect("second resources")
        );
        assert_eq!(
            first.inventory(),
            second.inventory(),
            "inventory order must be canonical"
        );
        assert_eq!(first.plans.len(), lab.components.len());
        assert!(first.plans.iter().all(|plan| {
            first.inventory().iter().any(|entry| {
                entry.name == plan.component_id || entry.name.starts_with(&plan.component_id)
            })
        }));
    }

    #[test]
    fn observation_requires_the_compiled_rollout_identity() {
        let lab = workspace_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("workspace lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("workspace plans");
        let wallet = plans
            .iter()
            .find(|plan| plan.component_id == "wallet-a")
            .expect("wallet plan");
        let mut workload = available_deployment(
            render_wallet_component(wallet)
                .expect("wallet render")
                .deployments
                .remove(0),
        );
        set_deployment_rollout(&mut workload, "sha256:stale");
        let stale_resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&workload),
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let stale = observe_component_statuses(
            "instance",
            &plans,
            &stale_resources,
            &[],
            &BTreeSet::new(),
            10,
        );
        let stale_wallet = stale
            .iter()
            .find(|status| status.id == "wallet-a")
            .expect("wallet status");
        assert!(!stale_wallet.ready);
        let stale_workload = status_condition(stale_wallet, ComponentConditionType::WorkloadReady);
        assert_eq!(stale_workload.state, ComponentConditionState::Unknown);
        assert_eq!(
            stale_workload.reason,
            ComponentConditionReason::StaleRevision
        );

        set_deployment_rollout(&mut workload, &wallet.rollout_digest);
        let accepted_resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&workload),
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let accepted = observe_component_statuses(
            "instance",
            &plans,
            &accepted_resources,
            &stale,
            &BTreeSet::new(),
            20,
        );
        let wallet_status = accepted
            .iter()
            .find(|status| status.id == "wallet-a")
            .expect("wallet status");
        assert!(
            !wallet_status.ready,
            "workload readiness alone must not overstate component readiness"
        );
        let workload = status_condition(wallet_status, ComponentConditionType::WorkloadReady);
        assert_eq!(workload.state, ComponentConditionState::True);
        assert_eq!(workload.last_transition_unix, 20);
        let component = status_condition(wallet_status, ComponentConditionType::ComponentReady);
        assert_eq!(component.state, ComponentConditionState::Unknown);
        assert_eq!(wallet_status.kind, wallet.kind);
        assert_eq!(wallet_status.ports, wallet.target_descriptor.ports);

        let empty_resources = ComponentObservationResources {
            deployments: &[],
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let repeated = observe_component_statuses(
            "instance",
            &plans,
            &empty_resources,
            &accepted,
            &BTreeSet::new(),
            30,
        );
        let repeated_wallet = repeated
            .iter()
            .find(|status| status.id == "wallet-a")
            .expect("wallet status");
        let storage = status_condition(repeated_wallet, ComponentConditionType::StorageReady);
        assert_eq!(
            storage.last_transition_unix, 10,
            "unchanged semantic conditions retain transition time"
        );
    }

    #[test]
    fn compatibility_ready_is_derived_only_from_component_ready() {
        let lab = workspace_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("workspace lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("workspace plans");
        let attacker = plans
            .iter()
            .find(|plan| plan.component_id == "attacker")
            .expect("attacker plan");
        let workload = available_deployment(
            render_attacker_component(attacker)
                .expect("attacker render")
                .deployments
                .remove(0),
        );
        let resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&workload),
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let observed =
            observe_component_statuses("instance", &plans, &resources, &[], &BTreeSet::new(), 10);
        let status = observed
            .iter()
            .find(|status| status.id == "attacker")
            .expect("attacker status");
        assert!(status.ready);
        assert_eq!(status.observed_revision_digest, attacker.revision_digest);
        assert_eq!(status.observed_rollout_digest, attacker.rollout_digest);
        assert_eq!(
            status_condition(status, ComponentConditionType::ComponentReady).state,
            ComponentConditionState::True
        );

        let stopped = observe_component_statuses(
            "instance",
            &plans,
            &resources,
            &observed,
            &BTreeSet::from(["attacker".to_owned()]),
            20,
        );
        let status = stopped
            .iter()
            .find(|status| status.id == "attacker")
            .expect("attacker status");
        assert!(!status.ready);
        let component = status_condition(status, ComponentConditionType::ComponentReady);
        assert_eq!(component.state, ComponentConditionState::False);
        assert_eq!(
            component.reason,
            ComponentConditionReason::IntentionallyStopped
        );
        assert_eq!(component.last_transition_unix, 20);
    }

    #[test]
    fn resource_observation_separates_absent_pending_and_ready_states() {
        let mut fixture = chain_observation_fixture();
        assert_eq!(fixture.plans[0].storage[0].claim_name, "data-chain-0");
        let resources = ComponentObservationResources {
            deployments: &[],
            stateful_sets: std::slice::from_ref(&fixture.workload),
            persistent_volume_claims: std::slice::from_ref(&fixture.claim),
            services: std::slice::from_ref(&fixture.service),
            endpoint_slices: std::slice::from_ref(&fixture.endpoint_slice),
            pods: &[],
        };
        let pending = observe_component_statuses(
            "instance",
            &fixture.plans,
            &resources,
            &[],
            &BTreeSet::new(),
            10,
        );
        assert_eq!(
            status_condition(&pending[0], ComponentConditionType::WorkloadReady).reason,
            ComponentConditionReason::WorkloadUnavailable
        );
        assert_eq!(
            status_condition(&pending[0], ComponentConditionType::StorageReady).reason,
            ComponentConditionReason::StoragePending
        );
        assert_eq!(
            status_condition(&pending[0], ComponentConditionType::ServiceReady).reason,
            ComponentConditionReason::EndpointsMissing
        );

        fixture
            .workload
            .status
            .as_mut()
            .expect("StatefulSet status")
            .observed_generation = Some(2);
        fixture.claim.status.as_mut().expect("PVC status").phase = Some("Bound".into());
        fixture.endpoint_slice.endpoints[0]
            .conditions
            .as_mut()
            .expect("endpoint conditions")
            .ready = Some(true);
        let ready_resources = ComponentObservationResources {
            deployments: &[],
            stateful_sets: std::slice::from_ref(&fixture.workload),
            persistent_volume_claims: std::slice::from_ref(&fixture.claim),
            services: std::slice::from_ref(&fixture.service),
            endpoint_slices: std::slice::from_ref(&fixture.endpoint_slice),
            pods: &[],
        };
        let ready = observe_component_statuses(
            "instance",
            &fixture.plans,
            &ready_resources,
            &pending,
            &BTreeSet::new(),
            20,
        );
        for condition_type in [
            ComponentConditionType::WorkloadReady,
            ComponentConditionType::StorageReady,
            ComponentConditionType::ServiceReady,
        ] {
            let condition = status_condition(&ready[0], condition_type);
            assert_eq!(condition.state, ComponentConditionState::True);
            assert_eq!(condition.last_transition_unix, 20);
        }
        assert_eq!(
            status_condition(&ready[0], ComponentConditionType::ProtocolReady).state,
            ComponentConditionState::Unknown
        );
        assert!(!ready[0].ready, "protocol readiness remains independent");

        let empty = ComponentObservationResources {
            deployments: &[],
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let absent = observe_component_statuses(
            "instance",
            &fixture.plans,
            &empty,
            &ready,
            &BTreeSet::new(),
            30,
        );
        assert_eq!(
            status_condition(&absent[0], ComponentConditionType::WorkloadReady).reason,
            ComponentConditionReason::NotObserved
        );
    }

    #[test]
    fn protocol_prober_is_single_bounded_and_credential_free() {
        let lab = lightning_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("plans");
        let prober = render_protocol_prober(&plans)
            .expect("prober render")
            .expect("applicable probes");
        let pod = prober
            .spec
            .as_ref()
            .expect("Deployment spec")
            .template
            .spec
            .as_ref()
            .expect("Pod spec");
        assert_eq!(
            pod.containers.len(),
            plans
                .iter()
                .filter(|plan| plan.protocol_probe.is_some())
                .count()
        );
        assert_eq!(pod.automount_service_account_token, Some(false));
        assert!(pod.volumes.as_ref().is_none_or(Vec::is_empty));
        for container in &pod.containers {
            assert_eq!(container.image.as_deref(), Some(PROBER_IMAGE));
            assert!(container.env.as_ref().is_none_or(Vec::is_empty));
            assert!(container.volume_mounts.as_ref().is_none_or(Vec::is_empty));
            let probe = container.readiness_probe.as_ref().expect("readiness probe");
            assert_eq!(probe.timeout_seconds, Some(2));
            assert_eq!(probe.period_seconds, Some(5));
            assert_eq!(probe.failure_threshold, Some(3));
            let resources = container.resources.as_ref().expect("resource bounds");
            assert!(resources.requests.as_ref().is_some_and(|values| {
                values.get("memory").is_some_and(|value| value.0 == "4Mi")
            }));
        }
    }

    #[test]
    fn protocol_prober_is_bounded_at_the_maximum_component_count() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "max-probes".into(),
            components: (0..64)
                .map(|index| {
                    component(
                        &format!("chain-{index}"),
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Laboratory,
                    )
                })
                .collect(),
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("max lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("max plans");
        let prober = render_protocol_prober(&plans)
            .expect("prober render")
            .expect("applicable probes");
        let containers = &prober
            .spec
            .as_ref()
            .expect("Deployment spec")
            .template
            .spec
            .as_ref()
            .expect("Pod spec")
            .containers;
        assert_eq!(containers.len(), 64);
        assert_eq!(
            containers
                .iter()
                .map(|container| container.name.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            64
        );
        assert!(
            containers
                .iter()
                .all(|container| container.name.len() <= 63)
        );
        assert_eq!(
            prober.spec.as_ref().and_then(|spec| spec.replicas),
            Some(0),
            "rendering never bypasses the global scheduler"
        );
        assert_eq!(
            prober
                .spec
                .as_ref()
                .and_then(|spec| spec.strategy.as_ref())
                .and_then(|strategy| strategy.type_.as_deref()),
            Some("Recreate")
        );
    }

    #[test]
    fn protocol_prober_refuses_more_than_the_per_lab_concurrency_limit() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "too-many-probes".into(),
            components: (0..=crate::MAX_PROTOCOL_PROBES_PER_LAB)
                .map(|index| {
                    component(
                        &format!("chain-{index}"),
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Laboratory,
                    )
                })
                .collect(),
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("plans");
        assert!(matches!(
            render_protocol_prober(&plans),
            Err(AdapterError::InvalidPlan(message))
                if message.contains("exceeds per-lab maximum")
        ));
    }

    #[test]
    fn component_status_shape_is_bounded_at_supported_scale() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "max-status".into(),
            components: (0..64)
                .map(|index| {
                    component(
                        &format!("chain-{index}"),
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Laboratory,
                    )
                })
                .collect(),
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("max lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("max plans");
        let resources = ComponentObservationResources {
            deployments: &[],
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let statuses =
            observe_component_statuses("instance", &plans, &resources, &[], &BTreeSet::new(), 1);
        assert_eq!(statuses.len(), 64);
        assert!(statuses.iter().all(|status| {
            status.conditions.len() <= MAX_COMPONENT_CONDITIONS
                && status
                    .conditions
                    .iter()
                    .map(|condition| condition.condition_type)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == status.conditions.len()
                && status.conditions.iter().all(|condition| {
                    condition.message.len() <= MAX_CONDITION_MESSAGE_BYTES
                        && !condition.message.chars().any(char::is_control)
                })
        }));

        let bounded =
            bounded_condition_message(&format!("{}{}", "line\n".repeat(80), "💥".repeat(80)));
        assert!(bounded.len() <= MAX_CONDITION_MESSAGE_BYTES);
        assert!(!bounded.chars().any(char::is_control));
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn protocol_observation_is_fresh_bounded_and_independent() {
        let mut fixture = chain_observation_fixture();
        make_chain_fixture_ready(&mut fixture);
        let prober = active_protocol_prober(&fixture.plans);
        let failed_pod = protocol_prober_pod(&fixture.plans, "chain", false);
        let failed_resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&prober),
            stateful_sets: std::slice::from_ref(&fixture.workload),
            persistent_volume_claims: std::slice::from_ref(&fixture.claim),
            services: std::slice::from_ref(&fixture.service),
            endpoint_slices: std::slice::from_ref(&fixture.endpoint_slice),
            pods: std::slice::from_ref(&failed_pod),
        };
        let failed = observe_component_statuses(
            "instance",
            &fixture.plans,
            &failed_resources,
            &[],
            &BTreeSet::new(),
            10,
        );
        assert_eq!(
            status_condition(&failed[0], ComponentConditionType::ProtocolReady).reason,
            ComponentConditionReason::ProtocolProbeFailed
        );
        assert!(!failed[0].ready);

        let ready_pod = protocol_prober_pod(&fixture.plans, "chain", true);
        let ready_resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&prober),
            stateful_sets: std::slice::from_ref(&fixture.workload),
            persistent_volume_claims: std::slice::from_ref(&fixture.claim),
            services: std::slice::from_ref(&fixture.service),
            endpoint_slices: std::slice::from_ref(&fixture.endpoint_slice),
            pods: std::slice::from_ref(&ready_pod),
        };
        let ready = observe_component_statuses(
            "instance",
            &fixture.plans,
            &ready_resources,
            &failed,
            &BTreeSet::new(),
            20,
        );
        assert_eq!(
            status_condition(&ready[0], ComponentConditionType::ProtocolReady).reason,
            ComponentConditionReason::ProtocolResponding
        );
        assert!(ready[0].ready);

        let mut stale_pod = ready_pod.clone();
        stale_pod
            .metadata
            .annotations
            .as_mut()
            .expect("prober annotations")
            .insert(
                PROTOCOL_PROBER_DIGEST_ANNOTATION.into(),
                "sha256:stale".into(),
            );
        let stale_resources = ComponentObservationResources {
            pods: std::slice::from_ref(&stale_pod),
            ..ready_resources
        };
        let stale = observe_component_statuses(
            "instance",
            &fixture.plans,
            &stale_resources,
            &ready,
            &BTreeSet::new(),
            30,
        );
        assert_eq!(
            status_condition(&stale[0], ComponentConditionType::ProtocolReady).reason,
            ComponentConditionReason::ProtocolProbePending
        );
        assert!(!stale[0].ready);

        let mut inactive_prober = prober.clone();
        inactive_prober
            .spec
            .as_mut()
            .expect("Deployment spec")
            .replicas = Some(0);
        let inactive_resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&inactive_prober),
            pods: std::slice::from_ref(&ready_pod),
            ..ready_resources
        };
        let inactive = observe_component_statuses(
            "instance",
            &fixture.plans,
            &inactive_resources,
            &ready,
            &BTreeSet::new(),
            40,
        );
        assert_eq!(
            status_condition(&inactive[0], ComponentConditionType::ProtocolReady).reason,
            ComponentConditionReason::ProtocolProbePending
        );
    }

    #[test]
    fn credential_observation_validates_the_linked_state_projection() {
        let lab = cdk_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("CDK lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("CDK plans");
        let mint = plans
            .iter()
            .find(|plan| plan.component_id == "mint")
            .expect("mint plan");
        assert_eq!(mint.credentials.len(), 1);
        assert_eq!(mint.credentials[0].claim_name, "data-mint-lnd-a-0");
        assert!(mint.credentials[0].read_only);
        let deployment = render_cdk_component(mint)
            .expect("mint render")
            .deployments
            .remove(0);
        let mut source = StatefulSet::default();
        source.metadata.name = Some("mint-lnd-a".into());
        source.metadata.annotations = Some(BTreeMap::from([(
            EXECUTION_STATE_CONTRACT_ANNOTATION.to_owned(),
            mint.credentials[0].source_state_contract.clone(),
        )]));
        let mut claim = PersistentVolumeClaim::default();
        claim.metadata.name = Some(mint.credentials[0].claim_name.clone());
        claim.status = Some(k8s_openapi::api::core::v1::PersistentVolumeClaimStatus {
            phase: Some("Bound".into()),
            ..Default::default()
        });
        let resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&deployment),
            stateful_sets: std::slice::from_ref(&source),
            persistent_volume_claims: std::slice::from_ref(&claim),
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let projected =
            observe_component_statuses("instance", &plans, &resources, &[], &BTreeSet::new(), 10);
        let mint_status = projected
            .iter()
            .find(|status| status.id == "mint")
            .expect("mint status");
        assert_eq!(
            status_condition(mint_status, ComponentConditionType::CredentialsReady).reason,
            ComponentConditionReason::CredentialsProjected
        );

        source
            .metadata
            .annotations
            .as_mut()
            .expect("source annotations")
            .insert(
                EXECUTION_STATE_CONTRACT_ANNOTATION.to_owned(),
                "proofstorm/stale-state/v1".into(),
            );
        let stale_resources = ComponentObservationResources {
            deployments: std::slice::from_ref(&deployment),
            stateful_sets: std::slice::from_ref(&source),
            persistent_volume_claims: std::slice::from_ref(&claim),
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let stale = observe_component_statuses(
            "instance",
            &plans,
            &stale_resources,
            &projected,
            &BTreeSet::new(),
            20,
        );
        let mint_status = stale
            .iter()
            .find(|status| status.id == "mint")
            .expect("mint status");
        assert_eq!(
            status_condition(mint_status, ComponentConditionType::CredentialsReady).reason,
            ComponentConditionReason::CredentialsMissing
        );
    }

    #[test]
    fn dependency_readiness_is_transitive_and_component_order_independent() {
        let lab = cdk_lab();
        let lock = resolve_lock(&lab, &default_catalog()).expect("CDK lock");
        let plans = compile_component_plans("i0123456789012345678", "sha256:revision", &lab, &lock)
            .expect("CDK plans");
        let empty = ComponentObservationResources {
            deployments: &[],
            stateful_sets: &[],
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
            pods: &[],
        };
        let mut statuses =
            observe_component_statuses("instance", &plans, &empty, &[], &BTreeSet::new(), 10);
        let target = statuses
            .iter_mut()
            .find(|status| status.id == "mint-lnd-a")
            .expect("target status");
        for condition in &mut target.conditions {
            condition.state = ComponentConditionState::True;
        }
        let initial = statuses.clone();
        resolve_dependency_conditions(&plans, &mut statuses, &[], &BTreeSet::new(), 20);
        let mint = statuses
            .iter()
            .find(|status| status.id == "mint")
            .expect("mint status");
        assert_eq!(
            status_condition(mint, ComponentConditionType::DependenciesReady).state,
            ComponentConditionState::True
        );

        let mut reversed_plans = plans.clone();
        reversed_plans.reverse();
        let mut reversed_statuses = reversed_plans
            .iter()
            .map(|plan| {
                initial
                    .iter()
                    .find(|status| status.id == plan.component_id)
                    .expect("status by plan")
                    .clone()
            })
            .collect::<Vec<_>>();
        resolve_dependency_conditions(
            &reversed_plans,
            &mut reversed_statuses,
            &[],
            &BTreeSet::new(),
            30,
        );
        let mint = reversed_statuses
            .iter()
            .find(|status| status.id == "mint")
            .expect("mint status");
        assert_eq!(
            status_condition(mint, ComponentConditionType::DependenciesReady).state,
            ComponentConditionState::True
        );
    }

    #[test]
    fn component_network_policy_excludes_only_selected_peers_and_keeps_dns() {
        let policy = render_component_network_policy(
            "i0123456789012345678",
            "mint-lnd",
            &["payer-lnd".into(), "attacker-cln".into()],
        )
        .expect("component policy");
        let value = serde_json::to_value(policy).expect("policy JSON");
        assert_eq!(value["metadata"]["name"], "mint-lnd");
        assert_eq!(
            value["spec"]["podSelector"]["matchLabels"][NETWORK_IDENTITY_LABEL],
            "mint-lnd"
        );
        let ingress = &value["spec"]["ingress"][0]["from"][0]["podSelector"]["matchExpressions"][0];
        assert_eq!(ingress["key"], NETWORK_IDENTITY_LABEL);
        assert_eq!(ingress["operator"], "NotIn");
        assert_eq!(
            ingress["values"],
            serde_json::json!(["payer-lnd", "attacker-cln"])
        );
        assert_eq!(
            value["spec"]["egress"][1]["ports"][0]["port"],
            serde_json::json!(53)
        );
    }

    #[test]
    fn renders_cln_with_private_rpc_and_versioned_pinned_adapter() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "cln-lab".into(),
            components: vec![
                component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component(
                    "attacker-cln",
                    ComponentKind::Lightning,
                    "cln",
                    ControlClass::Attacker,
                ),
            ],
            links: vec![LinkSpec {
                kind: LinkKind::ChainBackend,
                from: "attacker-cln".into(),
                to: "chain".into(),
            }],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("CLN lock");
        let rendered =
            render_lab("i0123456789012345678", "sha256:revision", &lab, &lock).expect("CLN render");
        let cln = rendered
            .stateful_sets
            .iter()
            .find(|stateful_set| stateful_set.metadata.name.as_deref() == Some("attacker-cln"))
            .expect("CLN StatefulSet");
        let pod = cln
            .spec
            .as_ref()
            .expect("CLN spec")
            .template
            .spec
            .as_ref()
            .expect("CLN pod");
        let args = pod.containers[0].args.as_ref().expect("CLN args");
        assert!(args.contains(&"--dev-no-reconnect".to_owned()));
        assert!(args.contains(&"--bitcoin-rpcconnect=chain".to_owned()));
        assert_eq!(
            component_ports(
                lab.components
                    .iter()
                    .find(|component| component.id == "attacker-cln")
                    .expect("CLN component")
            ),
            BTreeMap::from([("p2p".to_owned(), 9_735)])
        );
        assert!(lock.entries.iter().any(|entry| {
            entry.catalog_id == "cln"
                && entry.version == "26.06.7"
                && entry.image.contains("@sha256:")
        }));
    }
}
