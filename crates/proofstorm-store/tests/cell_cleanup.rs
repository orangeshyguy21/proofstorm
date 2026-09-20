mod support;

use proofstorm_core::{Capability, OperationKind};
use proofstorm_store::Store;
use serde_json::json;

use support::{cell, publish, seed};

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
    // Emulate indexes left by an older version, including a foreign cell's rows.
    let legacy = rusqlite::Connection::open(&path).unwrap();
    for table in ["wallet_payment_claims", "wallet_quote_observations"] {
        legacy.execute_batch(&format!("CREATE TABLE {table}(workspace_id TEXT, instance_id TEXT, FOREIGN KEY (workspace_id,instance_id) REFERENCES instances(workspace_id,id)); INSERT INTO {table} VALUES ('w','demo'),('w','other');")).unwrap();
    }
    drop(legacy);
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
    for table in ["wallet_payment_claims", "wallet_quote_observations"] {
        let remaining: String = db
            .query_row(&format!("SELECT instance_id FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, "other");
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
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
