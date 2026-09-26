use super::*;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;

fn context(work: &Path) -> Context {
    Context {
        root: work.into(),
        work: work.into(),
        home: work.join("state"),
        mcp: "/unused/mcp".into(),
        model: "gpt-6-astra".into(),
        harness: Harness::Codex {
            executable: "codex".into(),
            auth_file: Some(work.join("source-auth.json")),
        },
        task: crate::benchmark::task::o1().clone(),
    }
}
fn fixture(work: &Path, rows: &[Value]) {
    save(
        &work.join("benchmark-attempt.json"),
        &json!({"outcome":"completed","elapsed_seconds":5.0}),
    )
    .unwrap();
    fs::write(
        work.join("harness.jsonl"),
        rows.iter().fold(String::new(), |mut text, row| {
            writeln!(text, "{row}").unwrap();
            text
        }),
    )
    .unwrap();
}
fn tool(id: &str, status: &str) -> Value {
    json!({"type":if status=="in_progress" {"item.started"} else {"item.completed"},"item":{
        "id":id,"type":"mcp_tool_call","server":"proofstorm","tool":"catalog_list","arguments":{},"status":status,"error":null,"result":null}})
}
fn captured(work: &Path) {
    fs::write(work.join("events.jsonl"), "{\"kind\":\"start\",\"id\":1,\"tool\":\"catalog_list\",\"arguments\":{}}\n{\"kind\":\"end\",\"id\":1,\"success\":true,\"elapsed_ms\":9}\n").unwrap();
}
#[test]
fn codex_lifecycles_reconcile_once_and_preserve_usage_and_report() {
    let work = tempfile::tempdir().unwrap();
    captured(work.path());
    fixture(
        work.path(),
        &[
            tool("one", "in_progress"),
            tool("one", "completed"),
            tool("one", "completed"),
            json!({"type":"item.completed","item":{"id":"report","type":"agent_message","text":"{\"success\":false}"}}),
            json!({"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":4}}),
        ],
    );
    let output = retained(work.path()).unwrap();
    assert_eq!(output.outcome, "completed");
    assert_eq!(output.calls.len(), 1);
    assert_eq!(output.calls[0].success, Some(true));
    assert_eq!(output.calls[0].elapsed_ms, 9);
    assert_eq!(output.final_text, "{\"success\":false}");
    assert_eq!(output.usage["turns"][0]["input_tokens"], 12);
    assert!(output.usage["cost"].is_null());
    assert!(!output.unauthorized);
    assert!(output.telemetry_error.is_none());
}
#[test]
fn failed_or_missing_harness_reply_never_borrows_proxy_success() {
    for status in ["failed", "in_progress"] {
        let work = tempfile::tempdir().unwrap();
        captured(work.path());
        fixture(
            work.path(),
            &[tool("one", status), json!({"type":"turn.completed"})],
        );
        assert_eq!(
            retained(work.path()).unwrap().calls[0].success,
            if status == "failed" {
                Some(false)
            } else {
                None
            }
        );
    }
    let work = tempfile::tempdir().unwrap();
    fixture(
        work.path(),
        &[tool("rejected", "failed"), json!({"type":"turn.completed"})],
    );
    let output = retained(work.path()).unwrap();
    assert_eq!(output.calls.len(), 1);
    assert_eq!(output.calls[0].success, Some(false));
}
#[test]
fn interruptions_truncation_foreign_tools_and_errors_stay_visible() {
    let work = tempfile::tempdir().unwrap();
    fixture(
        work.path(),
        &[
            json!({"type":"item.completed","item":{"id":"shell","type":"command_execution","status":"completed"}}),
            json!({"type":"turn.failed","error":{"message":"provider refused"}}),
        ],
    );
    let output = retained(work.path()).unwrap();
    assert_eq!(output.outcome, "provider_or_harness_failure");
    assert!(output.unauthorized);
    assert!(output.calls[0].success.is_none());
    save(
        &work.path().join("benchmark-attempt.json"),
        &json!({"outcome":"running"}),
    )
    .unwrap();
    fs::write(work.path().join("harness.jsonl"), "{truncated").unwrap();
    let output = retained(work.path()).unwrap();
    assert_eq!(output.outcome, "interrupted");
    assert!(output.telemetry_error.is_some());
    assert_eq!(output.calls[0].tool, "telemetry_gap");
    fixture(
        work.path(),
        &[
            json!({"type":"future.unknown"}),
            json!({"type":"turn.completed"}),
        ],
    );
    assert!(retained(work.path()).unwrap().telemetry_error.is_some());
}
#[test]
fn model_selection_is_exact_and_controlled_profile_preserves_original() {
    let original = json!({"slug":"exact","base_instructions":"native instructions","tool_mode":"code_mode_only","apply_patch_tool_type":"freeform","multi_agent_version":"v2","experimental_supported_tools":["clock"]});
    let catalog = json!({"models":[original]});
    assert!(config::controlled_model(&catalog, "alias").is_err());
    let selected = config::controlled_model(&catalog, "exact").unwrap();
    assert_eq!(selected["original_model"], original);
    assert_eq!(selected["models"][0]["slug"], "exact");
    assert_eq!(
        selected["models"][0]["base_instructions"],
        "native instructions"
    );
    assert_eq!(selected["models"][0]["tool_mode"], "code_mode_only");
    assert!(selected["models"][0]["apply_patch_tool_type"].is_null());
    assert!(selected["models"][0]["multi_agent_version"].is_null());
}
#[test]
fn auth_is_private_copied_not_linked_and_parent_can_scrub_interrupted_runs() {
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    fs::create_dir(work.path().join("codex-home")).unwrap();
    let source = work.path().join("source-auth.json");
    fs::write(&source, r#"{"OPENAI_API_KEY":"fixture-only"}"#).unwrap();
    config::copy_auth(&ctx).unwrap();
    let auth = work.path().join("codex-home/auth.json");
    assert_eq!(
        fs::metadata(&auth).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&auth, r#"{"tokens":"refreshed copy"}"#).unwrap();
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        r#"{"OPENAI_API_KEY":"fixture-only"}"#
    );
    cleanup_auth(work.path()).unwrap();
    cleanup_auth(work.path()).unwrap();
    assert!(!auth.exists());
    assert!(source.exists());
    fs::remove_dir(work.path().join("codex-home")).unwrap();
    let foreign = tempfile::tempdir().unwrap();
    fs::write(foreign.path().join("auth.json"), "retain").unwrap();
    std::os::unix::fs::symlink(foreign.path(), work.path().join("codex-home")).unwrap();
    assert!(cleanup_auth(work.path()).is_err());
    assert_eq!(
        fs::read_to_string(foreign.path().join("auth.json")).unwrap(),
        "retain"
    );
}
#[test]
fn config_quotes_paths_and_audit_rejects_extra_servers_and_overrides() {
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    fs::create_dir(work.path().join("codex-home")).unwrap();
    config::write(&ctx, None).unwrap();
    let doc: toml_edit::DocumentMut =
        fs::read_to_string(work.path().join("codex-config.private.toml"))
            .unwrap()
            .parse()
            .unwrap();
    assert_eq!(doc["features"]["shell_tool"].as_bool(), Some(false));
    assert_eq!(doc["cli_auth_credentials_store"].as_str(), Some("file"));
    assert_eq!(
        doc["mcp_servers"]["proofstorm"]["enabled_tools"]
            .as_array()
            .unwrap()
            .len(),
        ctx.task.allowed_tools.len()
    );
    let row = json!({"name":"proofstorm","enabled":true,"transport":{"type":"stdio","command":std::env::current_exe().unwrap(),"args":["--benchmark-proxy",work.path().join("benchmark-context.json")],"env":null,"env_vars":[],"cwd":null}});
    config::audit_mcp(&ctx, &json!([row])).unwrap();
    assert!(config::audit_mcp(&ctx, &json!([row, row])).is_err());
    let mut altered = row;
    altered["transport"]["env"] = json!({"PROOFSTORM_HOME":"foreign"});
    assert!(config::audit_mcp(&ctx, &json!([altered])).is_err());
    let cmd = command(&ctx);
    let env: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
    assert_eq!(
        env[std::ffi::OsStr::new("CODEX_HOME")],
        Some(work.path().join("codex-home").as_os_str())
    );
}
#[test]
fn execution_bounds_both_streams_and_records_failed_exit_and_timeout() {
    for (script, deadline, expected) in [
        ("exit 9", 30, "harness_failure"),
        ("exec sleep 30", 0, "timeout"),
        ("head -c 33554433 /dev/zero >&2", 30, "output_limit"),
    ] {
        let work = tempfile::tempdir().unwrap();
        let mut ctx = context(work.path());
        fs::create_dir(work.path().join("agent")).unwrap();
        let exe = work.path().join("fake-codex");
        fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        ctx.harness = Harness::Codex {
            executable: exe,
            auth_file: None,
        };
        ctx.task.deadline_seconds = deadline;
        execute(&ctx).unwrap();
        assert_eq!(
            read(&work.path().join("benchmark-attempt.json")).unwrap()["outcome"],
            expected
        );
    }
}

/// Explicit local CLI qualification, never a paid model call or CI dependency.
#[test]
#[ignore = "requires installed Codex; uses a loopback fake model and fixture MCP"]
fn installed_cli_contract_without_paid_model() -> Result<()> {
    use std::{
        io::{BufRead, Read, Write},
        net::TcpListener,
    };
    let temporary = tempfile::tempdir()?;
    let work = std::env::var_os("PROOFSTORM_CODEX_TEST_WORK")
        .map_or_else(|| temporary.path().join("probe"), std::path::PathBuf::from);
    fs::DirBuilder::new().mode(0o700).create(&work)?;
    let mut ctx = context(&work);
    let real_proxy = std::env::var_os("PROOFSTORM_CODEX_TEST_PROXY");
    if real_proxy.is_some() {
        ctx.home = std::env::var_os("PROOFSTORM_CODEX_TEST_HOME")
            .context("select owned test home")?
            .into();
        ctx.mcp = std::env::var_os("PROOFSTORM_CODEX_TEST_MCP")
            .context("select test MCP binary")?
            .into();
        save(&work.join("benchmark-context.json"), &json!(ctx))?;
    }
    if let Some(executable) = std::env::var_os("PROOFSTORM_CODEX_TEST_BIN") {
        ctx.harness = Harness::Codex {
            executable: executable.into(),
            auth_file: Some(work.join("source-auth.json")),
        };
    }
    ctx.task.deadline_seconds = 30;
    ctx.task.prompt = "Use catalog_list, then report fixture complete.".into();
    for name in ["agent", "codex-home"] {
        fs::DirBuilder::new().mode(0o700).create(work.join(name))?;
    }
    fs::write(
        work.join("source-auth.json"),
        r#"{"OPENAI_API_KEY":"fixture-not-a-real-key"}"#,
    )?;
    config::copy_auth(&ctx)?;
    config::write(&ctx, None)?;
    let mut catalog = command(&ctx);
    catalog.args(["debug", "models", "--bundled"]);
    let catalog = crate::process::capture(catalog, 30)?;
    ensure!(
        catalog.status.success(),
        "Codex bundled catalog unavailable"
    );
    let selected = config::controlled_model(&serde_json::from_slice(&catalog.stdout)?, &ctx.model)?;
    let model_path = work.join("codex-model.private.json");
    save(&model_path, &selected)?;
    config::write(&ctx, Some(&model_path))?;
    let mut listed = command(&ctx);
    listed.args(["mcp", "list", "--json"]);
    let listed = crate::process::capture(listed, 30)?;
    ensure!(listed.status.success(), "real CLI MCP list failed");
    config::audit_mcp(&ctx, &serde_json::from_slice(&listed.stdout)?)?;
    let fixture = work.join("fixture-mcp.sh");
    // JSON-RPC IDs are integers in this CLI. The fixture has no host/runtime authority.
    fs::write(
        &fixture,
        r#"#!/bin/sh
while IFS= read -r line; do
 id=$(printf '%s' "$line" | sed -n 's/.*"id":\([^,}]*\).*/\1/p')
 test -n "$id" || continue
 case "$line" in
 *'"initialize"'*) result='{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}';;
 *'"tools/list"'*) result='{"tools":[{"name":"catalog_list","description":"fixture","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]}';;
 *'"tools/call"'*) result='{"content":[{"type":"text","text":"fixture ok"}],"isError":false}';;
 *) result='{}';;
 esac
 printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
