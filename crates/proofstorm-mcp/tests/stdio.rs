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
        15
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

#[test]
fn private_transfer_stdio_requires_method_fields_before_operation_admission() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("proofstorm.sqlite3");
    let mut client = McpClient::spawn(
        binary(),
        "private-transfer-contract",
        &[
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_WORKSPACE", "alpha".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "agent".as_ref()),
            (
                "PROOFSTORM_CAPABILITIES",
                "component.exec_live,artifact.read".as_ref(),
            ),
            ("PROOFSTORM_TOOLSET", "experiment".as_ref()),
        ],
    )
    .expect("spawn configured MCP without Kubernetes");
    let listed = client.request("tools/list", json!({})).unwrap();
    let tool = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "proofstorm_private_transfer")
        .unwrap();
    assert_private_transfer_schema(tool);
    let request = |transfer| {
        json!({"instance_id":"unmaterialized", "experiment_id":"test",
        "lease_id":"test", "operation_id":"must-not-exist", "idempotency_key":"test", "transfer":transfer})
    };
    for (transfer, field) in invalid_private_transfer_requests() {
        let response = client
            .call_response("proofstorm_private_transfer", request(transfer))
            .unwrap();
        // rmcp returns parameter decoding failures as a textual tool error.
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(field),
            "{response}"
        );
    }
    for size in [0, 1_048_577] {
        let error = client.call_error("proofstorm_private_transfer", request(json!({
            "transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":size
        }))).unwrap();
        assert!(
            error["message"].as_str().unwrap().contains("maximumBytes"),
            "{error}"
        );
    }
    // A complete synthetic request passes decoding and static validation, then
    // reaches the expected missing-instance boundary without a live cluster.
    let error = client.call_error("proofstorm_private_transfer", request(json!({
        "transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":65536
    }))).unwrap();
    assert_eq!(error["data"]["code"], "not_found", "{error}");
    let store = proofstorm_store::Store::open(&database).unwrap();
    assert!(matches!(
        store.operation("alpha", "agent", "must-not-exist"),
        Err(proofstorm_store::StoreError::NotFound {
            resource: "operation",
            ..
        })
    ));
}

fn assert_private_transfer_schema(tool: &Value) {
    let schema = &tool["inputSchema"];
    let reference = schema["properties"]["transfer"]["$ref"].as_str().unwrap();
    let transfer = schema
        .pointer(reference.strip_prefix('#').unwrap())
        .unwrap();
    let branches = transfer["oneOf"]
        .as_array()
        .expect("method-specific schema");
    assert_eq!(branches.len(), 5);
    for branch in branches {
        let method = branch["properties"]["transferMethod"]["const"]
            .as_str()
            .unwrap();
        let required = branch["required"].as_array().unwrap();
        for field in if method == "prepare" {
            vec![
                "transferMethod",
                "component",
                "destinationComponent",
                "maximumBytes",
            ]
        } else if method == "handoff" {
            vec![
                "transferMethod",
                "component",
                "reference",
                "recipientLeaseId",
            ]
        } else {
            vec!["transferMethod", "component", "reference"]
        } {
            assert!(
                required.contains(&json!(field)),
                "{method} must require {field}"
            );
        }
        assert_eq!(branch["additionalProperties"], false);
    }
}

fn invalid_private_transfer_requests() -> Vec<(Value, &'static str)> {
    vec![
        (
            json!({"transferMethod":"prepare","component":"wallet-a"}),
            "destinationComponent",
        ),
        (
            json!({"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b"}),
            "maximumBytes",
        ),
        (
            json!({"transferMethod":"prepare","component":"wallet-a","destinationComponent":null,"maximumBytes":65536}),
            "string",
        ),
        (
            json!({"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":null}),
            "u32",
        ),
        (
            json!({"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":65536,"reference":"wrong-method"}),
            "reference",
        ),
        (
            json!({"transferMethod":"handoff","component":"wallet-a","reference":"opaque"}),
            "recipientLeaseId",
        ),
        (
            json!({"transferMethod":"status","component":"wallet-a"}),
            "reference",
        ),
        (
            json!({"transferMethod":"deliver","component":"wallet-a"}),
            "reference",
        ),
        (
            json!({"transferMethod":"release","component":"wallet-a"}),
            "reference",
        ),
        (
            json!({"transferMethod":"deliver","component":"wallet-a","reference":"opaque","maximumBytes":1}),
            "maximumBytes",
        ),
    ]
}
