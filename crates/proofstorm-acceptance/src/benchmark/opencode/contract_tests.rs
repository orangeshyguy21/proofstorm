//! Explicitly opt-in tests using an installed CLI, with no model account/Docker.
use super::*;
use std::io::{BufRead, BufReader, Write};

fn contract(denied: bool) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let work = std::env::var_os("PROOFSTORM_OPENCODE_TEST_WORK")
        .map_or_else(|| temporary.path().join("run"), std::path::PathBuf::from);
    fs::create_dir(&work)?;
    let fixture = work.join("fixture.py");
    fs::write(&fixture, include_str!("fixture.py"))?;
    let mut model = Command::new("python3")
        .arg(&fixture)
        .arg(if denied { "denied" } else { "model" })
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
        server.capture_session(&context)?;
        drop(server);
        save(
            &work.join("benchmark-attempt.json"),
            &json!({"outcome":"completed","elapsed_seconds":1,"session_export_required":true}),
        )?;
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
    contract(false)
}

#[test]
#[ignore = "requires explicit OpenCode 2 CLI and Python; fake model/MCP only"]
fn installed_cli_forbidden_attempt() -> Result<()> {
    contract(true)
}