done
"#,
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut doc: toml_edit::DocumentMut =
        fs::read_to_string(work.join("codex-home/config.toml"))?.parse()?;
    doc["model_provider"] = toml_edit::value("fixture");
    doc["model_providers"]["fixture"]["name"] = toml_edit::value("Local contract fixture");
    doc["model_providers"]["fixture"]["base_url"] =
        toml_edit::value(format!("http://{}/v1", listener.local_addr()?));
    doc["model_providers"]["fixture"]["wire_api"] = toml_edit::value("responses");
    doc["model_providers"]["fixture"]["requires_openai_auth"] = toml_edit::value(false);
    doc["model_providers"]["fixture"]["request_max_retries"] = toml_edit::value(0);
    doc["model_providers"]["fixture"]["stream_max_retries"] = toml_edit::value(0);
    doc["features"]["enable_request_compression"] = toml_edit::value(false);
    doc["mcp_servers"]["proofstorm"]["command"] = toml_edit::value("/bin/sh");
    let mut args = toml_edit::Array::new();
    args.push(fixture.to_str().context("fixture path")?);
    doc["mcp_servers"]["proofstorm"]["args"] = toml_edit::value(args);
    if let Some(proxy) = &real_proxy {
        doc["mcp_servers"]["proofstorm"]["command"] =
            toml_edit::value(proxy.to_str().context("proxy path")?);
        let mut args = toml_edit::Array::new();
        args.push("--benchmark-proxy");
        args.push(
            work.join("benchmark-context.json")
                .to_str()
                .context("context path")?,
        );
        doc["mcp_servers"]["proofstorm"]["args"] = toml_edit::value(args);
    }
    fs::write(work.join("codex-home/config.toml"), doc.to_string())?;
    if real_proxy.is_some() {
        preflight(&ctx)?;
        ensure!(
            !work.join("proxy.started").exists(),
            "preflight consumed scored proxy identity"
        );
    } else {
        let mut prompt = command(&ctx);
        prompt.args(["debug", "prompt-input", &ctx.task.prompt]);
        let prompt = crate::process::capture(prompt, 30)?;
        ensure!(
            prompt.status.success(),
            "real CLI fixture prompt audit failed"
        );
        save(
            &work.join("codex-prompt.private.json"),
            &serde_json::from_slice(&prompt.stdout)?,
        )?;
    }

    let allowed: Vec<_> = ctx
        .task
        .allowed_tools
        .iter()
        .map(|name| format!("mcp__proofstorm__{name}"))
        .chain(
            [
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
            ]
            .map(str::to_owned),
        )
        .collect();
    let script = format!(
        "const allowed={}; if(ALL_TOOLS.some(t=>!allowed.includes(t.name))) throw new Error('unexpected tool surface'); const result=await tools.mcp__proofstorm__catalog_list({{}}); if(result.isError) throw new Error('MCP failed'); text('fixture ok');",
        serde_json::to_string(&allowed)?
    );
    let server = std::thread::spawn(move || -> Result<Vec<Value>> {
        let mut requests = Vec::new();
        for turn in 0..2 {
            let start = Instant::now();
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        ensure!(
                            start.elapsed() < Duration::from_secs(35),
                            "fixture request timeout"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut reader = std::io::BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" {
                    break;
                }
                ensure!(!line.is_empty(), "truncated HTTP request");
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>()?;
                }
            }
            ensure!(
                length > 0 && length < 4 * 1024 * 1024,
                "unexpected fixture request size"
            );
            let mut body = vec![0; length];
            reader.read_exact(&mut body)?;
            requests.push(serde_json::from_slice(&body)?);
            let item = if turn == 0 {
                json!({"id":"fc_fixture","type":"custom_tool_call","call_id":"fixture_call","name":"exec","namespace":"functions",
                "input":script})
            } else {
                json!({"id":"msg_fixture","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"fixture complete","annotations":[]}]})
            };
            let events = [
                json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress","output":[]}}),
                json!({"type":"response.output_item.added","output_index":0,"item":item}),
                json!({"type":"response.output_item.done","output_index":0,"item":item}),
                json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":1,"total_tokens":11}}}),
            ];
            let body = events.iter().fold(String::new(), |mut body, event| {
                write!(
                    body,
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
                .unwrap();
                body
            });
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        }
        Ok(requests)
    });
    let execution = execute(&ctx);
    cleanup_auth(&work)?;
    let requests = server.join().expect("fixture server panic")?;
    execution?;
    ensure!(
        requests.iter().all(|r| r["model"] == ctx.model),
        "model changed"
    );
    ensure!(
        requests[1]["input"]
            .as_array()
            .context("request input")?
            .iter()
            .any(|item| item["type"] == "custom_tool_call_output"
                && item["output"].to_string().contains("fixture ok")),
        "Codex did not complete the MCP call"
    );
    for (index, request) in requests.iter().enumerate() {
        save(&work.join(format!("fixture-request-{index}.json")), request)?;
    }
    if real_proxy.is_none() {
        captured(&work);
    }
    let output = retained(&work)?;
    ensure!(
        output.outcome == "completed" && output.final_text == "fixture complete",
        "incorrect final output: {}",
        output.outcome
    );
    ensure!(
        output.calls.len() == 1
            && output.calls[0].success == Some(true)
            && !output.unauthorized
            && output.telemetry_error.is_none(),
        "incorrect MCP telemetry"
    );
    ensure!(
        !work.join("codex-home/auth.json").exists(),
        "credential copy retained"
    );
    ensure!(
        fs::read_to_string(work.join("source-auth.json"))?
            == r#"{"OPENAI_API_KEY":"fixture-not-a-real-key"}"#,
        "source auth changed"
    );
    save(
        &work.join("contract-result.json"),
        &json!({"paid_model_calls":0,"real_proofstorm_proxy":real_proxy.is_some(),"tool_surface_checked":true,"mcp_round_trip":true,"credential_copy_removed":true,"source_auth_unchanged":true,"output":output}),
    )?;
    Ok(())
}

