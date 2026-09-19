use std::path::Path;

use proofstorm_acceptance::{McpClient, json as expect};
use serde_json::{Value, json};

fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_proofstorm-mcp"))
}

#[test]
fn release_metadata_exits_without_starting_transport_or_resolving_identity() {
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(binary())
        .current_dir(directory.path())
        .env("PROOFSTORM_HOME", directory.path().join("missing"))
        .env("PROOFSTORM_PRINCIPAL", "")
        .arg("--release-info")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn isolated_stdio_uses_the_selected_home_for_persistent_state() {
    let directory = tempfile::tempdir().unwrap();
    let installation =
        proofstorm_app::installation::Installation::initialize(directory.path(), None, None)
            .unwrap();
    let mut client = McpClient::spawn(
        binary(),
        "isolated-home-test",
        &[
            ("PROOFSTORM_HOME", installation.home.as_os_str()),
            ("PROOFSTORM_MODE", "offline".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "isolated-reader".as_ref()),
            ("PROOFSTORM_CAPABILITIES", "catalog.read".as_ref()),
        ],
    )
    .unwrap();
    let listed = client.request("tools/list", json!({})).unwrap();
    assert!(!listed["tools"].as_array().unwrap().is_empty());
    assert!(installation.database().is_file());
    let store = proofstorm_store::Store::open(installation.database()).unwrap();
    assert!(
        !store
            .capabilities(proofstorm_app::config::DEFAULT_WORKSPACE, "isolated-reader")
            .unwrap()
            .is_empty()
    );
    assert!(!installation.kubeconfig().exists());
}

#[test]
fn connected_stdio_refuses_ambient_cluster_before_creating_authority() {
    let directory = tempfile::tempdir().unwrap();
    let ambient = disconnected_kubeconfig(directory.path());
    let original = std::fs::read(&ambient).unwrap();
    for selector in [
        None,
        Some("PROOFSTORM_CONTEXT"),
        Some("PROOFSTORM_KUBECONFIG"),
    ] {
        let mut command = std::process::Command::new(binary());
        proofstorm_acceptance::client::clear_runtime_environment(&mut command);
        command
            .current_dir(directory.path())
            .env("PROOFSTORM_PRINCIPAL", "agent")
            .env("KUBECONFIG", &ambient);
        match selector {
            Some("PROOFSTORM_CONTEXT") => {
                command.env("PROOFSTORM_CONTEXT", "disconnected-test");
            }
            Some(_) => {
                command.env("PROOFSTORM_KUBECONFIG", &ambient);
            }
            None => {}
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires both"));
        assert!(!directory.path().join(".proofstorm").exists());
        assert_eq!(std::fs::read(&ambient).unwrap(), original);
    }
}

fn assert_resource_contract(client: &mut McpClient) {
    let templates = client
        .request("resources/templates/list", json!({}))
        .expect("list resource templates");
    expect::equals(
        &templates,
        "/resourceTemplates/0/uriTemplate",
        &Value::from("proofstorm://evidence/{run_id}/{digest}{?oracles,artifacts}"),
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
fn stdio_default_developer_discovery_respects_unconfigured_authority() {
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
            "catalog_config_schema_read",
            "catalog_entry_read",
            "catalog_list",
            "network_capabilities",
        ]
    );

    assert_resource_contract(&mut client);

    let mut cursor = Value::Null;
    let mut identities = std::collections::BTreeSet::new();
    loop {
        let catalog = client
            .call_response("catalog_list", json!({"cursor":cursor}))
            .expect("list catalog");
        let structured = catalog
            .pointer("/result/structuredContent")
            .expect("structured content");
        assert_eq!(structured["matched_count"], 17);
        let items = expect::array(structured, "/items").expect("catalog items");
        assert!(!items.is_empty());
        for item in items {
            assert!(identities.insert((item["id"].to_string(), item["version"].to_string())));
        }
        expect::within_bytes(structured, 8 * 1024, "catalog structured content")
            .expect("catalog fits the agent budget");
        expect::within_bytes(&catalog, 20 * 1024, "catalog wire response")
            .expect("catalog wire response fits");
        cursor = structured["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(identities.len(), 17);
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
            ("PROOFSTORM_MODE", "offline".as_ref()),
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_WORKSPACE", "alpha".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "reader".as_ref()),
            ("PROOFSTORM_CAPABILITIES", "cell.read".as_ref()),
        ],
    )
    .expect("spawn configured proofstorm-mcp");

    let listed = client.request("tools/list", json!({})).expect("list tools");
    let names = expect::array(&listed, "/tools")
        .expect("tool array")
        .iter()
        .map(|tool| expect::string(tool, "/name").expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["cell_read", "cell_search"]);

    let refused = client
        .call_error("cell_create", json!({}))
        .expect("cell create must be refused");
    expect::equals(&refused, "/message", &Value::from("tool not found")).expect("refusal message");
}

#[test]
fn recorded_search_and_selected_reads_work_over_offline_stdio() {
    use proofstorm_core::{Capability, OperationKind, OperationPhase};
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("recorded.sqlite3");
    let store = proofstorm_store::Store::open(&database).unwrap();
    proofstorm_app::developer::configure(&store, "test", "agent").unwrap();
    let spec = serde_json::from_value(json!({
        "api_version":"proofstorm/v1alpha1", "name":"search-demo", "links":[],
        "components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core",
            "version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]
    }))
    .unwrap();
    store
        .create_draft("test", "agent", "draft", &spec, "draft")
        .unwrap();
    let revision = store
        .publish("test", "agent", "draft", 1, "publish")
        .unwrap();
    store
        .materialize("test", "agent", "search-demo", &revision.digest, "up")
        .unwrap();
    store
        .create_operation(
            "test",
            "agent",
            "search-demo",
            "",
            "",
            "recorded-command",
            OperationKind::ComponentExecLive,
            &json!({"component":"chain","argv":["example"]}),
            "recorded-command",
            Capability::ComponentExecLive,
        )
        .unwrap();
    store
        .record_operation_result(
            "test",
            "recorded-command",
            OperationPhase::Succeeded,
            json!({"stdout":"database busy", "exit_code":1}),
        )
        .unwrap();
    let before = store
        .activity_observation_digest("test", "agent", "search-demo")
        .unwrap();
    let mut client = McpClient::spawn(
        binary(),
        "recorded-search",
        &[
            ("PROOFSTORM_MODE", "offline".as_ref()),
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_WORKSPACE", "test".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "agent".as_ref()),
        ],
    )
    .unwrap();
    let found = client
        .call(
            "activity_search",
            json!({"name":"search-demo", "query":"database",
        "native_exit_code":1, "fields":["/artifact/content/exit_code"]}),
        )
        .unwrap();
    assert_eq!(found["items"][0]["operation_id"], "recorded-command");
    assert_eq!(found["items"][0]["fields"][0]["value"], 1);
    let receipt = client
        .call(
            "operation_read",
            json!({
                "operation_id":found["items"][0]["operation_id"],
                "expected_digest":found["items"][0]["operation_digest"],
                "pointer":found["items"][0]["matches"][0]["pointer"], "offset":9,"limit":4
            }),
        )
        .unwrap();
    assert_eq!(receipt["value"], "busy");
    assert!(receipt["next_offset"].is_null());
    let sessions = client
        .call(
            "session_list",
            json!({"instance_id":"search-demo","principal_id":"agent","scan":true}),
        )
        .unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    let session = client
        .call(
            "session_list",
            json!({"id":sessions["sessions"][0]["id"],"fields":["/id","/principal_id"]}),
        )
        .unwrap();
    assert_eq!(session["sessions"][0]["/principal_id"], "agent");
    assert_eq!(
        before,
        store
            .activity_observation_digest("test", "agent", "search-demo")
            .unwrap()
    );
}

#[test]
fn private_transfer_stdio_requires_method_fields_before_operation_admission() {
    let directory = tempfile::tempdir().expect("tempdir");
    let kubeconfig = disconnected_kubeconfig(directory.path());
    let database = directory.path().join("proofstorm.sqlite3");
    let mut client = McpClient::spawn(
        binary(),
        "private-transfer-contract",
        &[
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_KUBECONFIG", kubeconfig.as_os_str()),
            ("PROOFSTORM_CONTEXT", "disconnected-test".as_ref()),
            ("PROOFSTORM_WORKSPACE", "alpha".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "agent".as_ref()),
            (
                "PROOFSTORM_CAPABILITIES",
                "component.exec_live,artifact.read".as_ref(),
            ),
        ],
    )
    .expect("spawn configured MCP without Kubernetes");
    let listed = client.request("tools/list", json!({})).unwrap();
    let tool = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "private_transfer")
        .unwrap();
    assert_private_transfer_schema(tool);
    let request = |transfer| json!({"name":"unmaterialized", "run_id":"test", "request_id":"must-not-exist", "transfer":transfer});
    for (transfer, field) in invalid_private_transfer_requests() {
        let response = client
            .call_response("private_transfer", request(transfer))
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
        let error = client.call_error("private_transfer", request(json!({
            "transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":size
        }))).unwrap();
        assert!(
            error["message"].as_str().unwrap().contains("maximumBytes"),
            "{error}"
        );
    }
    // A complete synthetic request passes decoding and static validation, then
    // reaches the expected missing-instance boundary without a live cluster.
    let error = client.call_error("private_transfer", request(json!({
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
    let branches = transfer["anyOf"]
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
                "recipientGrantId",
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
            "recipientGrantId",
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

#[test]
fn default_surface_exposes_the_complete_registry_without_manual_coordination() {
    let directory = tempfile::tempdir().unwrap();
    let kubeconfig = disconnected_kubeconfig(directory.path());
    let database = directory.path().join("developer.sqlite3");
    let store = proofstorm_store::Store::open(&database).unwrap();
    proofstorm_app::developer::configure(&store, "local", "developer").unwrap();
    let mut client = McpClient::spawn(
        binary(),
        "developer-discovery",
        &[
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_KUBECONFIG", kubeconfig.as_os_str()),
            ("PROOFSTORM_CONTEXT", "disconnected-test".as_ref()),
            ("PROOFSTORM_WORKSPACE", "local".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "developer".as_ref()),
        ],
    )
    .unwrap();
    let listed = client.request("tools/list", json!({})).unwrap();
    let names = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected = proofstorm_core::mcp::TOOLS
        .iter()
        .map(|t| t.name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert_selectors_advertised(&listed);
    for name in [
        "session_list",
        "cell_up",
        "cell_inspect",
        "cell_read",
        "cell_search",
        "environment_read",
        "cell_exec",
        "cell_sync",
        "activity_search",
        "operation_read",
        "cell_remove",
    ] {
        assert!(names.contains(&name));
    }
    for name in [
        "experiment_create",
        "session_start",
        "cell_recipe_bootstrap",
        "wallet_pay",
    ] {
        assert!(!names.contains(&name));
    }
    assert!(
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":listed}))
            .unwrap()
            .len()
            < 128 * 1024
    );
    assert!(
        client.initialize_result()["instructions"]
            .as_str()
            .unwrap()
            .contains("cell_up")
    );
}

fn assert_selectors_advertised(listed: &serde_json::Value) {
    for (name, fields) in [
        (
            "cell_up",
            vec!["expected_generation", "expected_instance_key"],
        ),
        ("cell_inspect", vec!["fields"]),
        (
            "environment_read",
            vec![
                "scan",
                "name",
                "owner",
                "phase",
                "component_kind",
                "implementation",
                "query",
                "sections",
                "fields",
                "cursor",
            ],
        ),
        (
            "session_list",
            vec![
                "id",
                "overlaps_with",
                "principal_id",
                "run_id",
                "phase",
                "started_after_unix",
                "last_activity_before_unix",
                "query",
                "scan",
                "fields",
                "cursor",
            ],
        ),
        (
            "cell_search",
            vec!["id", "scan", "query", "fields", "cursor"],
        ),
        (
            "cell_component_status_list",
            vec![
                "component",
                "ready",
                "scan",
                "query",
                "regex",
                "fields",
                "cursor",
            ],
        ),
    ] {
        let tool = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        for field in fields {
            assert!(
                tool["inputSchema"]["properties"].get(field).is_some(),
                "{name} must advertise {field}"
            );
            assert!(
                tool["inputSchema"]["required"]
                    .as_array()
                    .is_none_or(|required| !required.contains(&json!(field))),
                "existing callers may omit {field}"
            );
        }
    }
}

fn disconnected_kubeconfig(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("kubeconfig");
    std::fs::write(&path, "apiVersion: v1\nkind: Config\ncurrent-context: other\ncontexts:\n- name: disconnected-test\n  context: {cluster: test, user: test}\nclusters:\n- name: test\n  cluster: {server: 'http://127.0.0.1:1'}\nusers:\n- name: test\n  user: {}\n").unwrap();
    path
}

#[test]
fn offline_mode_uses_existing_grants_without_replacing_them() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("existing.db");
    let store = proofstorm_store::Store::open(&database).unwrap();
    store
        .put_workspace(&proofstorm_store::Workspace {
            id: "local-cell".into(),
            name: "local-cell".into(),
        })
        .unwrap();
    store.put_principal("reader").unwrap();
    store
        .grant(
            "local-cell",
            "reader",
            proofstorm_core::Capability::CellRead,
        )
        .unwrap();
    let mut client = McpClient::spawn(
        binary(),
        "existing-grants",
        &[
            ("PROOFSTORM_MODE", "offline".as_ref()),
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_PRINCIPAL", "reader".as_ref()),
        ],
    )
    .unwrap();
    let listed = client.request("tools/list", json!({})).unwrap();
    let names = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"cell_read"));
    assert!(!names.contains(&"cell_apply"));
    assert!(!names.contains(&"component_exec_live"));
    assert_eq!(
        store.capabilities("local-cell", "reader").unwrap(),
        [proofstorm_core::Capability::CellRead].into()
    );
}

#[test]
fn missing_agent_identity_fails_instead_of_starting_an_ephemeral_service() {
    let mut command = std::process::Command::new(binary());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PROOFSTORM_") {
            command.env_remove(key);
        }
    }
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("PROOFSTORM_PRINCIPAL"));
    assert!(output.stdout.is_empty());
}

#[test]
fn public_calls_recheck_the_entire_registry_after_live_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let kubeconfig = disconnected_kubeconfig(directory.path());
    let database = directory.path().join("proofstorm.sqlite3");
    let mut client = McpClient::spawn(
        binary(),
        "revocation-contract",
        &[
            ("PROOFSTORM_DB", database.as_os_str()),
            ("PROOFSTORM_KUBECONFIG", kubeconfig.as_os_str()),
            ("PROOFSTORM_CONTEXT", "disconnected-test".as_ref()),
            ("PROOFSTORM_WORKSPACE", "alpha".as_ref()),
            ("PROOFSTORM_PRINCIPAL", "agent".as_ref()),
            (
                "PROOFSTORM_CAPABILITIES",
                "experiment.close,experiment.read,artifact.read".as_ref(),
            ),
        ],
    )
    .unwrap();
    let discovery = client.request("tools/list", json!({})).unwrap();
    assert!(
        discovery["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "run_finish")
    );
    proofstorm_store::Store::open(&database)
        .unwrap()
        .revoke("alpha", "agent", proofstorm_core::Capability::ArtifactRead)
        .unwrap();
    let error = client
        .call_error(
            "run_finish",
            json!({"run_id":"absent-run","request_id":"denied"}),
        )
        .unwrap();
    assert_eq!(error["data"]["code"], "access_denied");
}

#[test]
fn incompatible_managed_startup_refuses_before_opening_shared_database() {
    let root = tempfile::tempdir().unwrap();
    let installation = proofstorm_app::installation::Installation::initialize(
        root.path(),
        Some(12341),
        Some(12342),
    )
    .unwrap();
    let sentinel = b"not a database: prove startup never opens it";
    std::fs::write(installation.database(), sentinel).unwrap();
    let mut command = std::process::Command::new(binary());
    proofstorm_acceptance::client::clear_runtime_environment(&mut command);
    let output = command
        .arg("--home")
        .arg(root.path())
        .args(["--attachment", "unconfigured-test"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains("not a database"), "{error}");
    assert_eq!(std::fs::read(installation.database()).unwrap(), sentinel);
    assert!(
        !installation
            .database()
            .with_extension("sqlite3-wal")
            .exists()
    );
}
