use std::path::Path;

use proofstorm_acceptance::{McpClient, json as expect};
use serde_json::{Value, json};

fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_proofstorm-mcp"))
}

fn assert_resource_contract(client: &mut McpClient) {
    let templates = client
        .request("resources/templates/list", json!({}))
        .expect("list resource templates");
    expect::equals(
        &templates,
        "/resourceTemplates/0/uriTemplate",
        &Value::from("proofstorm://evidence/{experiment_id}/{digest}{?oracles,artifacts}"),
    )
    .expect("evidence resource template");

    let missing_resource = client
        .request_error("resources/read", json!({"uri": "proofstorm://unknown"}))
        .expect("unknown resource must be refused");
    expect::equals(
        &missing_resource,
        "/message",
        &Value::from("unknown Proofstorm resource URI"),
    )
    .expect("unknown resource message");
}

#[test]
fn stdio_server_advertises_exact_slice_one_tools() {
    let mut client = McpClient::spawn_bare(binary(), "proofstorm-test").expect("spawn");

    let initialized = client.initialize_result().clone();
    expect::equals(
        &initialized,
        "/serverInfo/name",
        &Value::from("proofstorm-mcp"),
    )
    .expect("server name");
    expect::object(&initialized, "/capabilities/resources").expect("resource capability");

    let listed = client.request("tools/list", json!({})).expect("list tools");
    let names = expect::array(&listed, "/tools")
        .expect("tool array")
        .iter()
        .map(|tool| expect::string(tool, "/name").expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "proofstorm_catalog_config_schema_read",
            "proofstorm_catalog_entry_read",
            "proofstorm_catalog_list",
            "proofstorm_lab_validate",
            "proofstorm_network_capabilities",
        ]
    );

    assert_resource_contract(&mut client);

    let catalog = client
        .call_response("proofstorm_catalog_list", json!({}))
        .expect("list catalog");
    let structured = catalog
        .pointer("/result/structuredContent")
        .expect("structured content");
    assert_eq!(
        expect::array(structured, "/items")
            .expect("catalog items")
            .len(),
        12
    );
    expect::within_bytes(structured, 8 * 1024, "catalog structured content")
        .expect("catalog fits the agent budget");
    expect::within_bytes(&catalog, 20 * 1024, "catalog wire response")
        .expect("catalog wire response fits");
}

#[test]
fn oversized_stdio_frame_fails_closed() {
    let mut client = McpClient::spawn_bare(binary(), "proofstorm-test").expect("spawn");

    client
        .send_raw(&vec![b'x'; 1024 * 1024 + 1])
        .expect("write oversized frame");
    client
        .expect_transport_closed()
        .expect("oversized frame must close the transport without a response");

    assert!(client.wait().expect("reap proofstorm-mcp").success());
}

#[test]
fn configured_stdio_discovery_and_direct_calls_are_capability_filtered() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("proofstorm.sqlite3");
    let mut client = McpClient::spawn(
        binary(),
        "proofstorm-policy-test",
        &[
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_WORKSPACE", "alpha".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "reader".as_ref()),
            ("PROOFSTORM_CAPABILITIES", "lab.read".as_ref()),
        ],
    )
    .expect("spawn configured proofstorm-mcp");

    let listed = client.request("tools/list", json!({})).expect("list tools");
    let names = expect::array(&listed, "/tools")
        .expect("tool array")
        .iter()
        .map(|tool| expect::string(tool, "/name").expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "proofstorm_lab_diff",
            "proofstorm_lab_read",
            "proofstorm_workspace_read"
        ]
    );

    let refused = client
        .call_error("proofstorm_lab_create", json!({}))
        .expect("lab create must be refused");
    expect::equals(&refused, "/message", &Value::from("tool not found")).expect("refusal message");
}
