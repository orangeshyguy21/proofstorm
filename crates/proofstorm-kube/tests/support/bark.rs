//! Unpublished fixtures; no catalog image or architecture qualification.
use proofstorm_core::{
    CellSpec, ComponentPlanContract, ComponentPlanInput, TargetDescriptorContract,
    default_backend_registry, default_catalog, processor_ids::BARK_PROCESSOR, resolve_lock,
};
use std::collections::BTreeMap;

pub fn plan() -> ComponentPlanContract {
    stack_plan(BARK_PROCESSOR)
}

pub fn stack_plan(backend: &str) -> ComponentPlanContract {
    let cell: CellSpec = serde_json::from_str(include_str!(
        "../../../proofstorm-core/tests/fixtures/bark-topology.json"
    ))
    .unwrap();
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
    lock.config_version.clone_from(&component.config_version);
    lock.rollout_digest = "sha256:bark-render-test".into();
    lock.image = if backend == BARK_PROCESSOR {
        "bark-processor:unpublished-test-fixture".into()
    } else {
        format!("{backend}:unpublished-test-fixture")
    };
    let links: Vec<_> = cell
        .links
        .iter()
        .filter(|l| l.from == component.id)
        .cloned()
        .collect();
    let targets: BTreeMap<_, _> = links
        .iter()
        .map(|link| {
            let target = cell.components.iter().find(|c| c.id == link.to).unwrap();
            let mut ports = default_backend_registry()
                .require(&target.implementation)
                .unwrap()
                .service_ports
                .clone();
            // Preserve the original processor fixture's narrow chain descriptor.
            if backend == BARK_PROCESSOR && target.id == "chain" {
                ports.retain(|name, _| name == "rpc");
            }
            (
                link.id.clone(),
                TargetDescriptorContract {
                    component_id: target.id.clone(),
                    kind: target.kind,
                    backend_id: target.implementation.clone(),
                    version: target.version.clone().unwrap(),
                    ports,
                },
            )
        })
        .collect();
    default_backend_registry()
        .compile_contract(&ComponentPlanInput {
            instance_key: "i-bark-render".into(),
            revision_digest: "sha256:render-test".into(),
            component,
            lock,
            relevant_links: links,
            linked_targets: targets,
            linked_state: BTreeMap::new(),
        })
        .unwrap()
}
