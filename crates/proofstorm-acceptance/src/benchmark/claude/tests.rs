use super::*;
use crate::benchmark::read;
use std::{collections::BTreeSet, fmt::Write as _, os::unix::fs::PermissionsExt};

fn context(work: &Path) -> Context {
    Context {
        root: work.into(),
        work: work.into(),
        home: work.join("state"),
        mcp: "/unused/mcp".into(),
        model: "claude-fable-5-1".into(),
        harness: Harness::ClaudeCode {
            executable: "claude".into(),
        },
        task: crate::benchmark::task::o1().clone(),
    }
}
fn init(ctx: &Context) -> Value {
    json!({"type":"system","subtype":"init","tools":expected_tools(&ctx.task),
        "mcp_servers":[{"name":"proofstorm","status":"connected","source":"dynamic"}],
        "model":ctx.model,"permissionMode":"dontAsk","apiKeySource":"ANTHROPIC_API_KEY",
        "claude_code_version":"test","skills":[],"slash_commands":[],
        "plugins":[{"name":"agents-md","path":"builtin"}]})
}
fn tool_use(id: &str, name: &str) -> Value {
    json!({"type":"assistant","parent_tool_use_id":null,"message":{"model":"claude-fable-5-1-fixture",
        "content":[{"type":"tool_use","id":id,"name":name,"input":{}}]}})
}
fn tool_result(id: &str, error: bool) -> Value {
    json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":error}]}})
}
fn done(text: &str) -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"result":text,"total_cost_usd":0.25,
        "num_turns":2,"usage":{"input_tokens":12,"output_tokens":4},"modelUsage":{},"permission_denials":[]})
}
fn fixture(work: &Path, rows: &[Value]) {
    save(&work.join("benchmark-context.json"), &json!(context(work))).unwrap();
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
fn captured(work: &Path) {
    fs::write(work.join("events.jsonl"), "{\"kind\":\"start\",\"id\":1,\"tool\":\"catalog_list\",\"arguments\":{}}\n{\"kind\":\"end\",\"id\":1,\"success\":true,\"elapsed_ms\":9}\n").unwrap();
}

#[test]
fn stream_json_reconciles_once_and_preserves_cost_and_report() {
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    captured(work.path());
    fixture(
        work.path(),
        &[
            init(&ctx),
            tool_use("one", "mcp__proofstorm__catalog_list"),
            tool_use("one", "mcp__proofstorm__catalog_list"),
            tool_result("one", false),
            json!({"type":"rate_limit_event"}),
            json!({"type":"assistant","parent_tool_use_id":null,"message":{"model":"claude-fable-5-1-fixture","content":[{"type":"thinking","thinking":""},{"type":"text","text":"draft"}]}}),
            done("{\"success\":false}"),
        ],
    );
    let output = retained(work.path()).unwrap();
    assert_eq!(output.outcome, "completed");
    assert_eq!(output.calls.len(), 1);
    assert_eq!(output.calls[0].success, Some(true));
    assert_eq!(output.calls[0].elapsed_ms, 9);
    assert!(!output.unauthorized);
    assert!(output.telemetry_error.is_none());
    assert_eq!(output.final_text, "{\"success\":false}");
    assert_eq!(output.usage["total_cost_usd"], 0.25);
    assert_eq!(
        output.usage["models_observed"],
        json!(["claude-fable-5-1-fixture"])
    );
}

#[test]
fn failed_or_missing_harness_reply_never_borrows_proxy_success() {
    for (rows, expected) in [
        (vec![tool_result("one", true)], Some(false)),
        (vec![], None),
    ] {
        let work = tempfile::tempdir().unwrap();
        let ctx = context(work.path());
        captured(work.path());
        let mut all = vec![init(&ctx), tool_use("one", "mcp__proofstorm__catalog_list")];
        all.extend(rows);
        all.push(done("{}"));
        fixture(work.path(), &all);
        let output = retained(work.path()).unwrap();
        assert_eq!(output.calls[0].success, expected);
        assert!(!output.unauthorized);
    }
}

#[test]
fn refusals_foreign_tools_gaps_and_errors_stay_visible() {
    // A permission refusal is a failed attempt, not an unauthorized action.
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    fixture(
        work.path(),
        &[
            init(&ctx),
            tool_use("bash", "Bash"),
            tool_result("bash", true),
            done("{}"),
        ],
    );
    let output = retained(work.path()).unwrap();
    assert!(!output.unauthorized);
    assert_eq!(output.calls.last().unwrap().success, Some(false));
    assert!(output.telemetry_error.is_none());

    let cases: Vec<(Vec<Value>, &str, bool, bool)> = vec![
        // Unobserved successful foreign tool.
        (
            vec![
                init(&ctx),
                tool_use("bash", "Bash"),
                tool_result("bash", false),
                done("{}"),
            ],
            "completed",
            true,
            false,
        ),
        // Session exposed a tool outside the profile.
        (
            vec![
                {
                    let mut wide = init(&ctx);
                    wide["tools"].as_array_mut().unwrap().push(json!("Bash"));
                    wide
                },
                done("{}"),
            ],
            "completed",
            true,
            false,
        ),
        // Subagent traffic.
        (
            vec![
                init(&ctx),
                json!({"type":"assistant","parent_tool_use_id":"x","message":{"content":[]}}),
                done("{}"),
            ],
            "completed",
            true,
            false,
        ),
        // Unknown event and content types leave telemetry incomplete.
        (
            vec![init(&ctx), json!({"type":"surprise"}), done("{}")],
            "completed",
            false,
            true,
        ),
        (
            vec![
                init(&ctx),
                json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"image"}]}}),
                done("{}"),
            ],
            "completed",
            false,
            true,
        ),
        // No session initialization.
        (vec![done("{}")], "completed", false, true),
        // Missing or failed result is not a completed attempt.
        (
            vec![init(&ctx)],
            "provider_or_harness_failure",
            false,
            false,
        ),
        (
            vec![
                init(&ctx),
                json!({"type":"result","subtype":"error_during_execution","is_error":true,"result":"x"}),
            ],
            "provider_or_harness_failure",
            false,
            false,
        ),
    ];
    for (index, (rows, outcome, unauthorized, gap)) in cases.into_iter().enumerate() {
        let work = tempfile::tempdir().unwrap();
        fixture(work.path(), &rows);
        let output = retained(work.path()).unwrap();
        assert_eq!(output.outcome, outcome, "case {index}");
        assert_eq!(output.unauthorized, unauthorized, "case {index}");
        assert_eq!(output.telemetry_error.is_some(), gap, "case {index}");
    }

    let work = tempfile::tempdir().unwrap();
    fixture(work.path(), &[init(&ctx)]);
    fs::write(
        work.path().join("harness.jsonl"),
        format!("{}\n{{\"type\":", init(&ctx)),
    )
    .unwrap();
    save(
        &work.path().join("benchmark-attempt.json"),
        &json!({"outcome":"running"}),
    )
    .unwrap();
    let output = retained(work.path()).unwrap();
    assert_eq!(output.outcome, "interrupted");
    assert!(output.telemetry_error.is_some());
    assert_eq!(output.calls.last().unwrap().tool, "telemetry_gap");
}

