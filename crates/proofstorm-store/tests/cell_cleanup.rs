use proofstorm_core::{Capability, CellSpec, OperationKind, PublishedRevision};
use proofstorm_store::{Store, Workspace};
use serde_json::json;

fn seed(store: &Store) {
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
fn cell(ids: &[&str]) -> CellSpec {
    serde_json::from_value(json!({"api_version":"proofstorm/v1alpha1","name":"edit-test","components":ids.iter().map(|id|json!({"id":id,"kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}})).collect::<Vec<_>>(),"links":[]})).unwrap()
}
fn publish(store: &Store, id: &str, spec: &CellSpec) -> PublishedRevision {
    store
        .create_draft("w", "agent", id, spec, &format!("{id}-draft"))
        .unwrap();
    store
        .publish("w", "agent", id, 1, &format!("{id}-publish"))
        .unwrap()
}
#[test]
fn purge_releases_name_history_and_retries_but_preserves_shared_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let revision = publish(&store, "first", &cell(&["chain"]));
    let old = store
        .materialize("w", "agent", "demo", &revision.digest, "start")
        .unwrap();
    let other = store
        .materialize("w", "agent", "other", &revision.digest, "other-start")
        .unwrap();
    store
        .create_experiment("w", "agent", "run", "demo", "run")
        .unwrap();
    store
        .create_operation_at_revision(
            &revision.digest,
            "w",
            "agent",
            "demo",
            "run",
            "",
            "op",
            OperationKind::ComponentExecLive,
            &json!({"component":"chain"}),
            "op",
            Capability::ComponentExecLive,
        )
        .unwrap();
    publish(&store, "template", &cell(&["independent"]));
    let guard = store.try_lifecycle_guard().unwrap().unwrap();
    store.purge_cell(&old).unwrap();
    assert!(store.instance("w", "agent", "demo").is_err());
    assert!(store.experiment("w", "agent", "run").is_err());
    assert!(store.operation("w", "agent", "op").is_err());
    assert!(store.read_draft("w", "agent", "first").is_err());
    assert!(store.read_draft("w", "agent", "template").is_ok());
    assert_eq!(store.instance("w", "agent", "other").unwrap(), other);
    assert!(
        store
            .materialize("w", "agent", "demo", &revision.digest, "start")
            .is_err(),
        "old low-level retry must not resurrect a deleted cell"
    );
    let next = publish(&store, "fresh", &cell(&["different"]));
    let fresh = store
        .materialize("w", "agent", "demo", &next.digest, "new-start")
        .unwrap();
    assert_ne!(fresh.instance_key, old.instance_key);
    assert!(
        store.purge_cell(&old).is_err(),
        "stale cleanup cannot purge the replacement"
    );
    assert_eq!(store.instance("w", "agent", "demo").unwrap(), fresh);
    drop(guard);
    let db = rusqlite::Connection::open(path).unwrap();
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    let receipts: i64 = db
        .query_row(
            "SELECT count(*) FROM idempotency WHERE key IN ('start','run','op','first-draft')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 0);
}

#[test]
fn lifecycle_guard_coordinates_independent_connections_and_releases_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let first = Store::open(&path).unwrap();
    let second = Store::open(&path).unwrap();
    let held = first.try_lifecycle_guard().unwrap().unwrap();
    assert!(second.try_lifecycle_guard().unwrap().is_none());
    assert!(first.try_lifecycle_guard().unwrap().is_none());
    drop(held);
    assert!(second.try_lifecycle_guard().unwrap().is_some());
}

#[test]
fn a_fresh_plan_can_recreate_the_same_configuration_with_shared_revision() {
    let store = Store::memory().unwrap();
    seed(&store);
    let revision = publish(&store, "original-plan", &cell(&["chain"]));
    let old = store
        .materialize("w", "agent", "demo", &revision.digest, "create-demo")
        .unwrap();
    let shared = store
        .materialize("w", "agent", "shared", &revision.digest, "create-shared")
        .unwrap();
    store.purge_cell(&old).unwrap();
    let fresh_revision = publish(&store, "new-plan", &cell(&["chain"]));
    assert_eq!(fresh_revision.digest, revision.digest);
    let fresh = store
        .materialize("w", "agent", "demo", &fresh_revision.digest, "create-new")
        .unwrap();
    assert_ne!(old.instance_key, fresh.instance_key);
    store.purge_cell(&shared).unwrap();
    assert!(
        store.read_draft("w", "agent", "new-plan").is_ok(),
        "cleaning another incarnation must not consume this plan"
    );
    assert!(store.instance("w", "agent", "demo").is_ok());
}
