//! One headless OpenCode attempt; models are caller-selected, never substituted.
mod telemetry;
use super::{
    Context,
    harness::{AttemptOutput, Harness},
    read, save,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn command(config: &Context) -> Command {
    let Harness::OpenCode { executable } = &config.harness else {
        unreachable!("OpenCode adapter selection")
    };
    let mut cmd = Command::new(executable);
    crate::client::clear_runtime_environment(&mut cmd);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("OPENCODE_") {
            cmd.env_remove(key);
        }
    }
    cmd.current_dir(config.work.join("agent"))
        // OpenCode run resolves its directory from PWD before process.cwd().
        .env("PWD", config.work.join("agent"))
        .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
        .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
        .env("OPENCODE_CONFIG", config.work.join("agent/opencode.json"));
    cmd
}

pub fn run(config: &Context) -> Result<()> {
    let project = config.work.join("agent");
    fs::DirBuilder::new().mode(0o700).create(&project)?;
    let executable = std::env::current_exe()?;
    let entry = json!({"model":config.model,"share":"disabled","autoupdate":false,"snapshot":false,
        "permission":{"*":"deny","proofstorm_*":"allow"},
        "agent":{"benchmark":{"mode":"primary","description":format!("Proofstorm {} benchmark",config.task.id),
            "prompt":"Complete the user's task autonomously using only the supplied Proofstorm MCP tools. Follow the checkpoint and cleanup contract.",
            "steps":150,"permission":{"*":"deny","proofstorm_*":"allow"}}},
        "mcp":{"proofstorm":{"type":"local","command":[executable,"--benchmark-proxy",config.work.join("benchmark-context.json")],"enabled":true,"timeout":120_000}}});
    save(&project.join("opencode.json"), &entry)?;
    let mut version = command(config);
    version.arg("--version");
    let version = crate::process::capture(version, 30)?;
    ensure!(version.status.success(), "OpenCode version check failed");
    let mut models = command(config);
    models.arg("models").arg(
        config
            .model
            .split('/')
            .next()
            .context("provider required")?,
    );
    let models = crate::process::capture(models, 60)?;
    ensure!(
        models.status.success()
            && String::from_utf8_lossy(&models.stdout)
                .lines()
                .any(|s| s.trim() == config.model),
        "requested provider/model unavailable; no substitution"
    );
    let mut effective = command(config);
    effective.args(["debug", "config"]);
    let effective = crate::process::capture(effective, 60)?;
    ensure!(
        effective.status.success(),
        "OpenCode configuration audit failed"
    );
    let effective: Value = serde_json::from_slice(&effective.stdout)?;
    save(&config.work.join("harness-config.private.json"), &effective)?;
    ensure!(
        effective["plugin"].as_array().is_none_or(Vec::is_empty),
        "plugins outside pilot profile"
    );
    ensure!(
        effective["mcp"]
            .as_object()
            .is_some_and(|m| m.len() == 1 && m.contains_key("proofstorm")),
        "additional MCP servers outside pilot profile"
    );
    ensure!(
        effective["mcp"]["proofstorm"]["command"] == entry["mcp"]["proofstorm"]["command"],
        "MCP command changed during configuration resolution"
    );
    ensure!(
        effective["permission"] == entry["permission"]
            && effective["agent"]["benchmark"]["permission"]
                == entry["agent"]["benchmark"]["permission"]
            && effective["agent"]["benchmark"]["steps"] == 150,
        "effective tool permissions differ"
    );
    // Verify a real connection before spending model tokens. Give discovery its
    // own capture directory, so it cannot consume the attempt's proxy identity.
    let preflight = config.work.join("transport-preflight");
    fs::DirBuilder::new().mode(0o700).create(&preflight)?;
    let mut preflight_context = json!(config);
    preflight_context["work"] = json!(preflight);
    let preflight_context_path = preflight.join("context.json");
    save(&preflight_context_path, &preflight_context)?;
    let mut preflight_entry = entry.clone();
    preflight_entry["mcp"]["proofstorm"]["command"][2] = json!(preflight_context_path);
    let preflight_config = preflight.join("opencode.json");
    save(&preflight_config, &preflight_entry)?;
    let mut probe = command(config);
    probe
        .env("OPENCODE_CONFIG", preflight_config)
        .args(["mcp", "list", "--pure"]);
    let probe = crate::process::capture(probe, 60)?;
    fs::write(
        config.work.join("harness-preflight.private.txt"),
        &probe.stdout,
    )?;
    ensure!(
        probe.status.success() && preflight.join("proxy-ready.json").exists(),
        "OpenCode did not connect to the owned benchmark proxy; model not started"
    );
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
        &json!({"task":&config.task,"model_requested":config.model,
        "model_resolved":config.model,"model_is_alias":true,"harness":"opencode","harness_version":String::from_utf8_lossy(&version.stdout).trim(),
        "harness_config_sha256":proofstorm_core::digest_json(&effective),"prompt_sha256":proofstorm_core::digest_json(&config.task.prompt),
        "source_revision":String::from_utf8_lossy(&source.stdout).trim(),"source_dirty":!dirty.stdout.is_empty(),"runner_sha256":executable_sha256,
        "platform":std::env::consts::ARCH,"os":std::env::consts::OS,"budget":{"wall_seconds":u64::from(config.task.deadline_seconds),"agent_steps":150,"spend_hard_limit":null,"token_hard_limit":null},
        "limitations":["provider credentials/config inherited; effective config audited and privately retained","provider model alias may change","time target provisional","no hard token/spend budget claimed"]}),
    )?;
    fs::write(config.work.join("prompt.txt"), &config.task.prompt)?;
    let output = fs::File::create(config.work.join("harness.jsonl"))?;
    let error = fs::File::create(config.work.join("harness.stderr"))?;
    let start = Instant::now();
    save(
        &config.work.join("benchmark-attempt.json"),
        &json!({"outcome":"running","elapsed_seconds":null}),
    )?;
    let mut cmd = command(config);
    cmd.args([
        "run",
        "--pure",
        "--dir",
        project.to_str().context("project path is not UTF-8")?,
        "--format",
        "json",
        "--agent",
        "benchmark",
        "--model",
        &config.model,
        &config.task.prompt,
    ])
    .stdin(Stdio::null())
    .stdout(output)
    .stderr(error);
    let mut child = cmd.spawn().context("start OpenCode")?;
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
    let elapsed = start.elapsed().as_secs_f64();
    let attempt = json!({"outcome":outcome,"elapsed_seconds":elapsed,"model":config.model,"usage":null,"cost":null});
    save(&config.work.join("benchmark-attempt.json"), &attempt)?;
    if outcome != "completed" {
        return Ok(());
    }
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
    let (mut calls, unauthorized) = telemetry::reconcile(captured, &rows);
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
    fn harness_directory_and_ambient_config_are_explicit() {
        let config = Context {
            root: "/repo".into(),
            work: "/private/run".into(),
            home: "/private/run/state".into(),
            mcp: "/bin/mcp".into(),
            model: "provider/model".into(),
            harness: Harness::OpenCode {
                executable: "opencode".into(),
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
