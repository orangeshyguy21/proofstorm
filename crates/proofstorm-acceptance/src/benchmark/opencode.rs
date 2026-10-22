//! One isolated OpenCode 2 attempt; provider profiles remain private caller data.
#[cfg(test)]
mod contract_tests;
pub(crate) mod profile;
mod runtime;
mod session;
mod telemetry;
use super::{
    Context,
    harness::{AttemptOutput, Harness},
    read, save,
};
use anyhow::{Context as _, Result, ensure};
#[cfg(test)]
use runtime::command;
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn run(config: &Context) -> Result<()> {
    runtime::directories(config)?;
    let (provider, profile_sha256) = profile::prepare(config)?;
    let entry = profile::config(config, &provider)?;
    save(&config.work.join("agent/opencode.json"), &entry)?;
    let mut version = runtime::command(config);
    version.arg("--version");
    let version = crate::process::capture(version, 30)?;
    let version_text = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    ensure!(
        version.status.success() && version_text.starts_with("opencode v2."),
        "OpenCode 2 required"
    );
    let server = runtime::Server::start(config)?;
    // Discovery and the model share this owned server/MCP connection. No probe
    // can consume the proxy's single-client identity and then reconnect.
    let model = server.preflight(config, &entry)?;
    let executable = std::env::current_exe()?;
    let executable_sha256 = format!("{:x}", sha2::Sha256::digest(fs::read(&executable)?));
    let mut source = Command::new("git");
    source.current_dir(&config.root).args(["rev-parse", "HEAD"]);
    let source = crate::process::capture(source, 30)?;
    ensure!(source.status.success(), "source revision unavailable");
    let mut dirty = Command::new("git");
    dirty
        .current_dir(&config.root)
        .args(["status", "--porcelain"]);
    let dirty = crate::process::capture(dirty, 30)?;
    ensure!(dirty.status.success(), "source state unavailable");
    save(
        &config.work.join("benchmark-manifest.json"),
        &json!({
            "task":&config.task,"model_requested":config.model,"model_resolved":config.model,
            "model_is_alias":true,"harness":"opencode","harness_version":version_text,
            "profile":"controlled-mcp-only-opencode2-v1","provider_profile_sha256":profile_sha256,
            "model_catalog_sha256":proofstorm_core::digest_json(&model),
            "harness_config_sha256":proofstorm_core::digest_json(&entry),
            "prompt_sha256":proofstorm_core::digest_json(&config.task.prompt),
            "source_revision":String::from_utf8_lossy(&source.stdout).trim(),"source_dirty":!dirty.stdout.is_empty(),
            "runner_sha256":executable_sha256,"platform":std::env::consts::ARCH,"os":std::env::consts::OS,
            "budget":{"wall_seconds":config.task.deadline_seconds,"agent_steps":150,"spend_hard_limit":null,"token_hard_limit":null},
            "limitations":["explicit provider/plugin profile privately snapshotted; plugins are trusted caller input","provider model alias may change; upstream weights are not verified","no hard token/spend budget claimed"]
        }),
    )?;
    fs::write(config.work.join("prompt.txt"), &config.task.prompt)?;
    let output = fs::File::create(config.work.join("harness.jsonl"))?;
    let error = fs::File::create(config.work.join("harness.stderr"))?;
    save(
        &config.work.join("benchmark-attempt.json"),
        &json!({"outcome":"running","elapsed_seconds":null,"session_export_required":true}),
    )?;
    let mut cmd = server.command(config, "run");
    cmd.args([
        "--format",
        "json",
        "--agent",
        "benchmark",
        "--model",
        &config.model,
        "--title",
        "Proofstorm benchmark",
    ])
    .stdin(Stdio::piped())
    .stdout(output)
    .stderr(error);
    super::harness::mark_launch(config)?;
    let start = Instant::now();
    let mut child = cmd.spawn().context("start OpenCode")?;
    // stdin avoids v2 CLI's argument quoting changing the frozen task prompt.
    {
        use std::io::Write;
        let mut input = child.stdin.take().context("OpenCode prompt input")?;
        input.write_all(config.task.prompt.as_bytes())?;
    }
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break if status.success() {
                "completed"
            } else {
                "harness_failure"
            };
        }
        if start.elapsed().as_secs() >= u64::from(config.task.deadline_seconds) {
            child.kill()?;
            child.wait()?;
            break "timeout";
        }
        if fs::metadata(config.work.join("harness.jsonl"))?.len() > 32 * 1024 * 1024 {
            child.kill()?;
            child.wait()?;
            break "output_limit";
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let elapsed_seconds = start.elapsed().as_secs_f64();
    if matches!(outcome, "completed" | "harness_failure")
        && let Err(error) = server.capture_session(config)
    {
        fs::write(
            config.work.join("opencode-session-error.private.txt"),
            format!("{error:#}"),
        )?;
    }
    save(
        &config.work.join("benchmark-attempt.json"),
        &json!({"outcome":outcome,"elapsed_seconds":elapsed_seconds,"model":config.model,"usage":null,"cost":null,"session_export_required":true}),
    )?;
    drop(server);
    Ok(())
}