#[test]
fn proxy_call_without_harness_counterpart_is_a_gap() {
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    captured(work.path());
    fixture(work.path(), &[init(&ctx), done("{}")]);
    assert!(retained(work.path()).unwrap().telemetry_error.is_some());
}

#[test]
fn command_isolates_home_config_and_ambient_credentials() {
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    let cmd = command(&ctx);
    let env: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
    let get = |key: &str| env.get(std::ffi::OsStr::new(key)).copied().flatten();
    assert_eq!(
        get("HOME"),
        Some(work.path().join("claude-home").as_os_str())
    );
    assert_eq!(
        get("CLAUDE_CONFIG_DIR"),
        Some(work.path().join("claude-home/config").as_os_str())
    );
    assert_eq!(
        get("CLAUDE_CODE_DISABLE_AUTO_MEMORY"),
        Some(std::ffi::OsStr::new("1"))
    );
    for key in env.keys() {
        let key = key.to_string_lossy();
        assert!(
            !key.starts_with("ANTHROPIC_") && !key.starts_with("OPENAI_") && key != "CLAUDECODE",
            "{key}"
        );
    }
    assert_eq!(
        cmd.get_current_dir(),
        Some(work.path().join("agent").as_path())
    );
}

#[test]
fn arguments_pin_the_controlled_profile_and_end_options_before_the_prompt() {
    let work = tempfile::tempdir().unwrap();
    let mut ctx = context(work.path());
    ctx.task.prompt = "--looks-like-a-flag".into();
    let args: Vec<_> = arguments(&ctx, Path::new("/owned/mcp.json"))
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    let after = |flag: &str| &args[args.iter().position(|a| a == flag).unwrap() + 1];
    assert_eq!(after("--tools"), "");
    assert_eq!(after("--setting-sources"), "");
    assert_eq!(after("--permission-mode"), "dontAsk");
    assert_eq!(after("--mcp-config"), "/owned/mcp.json");
    assert_eq!(after("--model"), "claude-fable-5-1");
    assert!(args.contains(&"--strict-mcp-config".into()));
    assert!(args.contains(&"--no-session-persistence".into()));
    let allowed: BTreeSet<_> = after("--allowedTools")
        .split(',')
        .map(str::to_owned)
        .collect();
    assert_eq!(allowed, expected_tools(&ctx.task));
    assert_eq!(&args[args.len() - 2..], ["--", "--looks-like-a-flag"]);
}

