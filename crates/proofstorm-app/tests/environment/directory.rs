use super::*;
use proofstorm_app::environment::EnvironmentReadQuery;

fn query(value: Value) -> EnvironmentReadQuery {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn scans_and_header_fields_skip_history_resources_and_per_cell_runtime_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let created = cells.up("scan-only", &spec()).await.unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    // These records would fail if the scan decoded requested-only sections.
    db.execute("UPDATE revisions SET revision_json='broken'", [])
        .unwrap();
    db.execute("UPDATE sessions SET phase_json='broken'", [])
        .unwrap();
    let before = store.observation_token("local", "developer").unwrap();
    cluster.lock().unwrap().requests.clear();
    let scan = cells
        .environment_read(&query(json!({"scan":true})), 8192)
        .await
        .unwrap();
    assert_eq!(scan["matched_count"], 1);
    let header = &scan["cells"]["items"][0];
    assert_eq!(header["name"], "scan-only");
    for section in ["activity", "sessions", "resources", "components", "links"] {
        assert!(header.get(section).is_none());
    }
    let requests = cluster.lock().unwrap().requests.clone();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].1.ends_with("/proofstormcells"));
    let selected = cells.environment_read(&query(json!({"instance_id":created.cell.instance_id,"fields":["/name","/desired_generation"]})),8192).await.unwrap();
    assert_eq!(
        selected["cells"]["items"][0]["selected"],
        json!({"/name":"scan-only","/desired_generation":1})
    );
    let unavailable = cells
        .environment_read(
            &query(json!({"scan":true,"component_kind":"bitcoin"})),
            8192,
        )
        .await
        .unwrap();
    assert_eq!(unavailable["unavailable_count"], 1);
    assert_eq!(unavailable["matched_count"], 0);
    assert_eq!(
        store.observation_token("local", "developer").unwrap(),
        before
    );
}

#[tokio::test]
async fn filtered_directories_page_all_matches_and_reject_changed_queries_or_membership() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    for index in 0..32 {
        cells
            .up(&format!("fleet-{index:02}"), &spec())
            .await
            .unwrap();
    }
    let mut request = query(
        json!({"scan":true,"owner":"developer","component_kind":"bitcoin","implementation":"bitcoin-core","query":"FLEET-","case_insensitive":true,"limit":50}),
    );
    let first = cells.environment_read(&request, 4096).await.unwrap();
    assert_eq!(first["matched_count"], 32);
    assert!(first["cells"]["items"].as_array().unwrap().len() < 32);
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let result = cells.environment_read(&request, 4096).await.unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= 4096);
        for cell in result["cells"]["items"].as_array().unwrap() {
            assert!(seen.insert(cell["id"].as_str().unwrap().to_owned()));
        }
        let Some(cursor) = result["cells"]["next_cursor"].as_str() else {
            break;
        };
        request.cursor = cursor.into();
    }
    assert_eq!(seen.len(), 32);
    request.cursor = first["cells"]["next_cursor"].as_str().unwrap().into();
    let mut changed = request.clone();
    changed.query = "fleet-0".into();
    assert_eq!(
        cells
            .environment_read(&changed, 4096)
            .await
            .unwrap_err()
            .details
            .unwrap()["code"],
        "environment_cursor_invalid"
    );
    cells.up("fleet-new", &spec()).await.unwrap();
    assert_eq!(
        cells
            .environment_read(&request, 4096)
            .await
            .unwrap_err()
            .details
            .unwrap()["code"],
        "environment_cursor_invalid"
    );
}

