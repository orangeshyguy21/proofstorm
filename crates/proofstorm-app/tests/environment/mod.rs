use super::*;
use proofstorm_app::environment::{EnvironmentQuery, ObservationState};

fn observer(labs: &Labs) -> Labs {
    labs.store.put_principal("viewer").unwrap();
    for cap in [
        Capability::LabRead,
        Capability::LabStatus,
        Capability::ExperimentRead,
    ] {
        labs.store.grant("local", "viewer", cap).unwrap();
    }
    Labs::new(
        labs.store.clone(),
        labs.runtime.clone(),
        "local".into(),
        "viewer".into(),
    )
}
fn ready(cluster: &Arc<Mutex<Cluster>>) {
    for (path, object) in &mut cluster.lock().unwrap().objects {
        if path.contains("/proofstormlabs/") {
            object["metadata"]["generation"] = json!(1);
            object["metadata"]["resourceVersion"] = json!("12");
            object["metadata"]["managedFields"] =
                json!([{"subresource":"status","time":"2026-09-06T12:00:00Z"}]);
            object["status"]["observedGeneration"] = json!(1);
            object["status"]["phase"] = json!("Ready");
            object["status"]["components"] = json!([{"id":"chain","kind":"bitcoin","observed_revision_digest":object["spec"]["revisionDigest"],"observed_rollout_digest":"rollout","conditions":[],"ready":true,"service":"chain","ports":{"rpc":18443}}]);
        }
    }
}

#[tokio::test]
async fn environment_reads_are_passive_scoped_and_credential_free() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let lab = labs.up("demo", &spec()).await.unwrap().lab;
    let mut cmd = command();
    cmd.argv.push("SECRET-IN-ARGV".into());
    let op = labs
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
            &lab.run_id(),
            "second-session",
            "second-session",
        )
        .unwrap();
    ready(&cluster);
    let before = store
        .sessions("local", "developer", &lab.instance_id, "", 100)
        .unwrap();
    let request_start = cluster.lock().unwrap().requests.len();
    let view = observer(&labs)
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    let item = &view.labs.items[0];
    assert_eq!(item.id, lab.instance_id);
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
            .sessions("local", "developer", &lab.instance_id, "", 100)
            .unwrap()
            .sessions
    );
    assert!(
        cluster.lock().unwrap().requests[request_start..]
            .iter()
            .all(|(method, _)| method == "GET")
    );
    assert_workspace_isolation(&labs, &lab.instance_id).await;
}