#[test]
fn mcp_config_restores_home_and_blanks_model_credentials_for_the_proxy() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("mcp.json");
    write_mcp_config(Path::new("/owned/context.json"), &path).unwrap();
    let config = read(&path).unwrap();
    let server = &config["mcpServers"]["proofstorm"];
    assert_eq!(config["mcpServers"].as_object().unwrap().len(), 1);
    assert_eq!(
        server["args"],
        json!(["--benchmark-proxy", "/owned/context.json"])
    );
    for key in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
    ] {
        assert_eq!(server["env"][key], "", "{key}");
    }
}

#[test]
fn exactly_one_explicit_credential_is_required() {
    let lookup = |set: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            set.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    };
    assert!(credential(lookup(&[])).is_err());
    assert!(credential(lookup(&[("ANTHROPIC_API_KEY", "")])).is_err());
    assert!(
        credential(lookup(&[
            ("ANTHROPIC_API_KEY", "a"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "b")
        ]))
        .is_err()
    );
    assert_eq!(
        credential(lookup(&[("CLAUDE_CODE_OAUTH_TOKEN", "b")]))
            .unwrap()
            .0,
        "CLAUDE_CODE_OAUTH_TOKEN"
    );
}

fn preflight_fixture(ctx: &Context) -> (Vec<Value>, Vec<Value>) {
    let tools: Vec<_> = expected_tools(&ctx.task)
        .into_iter()
        .map(|name| json!({"name":name}))
        .collect();
    let request = json!({"method":"POST","path":"/v1/messages?beta=true","headers":{},"body":{
        "model":ctx.model,"tools":tools,"system":[{"type":"text","text":"sys"}],
        "messages":[{"role":"user","content":[{"type":"text","text":"<system-reminder>x</system-reminder>"},{"type":"text","text":ctx.task.prompt}]}]}});
    (
        vec![init(ctx)],
        vec![
            json!({"method":"GET","path":"/api/hello","headers":{},"body":null}),
            request,
        ],
    )
}

#[test]
fn preflight_audit_rejects_any_profile_drift() {
    let work = tempfile::tempdir().unwrap();
    let ctx = context(work.path());
    let (rows, requests) = preflight_fixture(&ctx);
    let profile = preflight::audit(&ctx, &rows, &requests).unwrap();
    assert!(
        profile["system_prompt_sha256"]
            .as_str()
            .is_some_and(|d| d.len() >= 64)
    );
    let mutations: Vec<(&str, Value, bool)> = vec![
        ("/tools/0", json!("Bash"), true),
        ("/mcp_servers/0/status", json!("failed"), true),
        ("/permissionMode", json!("bypassPermissions"), true),
        ("/model", json!("claude-other"), true),
        ("/apiKeySource", json!("/login managed key"), true),
        ("/skills", json!(["personal"]), true),
        ("/plugins/0/path", json!("/Users/someone/plugin"), true),
        ("/body/model", json!("claude-other"), false),
        ("/body/tools/0/name", json!("Bash"), false),
        (
            "/body/messages/0/content/1/text",
            json!("changed prompt"),
            false,
        ),
    ];
    for (pointer, value, session) in mutations {
        let (mut rows, mut requests) = preflight_fixture(&ctx);
        let target = if session {
            &mut rows[0]
        } else {
            &mut requests[1]
        };
        *target.pointer_mut(pointer).unwrap() = value;
        assert!(
            preflight::audit(&ctx, &rows, &requests).is_err(),
            "{pointer}"
        );
    }
    let (rows, _) = preflight_fixture(&ctx);
    assert!(preflight::audit(&ctx, &rows, &[]).is_err());
    assert!(preflight::audit(&ctx, &[], &preflight_fixture(&ctx).1).is_err());
}

