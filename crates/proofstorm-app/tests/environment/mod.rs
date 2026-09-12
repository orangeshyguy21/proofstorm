use super::*;
use proofstorm_app::environment::{EnvironmentQuery, ObservationState};
mod prober;

fn observer(cells: &Cells) -> Cells {
    cells.store.put_principal("viewer").unwrap();
    for cap in [
        Capability::CellRead,
        Capability::CellStatus,
        Capability::ExperimentRead,
    ] {
        cells.store.grant("local", "viewer", cap).unwrap();
    }
    Cells::new(
        cells.store.clone(),
        cells.runtime.clone(),
        "local".into(),
        "viewer".into(),
    )
}
pub(super) fn ready(cluster: &Arc<Mutex<Cluster>>) {
    for (path, object) in &mut cluster.lock().unwrap().objects {
        if path.contains("/proofstormcells/") {
            object["metadata"]["generation"] = json!(1);
            object["metadata"]["resourceVersion"] = json!("12");
            object["metadata"]["managedFields"] =
                json!([{"subresource":"status","time":"2026-09-06T12:00:00Z"}]);
            object["status"]["observedGeneration"] = json!(1);
            object["status"]["phase"] = json!("Ready");
            object["status"]["components"] = json!([{"id":"chain","kind":"bitcoin","observed_revision_digest":object["spec"]["revisionDigest"],"observed_rollout_digest":object["spec"]["lock"]["entries"][0]["rollout_digest"],"conditions":[],"ready":true,"service":"chain","ports":{"rpc":18443}}]);
        }
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one persisted fixture verifies partial reads, preservation, and cluster deletion"
)]
async fn incompatible_history_does_not_hide_current_cells_or_rewrite_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let healthy = cells.up("healthy", &spec()).await.unwrap().cell;
    let mut legacy_spec = spec();
    legacy_spec.name = "legacy".into();
    let legacy = cells.up("legacy", &legacy_spec).await.unwrap().cell;
    let digest = store
        .instance("local", "developer", &legacy.instance_id)
        .unwrap()
        .revision_digest;
    let db = rusqlite::Connection::open(&path).unwrap();
    let raw: String = db
        .query_row(
            "SELECT revision_json FROM revisions WHERE digest=?1",
            [&digest],
            |r| r.get(0),
        )
        .unwrap();
    let mut encoded: Value = serde_json::from_str(&raw).unwrap();
    encoded["cell"]["policy"]["allow"] =
        json!(["lease.acquire", "lease.release", "component.exec"]);
    let legacy_json = encoded.to_string();
    db.execute(
        "UPDATE revisions SET revision_json=?1 WHERE digest=?2",
        rusqlite::params![legacy_json, digest],
    )
    .unwrap();
    ready(&cluster);

    let viewer = observer(&cells);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(proofstorm_app::http::serve_listener(
        viewer.clone(),
        listener,
    ));
    let response = reqwest::get(format!("http://{address}/v1/environment"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let view: proofstorm_app::environment::EnvironmentView = response.json().await.unwrap();
    assert_eq!(view.cells.items.len(), 2);
    let current = view
        .cells
        .items
        .iter()
        .find(|cell| cell.id == healthy.instance_id)
        .unwrap();
    assert!(current.read_error.is_none());
    assert_eq!(current.components.items[0].ready, Some(true));
    let old = view
        .cells
        .items
        .iter()
        .find(|cell| cell.id == legacy.instance_id)
        .unwrap();
    assert_eq!(
        old.read_error.as_deref(),
        Some("stored_record_incompatible")
    );
    assert_eq!(old.handle.as_ref().unwrap().name, "legacy");
    assert!(old.components.items.is_empty());
    let detail = viewer
        .environment(&EnvironmentQuery {
            instance_id: Some(legacy.instance_id.clone()),
            ..EnvironmentQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(detail.cells.items[0].read_error, old.read_error);
    let after: String = db
        .query_row(
            "SELECT revision_json FROM revisions WHERE digest=?1",
            [&digest],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, legacy_json);
    // Compatibility is confined to observation; it cannot grant retired permissions.
    assert!(store.revision("local", "developer", &digest).is_err());
    cluster
        .lock()
        .unwrap()
        .objects
        .retain(|_, object| object["spec"]["instanceId"] != legacy.instance_id);
    let current = viewer
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert_eq!(current.cells.items.len(), 1);
    assert_eq!(current.cells.items[0].id, healthy.instance_id);
    assert!(
        viewer
            .environment(&EnvironmentQuery {
                instance_id: Some(legacy.instance_id),
                ..EnvironmentQuery::default()
            })
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn incompatible_pending_receipts_do_not_starve_current_operations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    cells.up("demo", &spec()).await.unwrap();
    let bad = cells
        .exec("demo", "chain", command(), "a-legacy")
        .await
        .unwrap();
    let good = cells
        .exec("demo", "chain", command(), "z-current")
        .await
        .unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE actions SET capability_json='\"component.exec\"' WHERE id=?1",
        [&bad.id],
    )
    .unwrap();
    let live = std::collections::BTreeSet::from([good.instance_id.clone()]);
    let first = store
        .pending_observations("local", "developer", "", 1, &live)
        .unwrap();
    assert!(first.operations.is_empty());
    assert_eq!(first.incompatible_records, 1);
    assert_eq!(first.next_cursor.as_deref(), Some(bad.id.as_str()));
    let second = store
        .pending_observations(
            "local",
            "developer",
            first.next_cursor.as_deref().unwrap(),
            1,
            &live,
        )
        .unwrap();
    assert_eq!(second.operations[0].id, good.id);
    let absent = store
        .pending_observations(
            "local",
            "developer",
            "",
            50,
            &std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert!(absent.operations.is_empty());
    assert_eq!(absent.incompatible_records, 0);
    for (path, object) in &mut cluster.lock().unwrap().objects {
        if path.contains("/proofstormcellactions/") {
            object["status"] = json!({"phase":"Succeeded", "artifact":{"exit_code":0,"cleanup_verified":true,"timed_out":false}});
        }
    }
    let collector = proofstorm_app::observer::Observer::start(cells);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if collector.status.read().unwrap().recorded_operations == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        store
            .operation("local", "developer", &good.id)
            .unwrap()
            .phase,
        OperationPhase::Succeeded
    );
    let status = collector.status.read().unwrap();
    assert_eq!(status.state, "degraded");
    assert!(
        status
            .error
            .as_ref()
            .unwrap()
            .contains("incompatible stored records")
    );
    let retained: (String, String) = db
        .query_row(
            "SELECT capability_json,phase_json FROM actions WHERE id=?1",
            [&bad.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(retained.0, "\"component.exec\"");
    assert_ne!(retained.1, "\"succeeded\"");
}

#[tokio::test]
async fn http_preserves_startup_failure_reason_and_recovery_message() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    cells.up("blocked", &spec()).await.unwrap();
    ready(&cluster);
    let message = "Image pull is failing and backing off, not building. Operator: run just images and just doctor; verify registry access.";
    for (path, object) in &mut cluster.lock().unwrap().objects {
        if path.contains("/proofstormcells/") {
            object["status"]["phase"] = json!("Pending");
            object["status"]["components"][0]["ready"] = json!(false);
            object["status"]["components"][0]["conditions"] = json!([{
                "condition_type":"workload_ready", "state":"false", "reason":"image_pull_backoff", "message":message, "last_transition_unix":1
            }]);
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/environment", listener.local_addr().unwrap());
    let server = tokio::spawn(proofstorm_app::http::serve_listener(
        observer(&cells),
        listener,
    ));
    let response = reqwest::get(url).await.unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let component = &body["cells"]["items"][0]["components"]["items"][0];
    assert_eq!(component["ready"], false);
    assert_eq!(component["conditions"][0]["reason"], "image_pull_backoff");
    assert_eq!(component["conditions"][0]["message"], message);
    server.abort();
}

#[tokio::test]
async fn environment_reads_are_passive_scoped_and_credential_free() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let cell = cells.up("demo", &spec()).await.unwrap().cell;
    let mut cmd = command();
    cmd.argv.push("SECRET-IN-ARGV".into());
    let op = cells
        .exec("demo", "chain", cmd, "secret-operation")
        .await
        .unwrap();
    store
        .record_operation_result(
            "local",
            &op.id,
            OperationPhase::Succeeded,
            json!({"exit_code":7,"cleanup_verified":true,"private":"SECRET-IN-ARTIFACT"}),
        )
        .unwrap();
    store
        .start_session(
            "local",
            "developer",
            &store
                .default_run_id("local", "developer", &cell.instance_id)
                .unwrap(),
            "second-session",
            "second-session",
        )
        .unwrap();
    ready(&cluster);
    let before = store
        .sessions("local", "developer", &cell.instance_id, "", 100)
        .unwrap();
    let request_start = cluster.lock().unwrap().requests.len();
    let view = observer(&cells)
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    let item = &view.cells.items[0];
    assert_eq!(item.id, cell.instance_id);
    assert!(matches!(item.runtime.state, ObservationState::Available));
    assert_eq!(item.runtime.source_updated_at_unix, Some(1_788_696_000));
    assert_eq!(item.components.items[0].ready, Some(true));
    let endpoint = item.components.items[0]
        .endpoints
        .iter()
        .find(|e| e.name == "rpc")
        .unwrap();
    assert!(endpoint.local_connection_supported);
    assert_eq!(
        endpoint.local_authentication,
        Some(proofstorm_app::connections::Authentication::Basic)
    );
    assert_eq!(item.activity.items[0].native_exit_code, Some(7));
    assert_eq!(item.activity.items[0].components, vec!["chain"]);
    assert!(
        item.sessions
            .items
            .iter()
            .all(|s| s.overlapping_session_count >= 1)
    );
    assert_eq!(item.resources.as_ref().unwrap().storage.len(), 1);
    assert!(
        !item.resources.as_ref().unwrap().workloads[0].containers[0]
            .requests
            .is_empty()
    );
    let encoded = serde_json::to_string(&view).unwrap();
    for secret in [
        "SECRET-IN-ARGV",
        "SECRET-IN-ARTIFACT",
        proofstorm_kube::BITCOIN_RPC_PASSWORD,
        "\"config\"",
        "\"annotations\"",
        "\"argv\"",
    ] {
        assert!(!encoded.contains(secret), "leaked {secret}");
    }
    assert_eq!(
        before.sessions,
        store
            .sessions("local", "developer", &cell.instance_id, "", 100)
            .unwrap()
            .sessions
    );
    assert!(
        cluster.lock().unwrap().requests[request_start..]
            .iter()
            .all(|(method, _)| method == "GET")
    );
    assert_workspace_isolation(&cells, &cell.instance_id).await;
}

async fn assert_workspace_isolation(cells: &Cells, instance_id: &str) {
    let store = &cells.store;
    store
        .put_workspace(&Workspace {
            id: "other".into(),
            name: "other".into(),
        })
        .unwrap();
    let other = Cells::new(
        store.clone(),
        cells.runtime.clone(),
        "other".into(),
        "viewer".into(),
    );
    assert!(
        other
            .environment(&EnvironmentQuery::default())
            .await
            .is_err()
    );
    for cap in [
        Capability::CellRead,
        Capability::CellStatus,
        Capability::ExperimentRead,
    ] {
        store.grant("other", "viewer", cap).unwrap();
    }
    assert!(
        other
            .environment(&EnvironmentQuery::default())
            .await
            .unwrap()
            .cells
            .items
            .is_empty()
    );
    assert!(
        other
            .environment(&EnvironmentQuery {
                instance_id: Some(instance_id.into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn environment_lists_only_current_cluster_cells_without_duplicates() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster);
    let old = cells.up("demo", &spec()).await.unwrap().cell;
    cells.down("demo", 1).await.unwrap();
    let new = cells.up("demo", &spec()).await.unwrap().cell;
    let instance = store
        .instance("local", "developer", &new.instance_id)
        .unwrap();
    store
        .materialize(
            "local",
            "developer",
            "advanced-instance",
            &instance.revision_digest,
            "advanced-instance",
        )
        .unwrap();
    let _pending = store
        .reserve_cell("local", "developer", "pending", "digest")
        .unwrap();
    let mut query = EnvironmentQuery {
        limit: 1,
        ..Default::default()
    };
    let mut ids = Vec::new();
    loop {
        let view = cells.environment(&query).await.unwrap();
        assert_eq!(view.cells.items.len(), 1);
        let item = &view.cells.items[0];
        ids.push(item.id.clone());
        match view.cells.next_cursor {
            Some(cursor) => query.cursor = cursor,
            None => break,
        }
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 1);
    assert!(!ids.contains(&old.instance_id));
    assert!(ids.contains(&new.instance_id));
}

#[tokio::test]
async fn environment_paging_crosses_runs_and_preserves_unknown_outcomes() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let cell = cells.up("demo", &spec()).await.unwrap().cell;
    for id in ["op-a", "op-b", "op-c"] {
        cells.exec("demo", "chain", command(), id).await.unwrap();
    }
    store
        .create_experiment(
            "local",
            "developer",
            "advanced-run",
            &cell.instance_id,
            "advanced-run",
        )
        .unwrap();
    store
        .create_operation(
            "local",
            "developer",
            &cell.instance_id,
            "advanced-run",
            "",
            "op-d",
            proofstorm_core::OperationKind::ComponentExecLive,
            &json!({"component":"chain"}),
            "op-d",
            Capability::ComponentExecLive,
        )
        .unwrap();
    for (path, o) in &mut cluster.lock().unwrap().objects {
        if path.contains("/proofstormcellactions/") {
            o["status"] = json!({"phase":"Succeeded","artifact":{"exit_code":0}});
        }
    }
    let mut query = EnvironmentQuery {
        instance_id: Some(cell.instance_id),
        limit: 1,
        ..Default::default()
    };
    let mut ids = Vec::new();
    loop {
        let view = cells.environment(&query).await.unwrap();
        let item = &view.cells.items[0];
        assert_eq!(item.activity.items.len(), 1);
        let op = &item.activity.items[0];
        assert!(matches!(
            op.phase,
            OperationPhase::Running | OperationPhase::Pending
        ));
        ids.push(op.id.clone());
        match &item.activity.next_cursor {
            Some(c) => query.activity_cursor = c.clone(),
            None => break,
        }
    }
    ids.sort();
    assert_eq!(ids, vec!["op-a", "op-b", "op-c", "op-d"]);
    query.activity_cursor = "invalid".into();
    assert!(cells.environment(&query).await.is_err());
    query.limit = 51;
    assert!(cells.environment(&query).await.is_err());
    assert!(
        cells
            .environment(&EnvironmentQuery {
                session_cursor: "x".into(),
                ..Default::default()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn stale_generation_keeps_unchanged_components_ready_but_rejects_wrong_identity() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    cells.up("demo", &spec()).await.unwrap();
    ready(&cluster);
    let path = cluster
        .lock()
        .unwrap()
        .objects
        .keys()
        .find(|p| p.contains("/proofstormcells/"))
        .unwrap()
        .clone();
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["generation"] = json!(2);
    let view = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert!(matches!(
        view.cells.items[0].runtime.state,
        ObservationState::Stale
    ));
    assert_eq!(view.cells.items[0].components.items[0].ready, Some(true));
    let layout_id = view.cells.items[0].layout_id.clone();
    assert!(
        layout_id
            .as_deref()
            .is_some_and(|id| id.starts_with("local:"))
    );
    assert!(
        view.cells.items[0].components.items[0]
            .details
            .as_ref()
            .unwrap()
            .observed_version
            .is_some()
    );
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["status"]["components"][0]["observed_rollout_digest"] =
        json!("old-rollout");
    let changed = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert_eq!(changed.cells.items[0].layout_id, layout_id);
    assert_eq!(
        changed.cells.items[0].components.items[0]
            .details
            .as_ref()
            .unwrap()
            .observed_version,
        None
    );
    assert_eq!(
        view.cells.items[0].runtime.phase,
        Some(proofstorm_core::InstancePhase::Pending)
    );
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["spec"]["workspaceId"] = json!("other");
    let view = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert!(view.cells.items.is_empty());
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["spec"]["workspaceId"] = json!("local");
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["spec"]["instanceKey"] =
        json!("wrong-key");
    let mismatch = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert_eq!(
        mismatch.cells.items[0].runtime.error.as_deref(),
        Some("runtime_identity_mismatch")
    );
    cluster.lock().unwrap().objects.remove(&path);
    let view = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert!(view.cells.items.is_empty());
}

#[tokio::test]
async fn http_matches_shared_contract_and_refuses_writes_foreign_origins_and_revoked_reads() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster);
    cells.up("demo", &spec()).await.unwrap();
    let viewer = observer(&cells);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(proofstorm_app::http::serve_listener(
        viewer.clone(),
        listener,
    ));
    let url = format!("http://{address}/v1/environment");
    let client = reqwest::Client::new();
    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let wire: Value = response.json().await.unwrap();
    let shared = serde_json::to_value(
        viewer
            .environment(&EnvironmentQuery::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        wire["cells"]["items"][0]["id"],
        shared["cells"]["items"][0]["id"]
    );
    assert_eq!(
        wire["cells"]["items"][0]["resources"],
        shared["cells"]["items"][0]["resources"]
    );
    assert_eq!(
        client
            .get(format!("{url}/schema"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(client.post(&url).send().await.unwrap().status(), 405);
    assert_eq!(
        client
            .get(&url)
            .header("origin", "https://elsewhere.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&url)
            .header("host", "elsewhere.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{url}?limit=0"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .get(format!("{url}?unknown=true"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let system_url = format!("http://{address}/v1/system");
    assert_eq!(client.get(&system_url).send().await.unwrap().status(), 200);
    assert_eq!(client.post(&system_url).send().await.unwrap().status(), 405);
    assert_eq!(
        client
            .get(&system_url)
            .header("origin", "https://elsewhere.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    store.replace_grants("local", "viewer", []).unwrap();
    assert_eq!(client.get(&system_url).send().await.unwrap().status(), 403);
    assert_eq!(client.get(&url).send().await.unwrap().status(), 403);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cluster_inventory_failure_is_not_reported_as_an_empty_cluster() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    cells.up("demo", &spec()).await.unwrap();
    cluster.lock().unwrap().fail_reads = true;
    let error = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "runtime_failure");
}

#[tokio::test]
async fn large_topology_pages_fit_the_shared_budget_without_losing_components_or_links() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster);
    let mut spec = spec();
    spec.components = (0..64)
        .map(|n| {
            let mut c = spec.components[0].clone();
            c.id = format!("chain-{n:02}");
            c
        })
        .collect();
    spec.links = (1..64)
        .map(|n| proofstorm_core::LinkSpec {
            id: format!("link-{n:02}"),
            kind: proofstorm_core::LinkKind::NetworkPath,
            from: "chain-00".into(),
            to: format!("chain-{n:02}"),
            binding: None,
        })
        .collect();
    let cell = cells.up("large", &spec).await.unwrap().cell;
    let mut query = EnvironmentQuery {
        instance_id: Some(cell.instance_id),
        limit: 50,
        ..Default::default()
    };
    let mut components = Vec::new();
    loop {
        let view = cells.environment(&query).await.unwrap();
        assert!(serde_json::to_vec(&view).unwrap().len() <= 24 * 1024);
        let item = &view.cells.items[0];
        components.extend(item.components.items.iter().map(|c| c.id.clone()));
        assert!(item.resources.as_ref().unwrap().workloads.iter().all(|w| {
            w.component
                .as_ref()
                .is_none_or(|id| item.components.items.iter().any(|c| &c.id == id))
        }));
        match &item.components.next_cursor {
            Some(c) => query.component_cursor = c.clone(),
            None => break,
        }
    }
    assert_eq!(components.len(), 64);
    components.sort();
    components.dedup();
    assert_eq!(components.len(), 64);
    let mut links = Vec::new();
    loop {
        let view = cells.environment(&query).await.unwrap();
        let item = &view.cells.items[0];
        links.extend(item.links.items.iter().map(|l| l.id.clone()));
        match &item.links.next_cursor {
            Some(c) => query.link_cursor = c.clone(),
            None => break,
        }
    }
    assert_eq!(links.len(), 63);
    links.sort();
    links.dedup();
    assert_eq!(links.len(), 63);
}

#[test]
fn published_environment_schema_matches_the_shared_contract() {
    let published: Value = serde_json::from_str(include_str!(
        "../../../../schemas/v1alpha1/environment.schema.json"
    ))
    .unwrap();
    let actual = serde_json::to_value(schemars::schema_for!(
        proofstorm_app::environment::EnvironmentView
    ))
    .unwrap();
    assert_eq!(published, actual);
}

#[tokio::test]
async fn recorded_network_faults_identify_both_components() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster);
    let mut spec = spec();
    let mut second = spec.components[0].clone();
    second.id = "chain-two".into();
    spec.components.push(second);
    let cell = cells.up("demo", &spec).await.unwrap().cell;
    store
        .grant("local", "developer", Capability::NetworkPartition)
        .unwrap();
    store
        .create_operation(
            "local",
            "developer",
            &cell.instance_id,
            &store
                .default_run_id("local", "developer", &cell.instance_id)
                .unwrap(),
            "",
            "fault",
            proofstorm_core::OperationKind::NetworkPartition,
            &json!({"from_component":"chain","to_component":"chain-two"}),
            "fault",
            Capability::NetworkPartition,
        )
        .unwrap();
    let view = cells
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert_eq!(
        view.cells.items[0].activity.items[0].components,
        vec!["chain", "chain-two"]
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one server lifecycle covers receipt collection, reconnect and revocation"
)]
async fn server_collects_disconnected_agent_receipts_and_streams_changes() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let cell = cells.up("demo", &spec()).await.unwrap().cell;
    let op = cells
        .exec("demo", "chain", command(), "background-op")
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(proofstorm_app::http::serve_listener(
        cells.clone(),
        listener,
    ));
    let client = reqwest::Client::new();
    let mut stream = client.get(format!("{url}/v1/events")).send().await.unwrap();
    assert_eq!(stream.headers()["content-type"], "text/event-stream");
    let first = stream.chunk().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("event: environment"));
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        let mut received = String::new();
        loop {
            received.push_str(&String::from_utf8_lossy(
                &stream.chunk().await.unwrap().unwrap(),
            ));
            if received.contains("event: telemetry") {
                break;
            }
        }
    })
    .await
    .expect("sampler must invalidate cached measurements through SSE");
    let start = cluster.lock().unwrap().requests.len();
    for (path, object) in &mut cluster.lock().unwrap().objects {
        if path.contains("/proofstormcellactions/") {
            object["status"] = json!({"phase":"Succeeded", "artifact":{"exit_code":0,"cleanup_verified":true,"timed_out":false}});
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let result = cells
                .environment(&EnvironmentQuery::default())
                .await
                .unwrap();
            if result.cells.items[0].activity.items[0].phase == OperationPhase::Succeeded {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let chunk = stream.chunk().await.unwrap().unwrap();
            if String::from_utf8_lossy(&chunk).contains("event: environment") {
                break;
            }
        }
    })
    .await;
    assert!(event.is_ok());
    let status: Value = client
        .get(format!("{url}/v1/observer"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["state"], "watching");
    assert_eq!(status["recorded_operations"], 1);
    // Re-observation uses the existing idempotent journal path and creates no work.
    cells.sync("demo").await.unwrap();
    assert_eq!(
        store.operation("local", "developer", &op.id).unwrap().phase,
        OperationPhase::Succeeded
    );
    assert!(
        cluster.lock().unwrap().requests[start..]
            .iter()
            .all(|(method, _)| method == "GET")
    );
    assert_eq!(
        store
            .sessions("local", "developer", &cell.instance_id, "", 100)
            .unwrap()
            .sessions
            .len(),
        1
    );
    let mut reconnected = client
        .get(format!("{url}/v1/events"))
        .header("Last-Event-ID", "999999")
        .send()
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&reconnected.chunk().await.unwrap().unwrap())
            .contains("event: environment")
    );
    store.replace_grants("local", "developer", []).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stream.chunk().await.unwrap().is_some() {}
    })
    .await
    .unwrap();
    assert_eq!(
        client
            .get(format!("{url}/v1/events"))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn web_assets_and_same_origin_requests_are_served_safely() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cells = service(store, Arc::new(Mutex::new(Cluster::default())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(proofstorm_app::http::serve_listener(cells, listener));
    let client = reqwest::Client::new();
    let root = client.get(&url).send().await.unwrap();
    if root.status() == 200 {
        assert!(
            root.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("text/html")
        );
        let html = root.text().await.unwrap();
        assert!(html.contains(".wasm"));
        let wasm = html
            .split(['\"', '\''])
            .find(|part| {
                std::path::Path::new(part)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"))
            })
            .unwrap();
        let asset = client.get(format!("{url}{wasm}")).send().await.unwrap();
        assert_eq!(asset.headers()["content-type"], "application/wasm");
    } else {
        assert_eq!(root.status(), 503);
    }
    assert_eq!(
        client
            .get(format!("{url}/v1/environment"))
            .header("Origin", &url)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for path in ["/v1/events", "/"] {
        assert_eq!(
            client
                .get(format!("{url}{path}"))
                .header("Origin", "https://foreign.example")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .post(format!("{url}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            405
        );
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn system_measurements_are_scoped_passive_and_preserve_missing_metrics() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let handle = cells.up("demo", &spec()).await.unwrap().cell;
    let instance = store
        .instance("local", "developer", &handle.instance_id)
        .unwrap();
    let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
    let pods_path = format!("/api/v1/namespaces/{namespace}/pods");
    let metrics_path = format!("/apis/metrics.k8s.io/v1beta1/namespaces/{namespace}/pods");
    {
        let mut cluster = cluster.lock().unwrap();
        cluster.objects.insert(pods_path.clone(), json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{
            "metadata":{"name":"chain-0","labels":{"proofstorm.dev/component":"chain","proofstorm.dev/instance":instance.instance_key}},
            "spec":{"containers":[{"name":"component","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"1","memory":"256Mi"}}}]},
            "status":{"containerStatuses":[{"name":"component","image":"chain","imageID":"chain","ready":true,"restartCount":2,"state":{"running":{}}}]}
        }]}));
        cluster.objects.insert(metrics_path.clone(), json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":"PodMetricsList","metadata":{},"items":[{
            "metadata":{"name":"chain-0"},"timestamp":k8s_openapi::jiff::Timestamp::now().to_string(),
            "containers":[{"name":"component","usage":{"cpu":"125000000n","memory":"32Mi"}}]
        },{"metadata":{"name":"unrelated"},"containers":[{"name":"component","usage":{"cpu":"100","memory":"32Gi"}}]}]}));
        cluster.requests.clear();
    }
    let viewer = observer(&cells);
    let view = viewer.system().await.unwrap();
    assert_eq!(view.cells.len(), 1);
    assert_eq!(
        (view.totals.running, view.totals.ready, view.totals.sampled),
        (1, 1, 1)
    );
    assert!((view.totals.cpu_millicores.unwrap() - 125.0).abs() < 1e-9);
    assert_eq!(
        view.cells[0].processes[0].component.as_deref(),
        Some("chain")
    );
    assert!(view.cells[0].balances.is_empty()); // Viewer cannot run even passive commands.
    assert!(
        cluster
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, _)| method == "GET")
    );
    cluster.lock().unwrap().objects.remove(&metrics_path);
    let missing = viewer.system().await.unwrap();
    assert_eq!(missing.totals.running, 1);
    assert_eq!(missing.totals.sampled, 0);
    assert!(missing.totals.cpu_millicores.is_none());
    assert!(missing.cells[0].metrics_error.is_some());
    let cell_path = format!(
        "/apis/proofstorm.dev/v1alpha1/namespaces/system/proofstormcells/{}",
        instance.resource_name
    );
    cluster.lock().unwrap().objects.get_mut(&cell_path).unwrap()["spec"]["instanceKey"] =
        json!("wrong-key");
    let invalid = viewer.system().await.unwrap();
    assert!(invalid.cells[0].error.is_some());
    assert!(invalid.totals.cpu_millicores.is_none());
    assert!(
        !cluster
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(_, path)| path.contains("wrong-key"))
    );
    store.replace_grants("local", "viewer", []).unwrap();
    assert!(viewer.system().await.is_err());
}
