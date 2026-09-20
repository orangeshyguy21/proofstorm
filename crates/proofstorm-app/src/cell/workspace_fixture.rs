//! Workspace resources for capture/upload tests; callers supply permissions and mock behavior.
use k8s_openapi::api::core::v1::Pod;
use proofstorm_core::CellSpec;
use proofstorm_kube::{ProofstormCell, ProofstormCellSpec, instance_namespace};
use proofstorm_store::{Store, Workspace};
use serde_json::json;

pub(super) fn store() -> Store {
    let store = Store::memory().unwrap();
    store
        .put_workspace(&Workspace {
            id: "local".into(),
            name: "local".into(),
        })
        .unwrap();
    store
}

pub(super) fn materialize(store: &Store) -> (ProofstormCell, Pod) {
    let spec: CellSpec =
        serde_json::from_str(include_str!("../../../../examples/workspace/cell.json")).unwrap();
    store
        .create_draft("local", "actor", "draft", &spec, "draft")
        .unwrap();
    let revision = store
        .publish("local", "actor", "draft", 1, "publish")
        .unwrap();
    let instance = store
        .materialize(
            "local",
            "actor",
            "instance",
            &revision.digest,
            "materialize",
        )
        .unwrap();
    let mut cell = ProofstormCell::new(
        &instance.resource_name,
        ProofstormCellSpec {
            workspace_id: "local".into(),
            instance_id: instance.id,
            instance_key: instance.instance_key.clone(),
            revision_digest: revision.digest,
            cell: spec,
            lock: revision.lock,
        },
    );
    cell.metadata.uid = Some("cell-uid".into());
    let pod = serde_json::from_value(json!({"metadata":{"name":"scripts-pod","namespace":instance_namespace(&instance.instance_key),"uid":"pod-uid"},"status":{"phase":"Running"}})).unwrap();
    (cell, pod)
}
