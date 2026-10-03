use super::*;

fn explicit_contract(work: &Path) {
    save(
        &work.join("benchmark-task.json"),
        &json!(context(work).task),
    )
    .unwrap();
}

fn discovery(server: &str, name: &str, args: Value, status: &str) -> Value {
    let mut row = tool("discovery", status);
    row["item"]["server"] = json!(server);
    row["item"]["tool"] = json!(name);
    row["item"]["arguments"] = args;
    row
}

#[test]
fn real_codex_discovery_lifecycle_is_retained_without_tool_points() {
    let work = tempfile::tempdir().unwrap();
    captured(work.path());
    explicit_contract(work.path());
    // Verbatim discovery lifecycle from the 2026-10-03 Astra O5 QA transcript.
    // No provider credentials, model output or payment data are included.
    let recorded: Vec<Value> = include_str!("fixtures/discovery-0.159.3.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut rows = vec![tool("one", "completed")];
    rows.extend(recorded.clone());
    rows.push(json!({"type":"turn.completed"}));
    fixture(work.path(), &rows);

    let output = retained(work.path()).unwrap();
    assert_eq!(output.calls.len(), 1);
    assert_eq!(output.calls[0].tool, "catalog_list");
    assert_eq!(output.calls[0].success, Some(true));
    assert_eq!(
        output.usage["neutral_discovery"],
        json!([recorded[1]["item"]])
    );
    assert_eq!(output.outcome, "completed");
    assert!(!output.unauthorized);
    assert!(output.telemetry_error.is_none());
}

#[test]
fn both_discovery_namespaces_require_complete_scoped_listing() {
    for server in ["codex", "proofstorm"] {
        for name in ["list_mcp_resources", "list_mcp_resource_templates"] {
            for args in [json!({}), json!({"server":"proofstorm","cursor":"page-2"})] {
                for status in ["completed", "failed", "in_progress"] {
                    let work = tempfile::tempdir().unwrap();
                    captured(work.path());
                    explicit_contract(work.path());
                    fixture(
                        work.path(),
                        &[
                            tool("one", "completed"),
                            discovery(server, name, args.clone(), status),
                            json!({"type":"turn.completed"}),
                        ],
                    );
                    let output = retained(work.path()).unwrap();
                    assert_eq!(
                        output.usage["neutral_discovery"].as_array().unwrap().len(),
                        1
                    );
                    assert!(!output.calls.iter().any(|call| call.tool == name));
                    assert!(!output.unauthorized);
                    assert_eq!(output.telemetry_error.is_some(), status == "in_progress");
                    assert_eq!(
                        output.calls.len(),
                        if status == "in_progress" { 2 } else { 1 }
                    );
                }
            }
        }
    }
}

#[test]
fn unscoped_malformed_and_resource_read_calls_are_not_neutral() {
    for server in ["codex", "proofstorm", "foreign"] {
        for (name, args) in [
            ("list_mcp_resources", json!({"server":"foreign"})),
            ("list_mcp_resources", json!({"cursor":12})),
            ("list_mcp_resources", json!({"unexpected":true})),
            ("list_mcp_resources", json!(null)),
            ("list_mcp_resources", json!("{}")),
            ("list_mcp_resources", json!({"server":null})),
            (
                "read_mcp_resource",
                json!({"server":"proofstorm","uri":"test://data"}),
            ),
        ] {
            for status in ["completed", "failed"] {
                let work = tempfile::tempdir().unwrap();
                captured(work.path());
                explicit_contract(work.path());
                fixture(
                    work.path(),
                    &[
                        tool("one", "completed"),
                        discovery(server, name, args.clone(), status),
                        json!({"type":"turn.completed"}),
                    ],
                );
                let output = retained(work.path()).unwrap();
                assert_eq!(output.usage["neutral_discovery"], json!([]));
                assert_eq!(output.calls.len(), 2);
                assert_eq!(output.calls[1].tool, name);
                assert!(output.unauthorized);
                assert_eq!(output.calls[1].success, Some(status == "completed"));
            }
        }
    }
}

#[test]
fn proxy_captured_calls_take_precedence_over_discovery_names() {
    for success in [true, false] {
        for name in ["list_mcp_resources", "list_mcp_resource_templates"] {
            let work = tempfile::tempdir().unwrap();
            explicit_contract(work.path());
            let args = json!({"server":"proofstorm"});
            fs::write(
                work.path().join("events.jsonl"),
                format!(
                    "{}\n{}\n",
                    json!({"kind":"start","id":1,"tool":name,"arguments":args}),
                    json!({"kind":"end","id":1,"success":success,"elapsed_ms":9}),
                ),
            )
            .unwrap();
            fixture(
                work.path(),
                &[
                    discovery(
                        "proofstorm",
                        name,
                        args,
                        if success { "completed" } else { "failed" },
                    ),
                    json!({"type":"turn.completed"}),
                ],
            );
            let output = retained(work.path()).unwrap();
            assert_eq!(output.usage["neutral_discovery"], json!([]));
            assert_eq!(output.calls.len(), 1);
            assert_eq!(output.calls[0].success, Some(success));
            assert_eq!(output.calls[0].elapsed_ms, 9);
            assert!(output.telemetry_error.is_none());
        }
    }
}

#[test]
fn foreign_server_listing_and_legacy_contracts_keep_prior_accounting() {
    for server in ["codex", "proofstorm", "foreign"] {
        for explicit in [true, false] {
            let work = tempfile::tempdir().unwrap();
            captured(work.path());
            if explicit {
                explicit_contract(work.path());
            }
            fixture(
                work.path(),
                &[
                    tool("one", "completed"),
                    discovery(server, "list_mcp_resources", json!({}), "failed"),
                    json!({"type":"turn.completed"}),
                ],
            );
            let output = retained(work.path()).unwrap();
            assert_eq!(
                output.calls.len(),
                if explicit && server != "foreign" {
                    1
                } else {
                    2
                }
            );
            assert_eq!(output.unauthorized, explicit && server == "foreign");
            if !explicit {
                assert!(output.usage.get("neutral_discovery").is_none());
                assert_eq!(output.calls[1].success, Some(false));
            }
        }
    }
}

#[test]
fn discovery_does_not_hide_mutated_lifecycles_or_missing_proxy_calls() {
    let work = tempfile::tempdir().unwrap();
    captured(work.path());
    explicit_contract(work.path());
    fixture(
        work.path(),
        &[
            discovery("proofstorm", "list_mcp_resources", json!({}), "in_progress"),
            discovery(
                "proofstorm",
                "list_mcp_resources",
                json!({"server":"proofstorm"}),
                "completed",
            ),
            json!({"type":"turn.completed"}),
        ],
    );
    let output = retained(work.path()).unwrap();
    assert!(output.telemetry_error.is_some());
    assert_eq!(output.calls.last().unwrap().tool, "telemetry_gap");
}
