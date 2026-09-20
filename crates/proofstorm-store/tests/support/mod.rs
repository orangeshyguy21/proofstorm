//! Shared fixtures for cell cleanup and update integration tests.
use proofstorm_core::{Capability, CellSpec, PublishedRevision};
use proofstorm_store::{Store, Workspace};
use serde_json::json;

pub(super) fn seed(store: &Store) {
    store
        .put_workspace(&Workspace {
            id: "w".into(),
            name: "w".into(),
        })
        .unwrap();
    store.put_principal("agent").unwrap();
    for cap in [
        Capability::CellCreate,
        Capability::CellRead,
        Capability::CellEdit,
        Capability::CellPublish,
        Capability::CellMaterialize,
        Capability::CellStatus,
        Capability::CellClose,
        Capability::CatalogRead,
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::CellOperate,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
    ] {
        store.grant("w", "agent", cap).unwrap();
    }
}
pub(super) fn cell(ids: &[&str]) -> CellSpec {
    serde_json::from_value(json!({"api_version":"proofstorm/v1alpha1","name":"edit-test","components":ids.iter().map(|id|json!({"id":id,"kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}})).collect::<Vec<_>>(),"links":[]})).unwrap()
}
pub(super) fn publish(store: &Store, id: &str, spec: &CellSpec) -> PublishedRevision {
    store
        .create_draft("w", "agent", id, spec, &format!("{id}-draft"))
        .unwrap();
    store
        .publish("w", "agent", id, 1, &format!("{id}-publish"))
        .unwrap()
}
