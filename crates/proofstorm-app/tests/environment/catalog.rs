use super::*;

#[tokio::test]
async fn catalog_http_matches_agent_projection_and_enforces_read_permissions() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = observer(&service(store, cluster));
    cells
        .store
        .grant("local", "viewer", Capability::CatalogRead)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(proofstorm_app::http::serve_listener(
        cells.clone(),
        listener,
    ));
    let client = reqwest::Client::new();
    let selectors = json!({"query":"CDK","kinds":["mint"],"origins":["built_in"]});
    let query = serde_urlencoded::to_string([("selectors", selectors.to_string())]).unwrap();
    let url = format!("http://{address}/v1/catalog?{query}");
    let expected = proofstorm_app::catalog::read(
        &cells.store,
        "local",
        "viewer",
        &serde_json::from_value(selectors).unwrap(),
        32 * 1024,
    )
    .unwrap();
    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        client
            .get(&url)
            .header("origin", "https://unrelated.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(client.post(&url).send().await.unwrap().status(), 405);
    let denied = client
        .get(format!("http://{address}/v1/candidates?selectors=%7B%7D"))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    cells
        .store
        .grant("local", "viewer", Capability::CandidateRead)
        .unwrap();
    let empty = client
        .get(format!("http://{address}/v1/candidates?selectors=%7B%7D"))
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), 200);
    assert_eq!(empty.json::<Value>().await.unwrap()["items"], json!([]));
    server.abort();
}