#[test]
fn preflight_endpoint_refuses_and_never_records_credentials() {
    use std::io::{Read, Write};
    let responder = preflight::Responder::start().unwrap();
    let address = responder.url().trim_start_matches("http://").to_owned();
    let body = r#"{"model":"m"}"#;
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    write!(
        stream,
        "POST /v1/messages HTTP/1.1\r\nx-api-key: secret-value\r\nAuthorization: Bearer secret-value\r\nuser-agent: claude-cli/test\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 400"));
    let requests = responder.finish().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["body"]["model"], "m");
    assert_eq!(requests[0]["headers"]["user-agent"], "claude-cli/test");
    assert!(!requests[0].to_string().contains("secret-value"));
}

#[test]
fn execution_bounds_both_streams_and_records_failed_exit_and_timeout() {
    for (script, deadline, expected) in [
        ("exit 9", 30, "harness_failure"),
        ("exec sleep 30", 0, "timeout"),
        ("head -c 33554433 /dev/zero >&2", 30, "output_limit"),
        ("env > \"$PWD/../env.txt\"", 30, "completed"),
    ] {
        let work = tempfile::tempdir().unwrap();
        let mut ctx = context(work.path());
        fs::create_dir(work.path().join("agent")).unwrap();
        let exe = work.path().join("fake-claude");
        fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        ctx.harness = Harness::ClaudeCode { executable: exe };
        ctx.task.deadline_seconds = deadline;
        execute(
            &ctx,
            &[("ANTHROPIC_API_KEY", OsStr::new("fixture-not-a-real-key"))],
        )
        .unwrap();
        assert_eq!(
            read(&work.path().join("benchmark-attempt.json")).unwrap()["outcome"],
            expected
        );
        if expected == "completed" {
            let env = fs::read_to_string(work.path().join("env.txt")).unwrap();
            assert!(env.contains("ANTHROPIC_API_KEY=fixture-not-a-real-key"));
            assert!(!env.contains("ANTHROPIC_BASE_URL="));
            assert!(!env.lines().any(|l| l.starts_with("CLAUDECODE=")));
        }
    }
}

