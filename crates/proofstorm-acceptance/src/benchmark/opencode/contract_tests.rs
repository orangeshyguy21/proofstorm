//! Explicitly opt-in tests using an installed CLI, with no model account/Docker.
use super::*;
use std::io::{BufRead, BufReader, Write};

fn contract(denied: bool, timeout: bool) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let work = std::env::var_os("PROOFSTORM_OPENCODE_TEST_WORK")
        .map_or_else(|| temporary.path().join("run"), std::path::PathBuf::from);
    fs::create_dir(&work)?;
    let fixture = work.join("fixture.py");
    fs::write(&fixture, include_str!("fixture.py"))?;
    let mut model = Command::new("python3")
        .arg(&fixture)
        .arg(if timeout {
            "stall"
        } else if denied {
            "denied"
        } else {
            "model"
        })
        .arg(&work)
        .stdout(Stdio::piped())
        .spawn()?;
    let result = (|| -> Result<()> {
        let mut port = String::new();
        BufReader::new(model.stdout.take().context("model port")?).read_line(&mut port)?;
        let mut context = Context {
            root: std::env::current_dir()?,
            work: work.clone(),
            home: work.join("state"),
            mcp: "unused".into(),
            model: "fixture/model".into(),
            harness: Harness::OpenCode {
                executable: std::env::var_os("PROOFSTORM_OPENCODE_TEST_BIN")
                    .context("set PROOFSTORM_OPENCODE_TEST_BIN")?
                    .into(),
                profile: Some(work.join("profile.json")),
            },
            task: super::super::task::o1().clone(),
        };
        context.task.prompt = "Use catalog_list, then report fixture complete.".into();
        fs::create_dir(work.join("compat"))?;
        fs::write(
            work.join("compat/package.json"),
            r#"{"name":"fixture-compat","type":"module","exports":"./index.ts"}"#,
        )?;
        fs::write(
            work.join("compat/index.ts"),
            r#"
import { Plugin } from "@opencode/plugin"
export default Plugin.define({ id: "fixture.compat", async setup(ctx) {
  await ctx.session.hook("http.request", async event => {
    const headers = new Headers(event.request.headers)
    headers.set("x-proofstorm-fixture", "active")
    event.request = new Request(event.request, { headers })
  }, { providerID: "fixture" })
}})
"#,
        )?;
        save(&work.join("benchmark-task.json"), &json!(context.task))?;
        save(
            &work.join("profile.json"),
            &json!({"plugins":["./compat"],"providers":{"fixture":{"package":"@opencode/ai/providers/openai-compatible","settings":{"apiKey":"not-a-key","baseURL":format!("http://127.0.0.1:{}/v1",port.trim())},"models":{"model":{"capabilities":{"tools":true},"limit":{"context":32768,"output":1024}}}}}}),
        )?;
        runtime::directories(&context)?;
        let (provider, _) = profile::prepare(&context)?;
        let mut config = profile::config(&context, &provider)?;
        config["mcp"]["servers"]["proofstorm"]["command"] =
            json!(["python3", fixture, "mcp", work]);
        save(&work.join("agent/opencode.json"), &config)?;
        let server = runtime::Server::start(&context)?;
        server.preflight(&context, &config)?;
        let mut cmd = server.command(&context, "run");
        cmd.args([
            "--format",
            "json",
            "--agent",
            "benchmark",
            "--model",
            "fixture/model",
            "--title",
            "Fixture",
        ])
        .stdin(Stdio::piped())
        .stdout(fs::File::create(work.join("harness.jsonl"))?)
        .stderr(fs::File::create(work.join("harness.stderr"))?);
        let mut child = cmd.spawn()?;
        child
            .stdin
            .take()
            .context("prompt")?
            .write_all(context.task.prompt.as_bytes())?;
        let start = Instant::now();
        loop {
            if timeout && work.join("stalled").exists() {
                child.kill()?;
                child.wait()?;
                break;
            }
            if child.try_wait()?.is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                child.kill()?;
                child.wait()?;
                anyhow::bail!("fixture CLI timeout");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let outcome = if timeout { "timeout" } else { "completed" };
        save(
            &work.join("benchmark-attempt.json"),
            &json!({"outcome":outcome,"elapsed_seconds":1,"session_export_required":true}),
        )?;
        server.finish_session(&context, outcome)?;
        drop(server);
        diagnostics::summarize(&work)?;
        let stop = read(&work.join("opencode-stop.json"))?;
        ensure!(
            stop["export_succeeded"] == true,
            "durable session export failed: {stop}"
        );
        let capture_status = read(&work.join("transport-capture-status.json"))?;
        ensure!(
            capture_status["closed"] == true && capture_status["drain_complete"] == true,
            "capture did not flush at shutdown: {capture_status}"
        );
        ensure!(
            capture_status["write_errors"] == 0 && capture_status["dropped_records"] == 0,
            "capture evidence incomplete: {capture_status}"
        );
        let transport = fs::read_to_string(work.join("transport.private.jsonl"))?;
        ensure!(
            !transport.contains("not-a-key") && !transport.contains("fixture-secret-cookie"),
            "capture leaked credential"
        );
        let events: Vec<Value> = transport
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            !events
                .iter()
                .any(|v| v["type"] == "correlation_gap" || v["type"] == "capture_error"),
            "HTTP capture gap: {events:?}"
        );
        let requests: Vec<_> = events.iter().filter(|v| v["type"] == "request").collect();
        let responses: Vec<_> = events
            .iter()
            .filter(|v| v["type"] == "response_headers")
            .collect();
        ensure!(
            requests.len() == if denied { 3 } else { 2 },
            "missing/extra captured requests"
        );
        ensure!(
            responses.len() == requests.len(),
            "missing response headers"
        );
        for response in responses {
            ensure!(
                requests
                    .iter()
                    .any(|r| r["request_id"] == response["request_id"]),
                "uncorrelated response"
            );
            ensure!(
                response["headers"]["x-request-id"]
                    .as_str()
                    .is_some_and(|v| v.starts_with("http-fixture-")),
                "HTTP trace ID missing"
            );
        }
        if timeout {
            ensure!(
                stop["interrupt_succeeded"] == true,
                "owned session interrupt failed: {stop}"
            );
            ensure!(
                events.iter().any(|v| v["type"] == "request_signal_abort"
                    || v["type"] == "consumer_cancel"
                    || v["type"] == "stream_error"),
                "cancellation observation missing"
            );
            let diagnostics = read(&work.join("failure-diagnostics.json"))?;
            ensure!(
                diagnostics["local_termination"] == "timeout",
                "local timeout lost"
            );
            ensure!(
                fs::read_to_string(work.join("requests.jsonl"))?
                    .lines()
                    .count()
                    == 2,
                "interrupt initiated extra inference"
            );
            return Ok(());
        }
        let output = retained(&work)?;
        ensure!(
            output.outcome == "completed" && output.final_text == "fixture complete",
            "fixture transcript incomplete: {}",
            serde_json::to_string(&output)?
        );
        ensure!(
            output.unauthorized == denied,
            "forbidden attempt classification"
        );
        ensure!(
            output
                .calls
                .iter()
                .any(|c| c.tool == "catalog_list" && c.success == Some(true)),
            "MCP call was not reconciled"
        );
        ensure!(
            !output.calls.iter().any(|c| c.tool == "telemetry_gap"),
            "incomplete telemetry"
        );
        let requests: Vec<Value> = fs::read_to_string(work.join("requests.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            requests.len() == if denied { 3 } else { 2 },
            "unexpected extra inference calls"
        );
        for request in &requests {
            ensure!(
                request["fixture_plugin"] == "active",
                "explicit provider plugin did not run"
            );
            ensure!(request["model"] == "model", "model substituted");
            let names: Vec<_> = request["tools"]
                .as_array()
                .context("missing tools")?
                .iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect();
            ensure!(
                names == ["proofstorm_catalog_list"],
                "unexpected model tool surface: {names:?}"
            );
        }
        ensure!(
            requests[0]["messages"]
                .as_array()
                .context("messages")?
                .iter()
                .any(|m| m["role"] == "user" && m["content"] == context.task.prompt),
            "task prompt altered"
        );
        ensure!(
            output
                .usage
                .as_array()
                .is_some_and(|u| u.len() == requests.len()),
            "token accounting incomplete"
        );
        Ok(())
    })();
    let _ = model.kill();
    let _ = model.wait();
    result
}

#[test]
#[ignore = "requires explicit OpenCode 2 CLI and Python; fake model/MCP only"]
fn installed_cli_round_trip() -> Result<()> {
    contract(false, false)
}

#[test]
#[ignore = "requires explicit OpenCode 2 CLI and Python; fake model/MCP only"]
fn installed_cli_forbidden_attempt() -> Result<()> {
    contract(true, false)
}

#[test]
#[ignore = "requires explicit OpenCode 2 CLI and Python; fake model/MCP only"]
fn installed_cli_timeout_exports_and_cancels_owned_session() -> Result<()> {
    contract(false, true)
}