#[test]
fn repeated_arguments_keep_start_order_and_conflicting_terminals_are_gaps() {
    let work = tempfile::tempdir().unwrap();
    fs::write(
        work.path().join("events.jsonl"),
        concat!(
            "{\"kind\":\"start\",\"id\":1,\"tool\":\"catalog_list\",\"arguments\":{}}\n",
            "{\"kind\":\"end\",\"id\":1,\"success\":true,\"elapsed_ms\":9}\n",
            "{\"kind\":\"start\",\"id\":2,\"tool\":\"catalog_list\",\"arguments\":{}}\n",
            "{\"kind\":\"end\",\"id\":2,\"success\":false,\"elapsed_ms\":10}\n"
        ),
    )
    .unwrap();
    fixture(
        work.path(),
        &[
            tool("item_2", "in_progress"),
            tool("item_2", "completed"),
            tool("item_10", "failed"),
            json!({"type":"turn.completed"}),
        ],
    );
    let output = retained(work.path()).unwrap();
    assert_eq!(output.calls.len(), 2);
    assert_eq!(output.calls[0].success, Some(true));
    assert_eq!(output.calls[1].success, Some(false));
    fixture(
        work.path(),
        &[
            tool("item_2", "failed"),
            tool("item_2", "completed"),
            json!({"type":"turn.completed"}),
        ],
    );
    assert!(retained(work.path()).unwrap().telemetry_error.is_some());
}