/// Explicit local CLI qualification, never a paid model call or CI dependency.
/// A loopback fake model drives one real MCP round trip through the installed
/// CLI with the exact scored-attempt arguments and isolation.
#[test]
#[ignore = "requires installed Claude Code; uses a loopback fake model and fixture MCP"]
fn installed_cli_contract_without_paid_model() -> Result<()> {
    use std::{
        io::{BufRead, Read, Write},
        net::TcpListener,
        sync::atomic::{AtomicBool, Ordering},
    };
    let temporary = tempfile::tempdir()?;
    let work = std::env::var_os("PROOFSTORM_CLAUDE_TEST_WORK")
        .map_or_else(|| temporary.path().join("probe"), std::path::PathBuf::from);
    fs::DirBuilder::new().mode(0o700).create(&work)?;
    let mut ctx = context(&work);
    if let Some(executable) = std::env::var_os("PROOFSTORM_CLAUDE_TEST_BIN") {
        ctx.harness = Harness::ClaudeCode {
            executable: executable.into(),
        };
    }
    ctx.task.deadline_seconds = 90;
    ctx.task.prompt = "Use catalog_list, then report fixture complete.".into();
    save(&work.join("benchmark-context.json"), &json!(ctx))?;
    for name in ["agent", "claude-home", "claude-home/config"] {
        fs::DirBuilder::new().mode(0o700).create(work.join(name))?;
    }
    let mut init = Command::new("git");
    init.args(["init", "--quiet", "--initial-branch=main"])
        .arg(work.join("agent"));
    ensure!(
        crate::process::capture(init, 15)?.status.success(),
        "git init"
    );
    // Fixture MCP with the benchmark's exact tool names; no host/runtime authority.
    let tools: Vec<_> = ctx
        .task
        .allowed_tools
        .iter()
        .map(|name| json!({"name":name,"description":"fixture","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}))
        .collect();
    let fixture = work.join("fixture-mcp.sh");
    fs::write(
        &fixture,
        format!(
            r#"#!/bin/sh
while IFS= read -r line; do
 id=$(printf '%s' "$line" | sed -n 's/.*"id":\([^,}}]*\).*/\1/p')
 test -n "$id" || continue
 case "$line" in
 *'"initialize"'*) result='{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fixture","version":"1"}}}}';;
 *'"tools/list"'*) result='{{"tools":{tools}}}';;
 *'"tools/call"'*) result='{{"content":[{{"type":"text","text":"fixture ok"}}],"isError":false}}';;
 *) result='{{}}';;
 esac
 printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$id" "$result"
done
"#,
            tools = serde_json::to_string(&tools)?
        ),
    )?;
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700))?;
    let fixture_config =
        json!({"mcpServers":{"proofstorm":{"type":"stdio","command":fixture,"args":[]}}});
    save(&work.join("claude-mcp.private.json"), &fixture_config)?;

    // The real audit, against the real CLI, with the refusing preflight endpoint.
    let responder = preflight::Responder::start()?;
    let mut probe = command(&ctx);
    probe
        .args(arguments(&ctx, &work.join("claude-mcp.private.json")))
        .env("ANTHROPIC_API_KEY", "fixture-not-a-real-key")
        .env("ANTHROPIC_BASE_URL", responder.url());
    let probe = crate::process::capture(probe, 90);
    let requests = responder.finish()?;
    let (rows, _) = json_lines(&String::from_utf8_lossy(&probe?.stdout));
    let profile = preflight::audit(&ctx, &rows, &requests)?;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("http://{}", listener.local_addr()?);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&stop);
    let server = std::thread::spawn(move || -> Result<Vec<Value>> {
        let mut requests = Vec::new();
        while !flag.load(Ordering::SeqCst) {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut reader = std::io::BufReader::new(stream);
            let mut first = String::new();
            reader.read_line(&mut first)?;
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>()?;
                }
            }
            ensure!(length < 4 * 1024 * 1024, "unexpected fixture request size");
            let mut body = vec![0; length];
            reader.read_exact(&mut body)?;
            if !first.contains("/v1/messages") {
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                )?;
                continue;
            }
            let request: Value = serde_json::from_slice(&body)?;
            let answered = request["messages"].to_string().contains("tool_result");
            requests.push(request);
            let block = if answered {
                json!({"type":"text","text":""})
            } else {
                json!({"type":"tool_use","id":"toolu_fixture","name":"mcp__proofstorm__catalog_list","input":{}})
            };
            let delta = if answered {
                json!({"type":"text_delta","text":"fixture complete"})
            } else {
                json!({"type":"input_json_delta","partial_json":"{}"})
            };
            let events = [
                json!({"type":"message_start","message":{"id":"msg_fixture","type":"message","role":"assistant","model":"claude-fable-5-1","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}),
                json!({"type":"content_block_start","index":0,"content_block":block}),
                json!({"type":"content_block_delta","index":0,"delta":delta}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":if answered {"end_turn"} else {"tool_use"},"stop_sequence":null},"usage":{"output_tokens":5}}),
                json!({"type":"message_stop"}),
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
    let execution = execute(
        &ctx,
        &[
            ("ANTHROPIC_API_KEY", OsStr::new("fixture-not-a-real-key")),
            ("ANTHROPIC_BASE_URL", OsStr::new(&url)),
        ],
    );
    stop.store(true, Ordering::SeqCst);
    let requests = server.join().expect("fixture server panic")?;
    execution?;
    ensure!(
        requests.len() >= 2 && requests.iter().all(|r| r["model"] == ctx.model),
        "model changed or round trip incomplete"
    );
    // The fixture MCP is not the proxy; supply the proxy trace it would record.
    captured(&work);
    let output = retained(&work)?;
    ensure!(
        output.outcome == "completed" && output.final_text == "fixture complete",
        "incorrect final output: {} {:?}",
        output.outcome,
        output.final_text
    );
    ensure!(
        output.calls.len() == 1
            && output.calls[0].success == Some(true)
            && !output.unauthorized
            && output.telemetry_error.is_none(),
        "incorrect MCP telemetry: {:?} {:?}",
        output.calls,
        output.telemetry_error
    );
    ensure!(
        fs::read_dir(work.join("claude-home"))?.count() > 0,
        "owned home unused"
    );
    save(
        &work.join("contract-result.json"),
        &json!({"paid_model_calls":0,"preflight_profile":profile,"mcp_round_trip":true,"output":output}),
    )?;
    Ok(())
}