#[tokio::test]
async fn component_field_pages_fit_without_rendering_resources_or_losing_ids() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    let mut fleet = spec();
    fleet.components = (0..160)
        .map(|index| {
            let mut c = fleet.components[0].clone();
            c.id = format!("chain-{index:03}");
            c
        })
        .collect();
    let created = cells.up("large-fleet", &fleet).await.unwrap();
    cluster.lock().unwrap().requests.clear();
    let mut request = query(
        json!({"instance_id":created.cell.instance_id,"fields":["/components/items"],"limit":50}),
    );
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let result = cells.environment_read(&request, 8192).await.unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= 8192);
        let cell = &result["cells"]["items"][0];
        let components = cell["selected"]["/components/items"].as_array().unwrap();
        assert!(!components.is_empty());
        for component in components {
            assert!(seen.insert(component["id"].as_str().unwrap().to_owned()));
        }
        let Some(cursor) = cell["continuations"]["components"].as_str() else {
            break;
        };
        request.component_cursor = cursor.into();
    }
    assert_eq!(seen.len(), 160);
    assert!(
        cluster
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, path)| method == "GET" && !path.contains("/deployments"))
    );
    let targeted = cells
        .environment_read(
            &query(json!({"instance_id":request.instance_id,"fields":["/components/items/0/id"]})),
            8192,
        )
        .await
        .unwrap();
    assert_eq!(
        targeted["cells"]["items"][0]["selected"]["/components/items/0/id"],
        "chain-000"
    );
}

#[tokio::test]
async fn default_and_http_queries_preserve_the_gui_view_and_share_selection_semantics() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster);
    cells.up("gui", &spec()).await.unwrap();
    let full = cells
        .environment_read(&EnvironmentReadQuery::default(), 24576)
        .await
        .unwrap();
    for section in ["components", "links", "resources", "sessions", "activity"] {
        assert!(full["cells"]["items"][0].get(section).is_some());
    }
    let request: EnvironmentReadQuery =
        serde_urlencoded::from_str("name=gui&fields=%2Fname%2C%2Fdesired_generation").unwrap();
    let selected = cells.environment_read(&request, 8192).await.unwrap();
    assert_eq!(
        selected["cells"]["items"][0]["selected"],
        json!({"/name":"gui","/desired_generation":1})
    );
    let invalid = query(json!({"scan":true,"sections":["activity"]}));
    assert!(cells.environment_read(&invalid, 8192).await.is_err());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(proofstorm_app::http::serve_listener(cells, listener));
    let response = reqwest::get(format!(
        "http://{address}/v1/environment?name=gui&fields=%2Fname%2C%2Fdesired_generation"
    ))
    .await
    .unwrap();
    assert!(response.status().is_success());
    let http: Value = response.json().await.unwrap();
    assert_eq!(
        http["cells"]["items"][0]["selected"],
        selected["cells"]["items"][0]["selected"]
    );
    server.abort();
}

#[tokio::test]
async fn compact_scans_can_search_large_messages_and_report_unavailable_selected_sections() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let store = Store::open(&path).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    let created = cells.up("diagnostics", &spec()).await.unwrap();
    for object in cluster
        .lock()
        .unwrap()
        .objects
        .values_mut()
        .filter(|o| o["kind"] == "ProofstormCell")
    {
        object["status"]["message"] = json!(format!("unique-failure{}", "x".repeat(100_000)));
    }
    let scan = cells
        .environment_read(&query(json!({"scan":true,"query":"unique-failure"})), 8192)
        .await
        .unwrap();
    assert_eq!(scan["matched_count"], 1);
    assert_eq!(
        scan["cells"]["items"][0]["runtime"]["message_omitted"],
        true
    );
    assert!(
        scan["cells"]["items"][0]["runtime"]
            .get("message")
            .is_none()
    );
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute("UPDATE sessions SET phase_json='broken'", [])
        .unwrap();
    let result = cells
        .environment_read(
            &query(json!({"instance_id":created.cell.instance_id,"sections":["sessions"]})),
            8192,
        )
        .await
        .unwrap();
    assert_eq!(
        result["cells"]["items"][0]["read_error"],
        "stored_record_incompatible"
    );
    assert!(result["cells"]["items"][0]["sessions"].is_null());
}
