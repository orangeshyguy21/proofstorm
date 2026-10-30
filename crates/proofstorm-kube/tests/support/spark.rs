//! Unpublished fixtures; no catalog image or architecture qualification.
use proofstorm_core::{
    CellSpec, ComponentPlanContract, ComponentPlanInput, LinkedStateObservationContract,
    TargetDescriptorContract, default_backend_registry, default_catalog, resolve_lock,
};
use std::collections::BTreeMap;

pub fn cell() -> CellSpec {
    serde_json::from_str(include_str!(
        "../../../proofstorm-core/tests/fixtures/spark-topology.json"
    ))
    .unwrap()
}

/// The compiled plan of the fixture component with this implementation.
pub fn plan(backend: &str) -> ComponentPlanContract {
    plan_for(&cell(), backend)
}

pub fn plan_for(cell: &CellSpec, backend: &str) -> ComponentPlanContract {
    let registry = default_backend_registry();
    let component = cell
        .components
        .iter()
        .find(|c| c.implementation == backend)
        .unwrap()
        .clone();
    let ldk: CellSpec =
        serde_json::from_str(include_str!("../../../../examples/ldk-server-cell.json")).unwrap();
    let mut lock = resolve_lock(&ldk, default_catalog())
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.catalog_id == "cdk-ldk-server-processor")
        .unwrap();
    lock.component_id.clone_from(&component.id);
    lock.catalog_id = backend.into();
    lock.version = component.version.clone().unwrap();
    lock.config_version.clone_from(&component.config_version);
    lock.rollout_digest = "sha256:spark-render-test".into();
    lock.image = format!(
        "proofstorm-registry.localhost:5000/{backend}@sha256:{}",
        "0".repeat(64)
    );
    let links = cell.plan_links(&component.id);
    let mut targets = BTreeMap::new();
    let mut state = BTreeMap::new();
    for link in &links {
        let target = cell.components.iter().find(|c| c.id == link.to).unwrap();
        let contract = registry.require(&target.implementation).unwrap();
        targets.insert(
            link.id.clone(),
            TargetDescriptorContract {
                component_id: target.id.clone(),
                kind: target.kind,
                backend_id: target.implementation.clone(),
                version: target.version.clone().unwrap(),
                ports: contract.service_ports.clone(),
            },
        );
        state.insert(
            link.id.clone(),
            LinkedStateObservationContract {
                component_id: target.id.clone(),
                state_contract: contract.execution_state_contract.clone(),
                storage: contract
                    .storage_requirements
                    .iter()
                    .map(|requirement| requirement.resolve(&target.id))
                    .collect(),
            },
        );
    }
    registry
        .compile_contract(&ComponentPlanInput {
            instance_key: "i-spark-render".into(),
            revision_digest: "sha256:render-test".into(),
            component,
            lock,
            relevant_links: links,
            linked_targets: targets,
            linked_state: state,
        })
        .unwrap()
}