/// Reconstruct normalized events after ordinary completion or an interrupted
/// runner. The generic scorer never knows OpenCode's transcript format.
pub(super) fn retained(work: &Path) -> Result<AttemptOutput> {
    let attempt =
        read(&work.join("benchmark-attempt.json")).unwrap_or(json!({"outcome":"not_started"}));
    let transcript = fs::read_to_string(work.join("harness.jsonl")).unwrap_or_default();
    let mut rows = Vec::new();
    let mut complete = !transcript.is_empty();
    for line in transcript.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(row) => rows.push(row),
            Err(_) => complete = false,
        }
    }
    let exported = session::rows(work);
    if attempt["session_export_required"] == true && exported.is_err() {
        complete = false;
    }
    if let Ok(exported) = exported {
        // The v2 CLI can exit before printing the last step_finish. The durable
        // session supplies every assistant turn; keep CLI-only errors as well.
        rows.retain(|row| row["type"] != "step_finish" && row["type"] != "text");
        rows.extend(exported);
    }
    let captured = super::events(work).and_then(|rows| super::calls(&rows));
    let telemetry_error = captured.as_ref().err().map(ToString::to_string);
    let captured = captured.unwrap_or_else(|_| {
        vec![super::score::Call {
            id: 1,
            tool: "telemetry_gap".into(),
            arguments: Value::Null,
            success: None,
            elapsed_ms: 0,
        }]
    });
    let task = read(&work.join("benchmark-task.json"))
        .ok()
        .and_then(|value| serde_json::from_value::<super::task::Task>(value).ok());
    let (mut calls, unauthorized) = telemetry::reconcile(captured, &rows, task.as_ref());
    if !complete {
        let id = calls
            .iter()
            .map(|call| call.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("call ID overflow")?;
        calls.push(super::score::Call {
            id,
            tool: "telemetry_gap".into(),
            arguments: Value::Null,
            success: None,
            elapsed_ms: 0,
        });
    }
    let mut outcome = attempt["outcome"]
        .as_str()
        .unwrap_or("not_started")
        .to_owned();
    if outcome == "running" {
        outcome = "interrupted".into();
    }
    if rows.iter().any(|row| row["type"] == "error") {
        outcome = "provider_or_harness_failure".into();
    }
    let final_text = rows
        .iter()
        .rev()
        .find(|row| row["type"] == "text")
        .and_then(|row| row["part"]["text"].as_str())
        .unwrap_or("")
        .to_owned();
    let usage: Vec<_> = rows.iter().filter(|row| row["type"] == "step_finish").map(|row| json!({"tokens":row["part"]["tokens"],"cost":row["part"]["cost"],"reason":row["part"]["reason"]})).collect();
    Ok(AttemptOutput {
        outcome,
        elapsed_seconds: attempt["elapsed_seconds"].as_f64(),
        final_text,
        usage: json!(usage),
        calls,
        unauthorized,
        telemetry_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permitted_rejections_reduce_tool_score_without_failing_autonomy() -> Result<()> {
        for task in [super::super::task::o1(), super::super::task::o5()] {
            for status in ["error", "completed"] {
                let work = tempfile::tempdir()?;
                save(&work.path().join("benchmark-task.json"), &json!(task))?;
                save(
                    &work.path().join("benchmark-attempt.json"),
                    &json!({"outcome":"completed","elapsed_seconds":240.0}),
                )?;
                let args = json!({"name":task.cell_name});
                let rejected_input =
                    json!({"tool":"proofstorm_cell_up","error":"missing required field"});
                let rejected = json!({"type":"tool_use","part":{"callID":"rejected","tool":"invalid","state":{"status":status,"input":rejected_input}}});
                for (file, rows) in [
                    (
                        "events.jsonl",
                        vec![
                            json!({"kind":"start","id":1,"tool":"cell_inspect","arguments":args}),
                            json!({"kind":"end","id":1,"success":true,"elapsed_ms":1}),
                        ],
                    ),
                    (
                        "harness.jsonl",
                        vec![
                            json!({"type":"tool_use","part":{"callID":"inspected","tool":"proofstorm_cell_inspect","state":{"status":"completed","input":args}}}),
                            rejected.clone(),
                            rejected,
                        ],
                    ),
                ] {
                    fs::write(
                        work.path().join(file),
                        rows.iter()
                            .map(Value::to_string)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )?;
                }
                let outcome = retained(work.path())?;
                assert!(!outcome.unauthorized);
                assert_eq!(outcome.calls.len(), 2);
                assert_eq!(outcome.calls[1].tool, "cell_up");
                assert_eq!(outcome.calls[1].arguments, rejected_input);
                assert_eq!(outcome.calls[1].success, Some(false));
                let mut observations: Value = task
                    .assertions
                    .iter()
                    .map(|(name, _)| (name.clone(), json!(true)))
                    .collect();
                observations["autonomy"] = json!(!outcome.unauthorized);
                observations["report_valid"] = json!(true);
                let score = super::super::score::grade(
                    task,
                    &observations,
                    &outcome.calls,
                    outcome.elapsed_seconds,
                    &outcome.outcome,
                    true,
                    Some(true),
                );
                assert_eq!(score["tools"]["raw_failures"], 1);
                assert_eq!(score["tool_points"], 7.5);
                assert_eq!(score["accepted_score"], 92.5);
                assert_eq!(score["status"], "accepted");
            }
        }
        Ok(())
    }

    #[test]
    fn harness_directory_and_ambient_config_are_explicit() {
        let config = Context {
            root: "/repo".into(),
            work: "/private/run".into(),
            home: "/private/run/state".into(),
            mcp: "/bin/mcp".into(),
            model: "provider/model".into(),
            harness: Harness::OpenCode {
                executable: "opencode".into(),
                profile: None,
            },
            task: super::super::task::o1().clone(),
        };
        let command = command(&config);
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/private/run/agent"))
        );
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("PWD")],
            Some(std::ffi::OsStr::new("/private/run/agent"))
        );
        assert_eq!(
            env[std::ffi::OsStr::new("OPENCODE_DISABLE_PROJECT_CONFIG")],
            Some(std::ffi::OsStr::new("true"))
        );
    }
}