async fn assert_workspace_isolation(labs: &Labs, instance_id: &str) {
    let store = &labs.store;
    store
        .put_workspace(&Workspace {
            id: "other".into(),
            name: "other".into(),
        })
        .unwrap();
    let other = Labs::new(
        store.clone(),
        labs.runtime.clone(),
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
        Capability::LabRead,
        Capability::LabStatus,
        Capability::ExperimentRead,
    ] {
        store.grant("other", "viewer", cap).unwrap();
    }
    assert!(
        other
            .environment(&EnvironmentQuery::default())
            .await
            .unwrap()
            .labs
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
async fn environment_lists_unnamed_historical_and_unmaterialized_labs_without_duplicates() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster);
    let old = labs.up("demo", &spec()).await.unwrap().lab;
    labs.down("demo", 1).await.unwrap();
    let new = labs.up("demo", &spec()).await.unwrap().lab;
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
    let pending = store
        .reserve_lab("local", "developer", "pending", "digest")
        .unwrap();
    let mut query = EnvironmentQuery {
        limit: 1,
        ..Default::default()
    };
    let mut ids = Vec::new();
    loop {
        let view = labs.environment(&query).await.unwrap();
        assert_eq!(view.labs.items.len(), 1);
        let item = &view.labs.items[0];
        if item.id == old.instance_id {
            assert!(item.handle.is_none());
            assert!(matches!(item.runtime.state, ObservationState::Missing));
        }
        if item.id == pending.instance_id {
            assert!(matches!(
                item.runtime.state,
                ObservationState::NotMaterialized
            ));
        }
        ids.push(item.id.clone());
        match view.labs.next_cursor {
            Some(cursor) => query.cursor = cursor,
            None => break,
        }
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    assert!(ids.contains(&old.instance_id));
    assert!(ids.contains(&new.instance_id));
}

#[tokio::test]
async fn environment_paging_crosses_runs_and_preserves_unknown_outcomes() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let lab = labs.up("demo", &spec()).await.unwrap().lab;
    for id in ["op-a", "op-b", "op-c"] {
        labs.exec("demo", "chain", command(), id).await.unwrap();
    }
    store
        .create_experiment(
            "local",
            "developer",
            "advanced-run",
            &lab.instance_id,
            "advanced-run",
        )
        .unwrap();
    store
        .create_operation(
            "local",
            "developer",
            &lab.instance_id,
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
        if path.contains("/proofstormlabactions/") {
            o["status"] = json!({"phase":"Succeeded","artifact":{"exit_code":0}});
        }
    }
    let mut query = EnvironmentQuery {
        instance_id: Some(lab.instance_id),
        limit: 1,
        ..Default::default()
    };
    let mut ids = Vec::new();
    loop {
        let view = labs.environment(&query).await.unwrap();
        let item = &view.labs.items[0];
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
    assert!(labs.environment(&query).await.is_err());
    query.limit = 51;
    assert!(labs.environment(&query).await.is_err());
    assert!(
        labs.environment(&EnvironmentQuery {
            session_cursor: "x".into(),
            ..Default::default()
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn stale_missing_and_wrong_runtime_identity_never_claim_ready() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster.clone());
    labs.up("demo", &spec()).await.unwrap();
    ready(&cluster);
    let path = cluster
        .lock()
        .unwrap()
        .objects
        .keys()
        .find(|p| p.contains("/proofstormlabs/"))
        .unwrap()
        .clone();
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["generation"] = json!(2);
    let view = labs
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert!(matches!(
        view.labs.items[0].runtime.state,
        ObservationState::Stale
    ));
    assert_eq!(view.labs.items[0].components.items[0].ready, None);
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["spec"]["workspaceId"] = json!("other");
    let view = labs
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert_eq!(
        view.labs.items[0].runtime.error.as_deref(),
        Some("runtime_identity_mismatch")
    );
    cluster.lock().unwrap().objects.remove(&path);
    let view = labs
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert!(matches!(
        view.labs.items[0].runtime.state,
        ObservationState::Missing
    ));
    assert_eq!(view.labs.items[0].components.items.len(), 1);
}

#[tokio::test]
async fn http_matches_shared_contract_and_refuses_writes_foreign_origins_and_revoked_reads() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster);
    labs.up("demo", &spec()).await.unwrap();
    let viewer = observer(&labs);
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
        wire["labs"]["items"][0]["id"],
        shared["labs"]["items"][0]["id"]
    );
    assert_eq!(
        wire["labs"]["items"][0]["resources"],
        shared["labs"]["items"][0]["resources"]
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
    store.replace_grants("local", "viewer", []).unwrap();
    assert_eq!(client.get(&url).send().await.unwrap().status(), 403);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn runtime_failure_keeps_history_visible_without_exposing_errors() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster.clone());
    labs.up("demo", &spec()).await.unwrap();
    cluster.lock().unwrap().fail_reads = true;
    let view = labs
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert!(matches!(
        view.labs.items[0].runtime.state,
        ObservationState::Unavailable
    ));
    assert_eq!(view.labs.items[0].components.items.len(), 1);
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("PRIVATE-RUNTIME-DETAIL")
    );
}

#[tokio::test]
async fn large_topology_pages_fit_the_shared_budget_without_losing_components_or_links() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster);
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
    let lab = labs.up("large", &spec).await.unwrap().lab;
    let mut query = EnvironmentQuery {
        instance_id: Some(lab.instance_id),
        limit: 50,
        ..Default::default()
    };
    let mut components = Vec::new();
    loop {
        let view = labs.environment(&query).await.unwrap();
        assert!(serde_json::to_vec(&view).unwrap().len() <= 24 * 1024);
        let item = &view.labs.items[0];
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
        let view = labs.environment(&query).await.unwrap();
        let item = &view.labs.items[0];
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
    let labs = service(store.clone(), cluster);
    let mut spec = spec();
    let mut second = spec.components[0].clone();
    second.id = "chain-two".into();
    spec.components.push(second);
    let lab = labs.up("demo", &spec).await.unwrap().lab;
    store
        .grant("local", "developer", Capability::NetworkPartition)
        .unwrap();
    store
        .create_operation(
            "local",
            "developer",
            &lab.instance_id,
            &lab.run_id(),
            "",
            "fault",
            proofstorm_core::OperationKind::NetworkPartition,
            &json!({"from_component":"chain","to_component":"chain-two"}),
            "fault",
            Capability::NetworkPartition,
        )
        .unwrap();
    let view = labs
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    assert_eq!(
        view.labs.items[0].activity.items[0].components,
        vec!["chain", "chain-two"]
    );
}
