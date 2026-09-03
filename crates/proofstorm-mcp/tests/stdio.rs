use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

fn exchange(stdin: &mut impl Write, stdout: &mut impl BufRead, message: &Value) -> Value {
    writeln!(stdin, "{message}").expect("write MCP frame");
    stdin.flush().expect("flush MCP frame");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read MCP frame");
    serde_json::from_str(&line).expect("parse MCP frame")
}

fn assert_resource_contract(stdin: &mut impl Write, stdout: &mut impl BufRead) {
    let templates = exchange(
        stdin,
        stdout,
        &json!({"jsonrpc": "2.0", "id": 20, "method": "resources/templates/list", "params": {}}),
    );
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        "proofstorm://evidence/{experiment_id}/{digest}{?oracles,artifacts}"
    );
    let missing_resource = exchange(
        stdin,
        stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "resources/read",
            "params": {"uri": "proofstorm://unknown"}
        }),
    );
    assert_eq!(
        missing_resource["error"]["message"],
        "unknown Proofstorm resource URI"
    );
}

#[test]
fn stdio_server_advertises_exact_slice_one_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_proofstorm-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proofstorm-mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    let initialized = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "proofstorm-test", "version": "0.1.0"}
            }
        }),
    );
    assert_eq!(initialized["id"], 1);
    assert!(initialized.get("result").is_some(), "{initialized}");
    assert_eq!(
        initialized["result"]["serverInfo"]["name"],
        "proofstorm-mcp"
    );
    assert!(
        initialized["result"]["capabilities"]["resources"].is_object(),
        "{initialized}"
    );

    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .expect("write initialized notification");
    stdin.flush().expect("flush initialized notification");

    let listed = exchange(
        &mut stdin,
        &mut stdout,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    );
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tool array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
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

    assert_resource_contract(&mut stdin, &mut stdout);

    let catalog = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "proofstorm_catalog_list", "arguments": {}}
        }),
    );
    assert_eq!(
        catalog["result"]["structuredContent"]["items"]
            .as_array()
            .map(Vec::len),
        Some(12)
    );
    assert!(
        serde_json::to_vec(&catalog["result"]["structuredContent"])
            .expect("serialize catalog result")
            .len()
            < 8 * 1024
    );
    assert!(
        serde_json::to_vec(&catalog)
            .expect("serialize wire result")
            .len()
            < 20 * 1024
    );

    child.kill().expect("stop proofstorm-mcp");
    child.wait().expect("reap proofstorm-mcp");
}

#[test]
fn oversized_stdio_frame_fails_closed() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_proofstorm-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn proofstorm-mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    let initialized = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "proofstorm-test", "version": "0.1.0"}
            }
        }),
    );
    assert!(initialized.get("result").is_some(), "{initialized}");

    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .expect("write initialized notification");
    stdin
        .write_all(&vec![b'x'; 1024 * 1024 + 1])
        .expect("write oversized frame");
    stdin.write_all(b"\n").expect("terminate oversized frame");
    stdin.flush().expect("flush oversized frame");

    let mut response = String::new();
    assert_eq!(
        stdout.read_line(&mut response).expect("read transport EOF"),
        0,
        "oversized frame must close the transport without a response"
    );
    drop(stdin);
    assert!(child.wait().expect("reap proofstorm-mcp").success());
}

#[test]
fn configured_stdio_discovery_and_direct_calls_are_capability_filtered() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("proofstorm.sqlite3");
    let mut child = Command::new(env!("CARGO_BIN_EXE_proofstorm-mcp"))
        .env("PROOFSTORM_DB", database)
        .env("PROOFSTORM_WORKSPACE", "alpha")
        .env("PROOFSTORM_PRINCIPAL", "reader")
        .env("PROOFSTORM_CAPABILITIES", "lab.read")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn configured proofstorm-mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    let initialized = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "proofstorm-policy-test", "version": "0.1.0"}
            }
        }),
    );
    assert!(initialized.get("result").is_some(), "{initialized}");
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .expect("write initialized notification");
    stdin.flush().expect("flush initialized notification");

    let listed = exchange(
        &mut stdin,
        &mut stdout,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    );
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tool array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "proofstorm_lab_diff",
            "proofstorm_lab_read",
            "proofstorm_workspace_read"
        ]
    );

    let refused = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "proofstorm_lab_create", "arguments": {}}
        }),
    );
    assert_eq!(refused["error"]["message"], "tool not found", "{refused}");

    child.kill().expect("stop proofstorm-mcp");
    child.wait().expect("reap proofstorm-mcp");
}
