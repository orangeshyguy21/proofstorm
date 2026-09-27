//! Codex exec adapter. No personal configuration or model substitution.
mod config;
mod telemetry;
#[cfg(test)]
mod tests;

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
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn command(context: &Context) -> Command {
    let Harness::Codex { executable, .. } = &context.harness else {
        unreachable!("Codex adapter")
    };
    let mut command = Command::new(executable);
    crate::client::clear_runtime_environment(&mut command);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CODEX_") || name.starts_with("OPENAI_") || name.starts_with("CHATGPT_")
        {
            command.env_remove(key);
        }
    }
    command
        .current_dir(context.work.join("agent"))
        .env("PWD", context.work.join("agent"))
        .env("CODEX_HOME", context.work.join("codex-home"));
    command
}

pub fn cleanup_auth(work: &Path) -> Result<()> {
    let home = work.join("codex-home");
    match fs::symlink_metadata(&home) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(stat) => ensure!(
            stat.is_dir(),
            "refusing linked Codex home during credential cleanup"
        ),
    }
    match fs::remove_file(home.join("auth.json")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn run(context: &Context) -> Result<()> {
    let result = run_inner(context);
    let cleaned = cleanup_auth(&context.work);
    result.and(cleaned)
}

fn run_inner(context: &Context) -> Result<()> {
    for name in ["agent", "codex-home"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(context.work.join(name))?;
    }
    // Bound project discovery to an owned repository, not the parent checkout.
    let mut init = Command::new("git");
    init.args(["init", "--quiet"])
        .arg(context.work.join("agent"));
    ensure!(
        crate::process::capture(init, 15)?.status.success(),
        "create owned Codex project"
    );
    config::write(context, None)?;
    let mut version = command(context);
    version.arg("--version");
    let version = crate::process::capture(version, 30)?;
    ensure!(version.status.success(), "Codex version check failed");
    let mut catalog = command(context);
    catalog.args(["debug", "models", "--bundled"]);
    let catalog = crate::process::capture(catalog, 30)?;
    ensure!(
        catalog.status.success(),
        "Codex must support debug models --bundled; no model started"
    );
    let selected =
        config::controlled_model(&serde_json::from_slice(&catalog.stdout)?, &context.model)?;
    let model_path = context.work.join("codex-model.private.json");
    save(&model_path, &selected)?;
    config::write(context, Some(&model_path))?;
    config::copy_auth(context)?;
    let mut login = command(context);
    login.args(["login", "status"]);
    ensure!(
        crate::process::capture(login, 30)?.status.success(),
        "Codex authentication unavailable in owned home"
    );
    let mut listed = command(context);
    listed.args(["mcp", "list", "--json"]);
    let listed = crate::process::capture(listed, 30)?;
    ensure!(
        listed.status.success(),
        "Codex MCP configuration check failed"
    );
    let listed: Value = serde_json::from_slice(&listed.stdout)?;
    config::audit_mcp(context, &listed)?;
    save(&context.work.join("harness-config.private.json"), &listed)?;
    preflight(context)?;
    let mut source = Command::new("git");
    source
        .current_dir(&context.root)
        .args(["rev-parse", "HEAD"]);
    let source = crate::process::capture(source, 15)?;
    let mut dirty = Command::new("git");
    dirty
        .current_dir(&context.root)
        .args(["status", "--porcelain"]);
    let dirty = crate::process::capture(dirty, 15)?;
    ensure!(
        source.status.success() && dirty.status.success(),
        "source identity unavailable"
    );
    save(
        &context.work.join("benchmark-manifest.json"),
        &json!({
        "task":context.task,"harness":"codex","harness_version":String::from_utf8_lossy(&version.stdout).trim(),
        "model_requested":context.model,"model_resolved":null,"model_catalog_match":context.model,"model_is_alias":true,
        "provider":"openai","profile":"controlled-mcp-only-v1","model_catalog_sha256":proofstorm_core::digest_json(&selected),
        "harness_config_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(context.work.join("codex-config.private.toml"))?)),
        "prompt_sha256":proofstorm_core::digest_json(&context.task.prompt),
        "source_revision":String::from_utf8_lossy(&source.stdout).trim(),"source_dirty":!dirty.stdout.is_empty(),
        "runner_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(std::env::current_exe()?)?)),
        "platform":std::env::consts::ARCH,"os":std::env::consts::OS,
        "budget":{"wall_seconds":context.task.deadline_seconds,"token_hard_limit":null,"spend_hard_limit":null},
        "limitations":["bundled catalog membership is not account availability or provider-resolved model identity",
        "controlled catalog disables patching, delegation and experimental tools; original entry retained",
        "CLI-bundled skill descriptions may remain in the rendered prompt; personal skill discovery is disabled",
        "Codex JSONL does not expose wrapper-only code-mode errors; tool ratio covers observed MCP attempts",
        "request_user_input may be advertised by Codex but exec cannot accept human assistance",
        "no hard token/spend budget claimed","time target provisional"]}),
    )?;
    execute(context)
}

fn preflight(context: &Context) -> Result<()> {
    // debug prompt-input connects to MCP without making a model request. Give
    // discovery a separate proxy identity so the scored trace stays untouched.
    let directory = context.work.join("transport-preflight");
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut preflight_context = json!(context);
    preflight_context["work"] = json!(directory);
    let path = directory.join("context.json");
    save(&path, &preflight_context)?;
    config::proxy_context(context, &path)?;
    let mut prompt = command(context);
    prompt.args(["debug", "prompt-input", &context.task.prompt]);
    let prompt = crate::process::capture(prompt, 60);
    config::proxy_context(context, &context.work.join("benchmark-context.json"))?;
    fs::copy(
        context.work.join("codex-home/config.toml"),
        context.work.join("codex-config.private.toml"),
    )?;
    let prompt = prompt?;
    fs::write(
        context.work.join("harness-preflight.private.txt"),
        &prompt.stderr,
    )?;
    ensure!(
        prompt.status.success() && directory.join("proxy-ready.json").exists(),
        "Codex did not connect to the owned benchmark proxy; model not started"
    );
    let tools = read(&directory.join("tools.json"))?;
    ensure!(
        tools["tools"]
            .as_array()
            .is_some_and(|tools| tools.len() == context.task.allowed_tools.len()),
        "benchmark proxy tools missing"
    );
    save(
        &context.work.join("harness-preflight.private.txt"),
        &json!({
            "stderr":String::from_utf8_lossy(&prompt.stderr),
            "proxy_ready":read(&directory.join("proxy-ready.json"))?,
            "tools_sha256":proofstorm_core::digest_json(&tools),"model_request_made":false
        }),
    )?;
    save(
        &context.work.join("codex-prompt.private.json"),
        &serde_json::from_slice(&prompt.stdout)?,
    )?;
    Ok(())
}

fn execute(context: &Context) -> Result<()> {
    fs::write(context.work.join("prompt.txt"), &context.task.prompt)?;
    let private = |name| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(context.work.join(name))
    };
    let out = private("harness.jsonl")?;
    let err = private("harness.stderr")?;
    save(
        &context.work.join("benchmark-attempt.json"),
        &json!({"outcome":"running","elapsed_seconds":null}),
    )?;
    let mut cmd = command(context);
    cmd.args([
        "exec",
        "--strict-config",
        "--ephemeral",
        "--ignore-rules",
        "--json",
        "--color",
        "never",
        "--skip-git-repo-check",
        "--model",
        &context.model,
        &context.task.prompt,
    ])
    .stdin(Stdio::null())
    .stdout(out)
    .stderr(err);
    // Inherit the acceptance worker's process group. Its parent reaps the whole
    // tree on completion/cancellation; creating another group would escape it.
    let start = Instant::now();
    let mut child = cmd.spawn().context("start Codex exec")?;
    let result = (|| -> Result<&str> {
        loop {
            let over = ["harness.jsonl", "harness.stderr"].iter().try_fold(
                false,
                |over, name| -> Result<bool> {
                    Ok(over || fs::metadata(context.work.join(name))?.len() > 32 * 1024 * 1024)
                },
            )?;
            if over {
                return Ok("output_limit");
            }
            if let Some(status) = child.try_wait()? {
                return Ok(if status.success() {
                    "completed"
                } else {
                    "harness_failure"
                });
            }
            if start.elapsed() >= Duration::from_secs(context.task.deadline_seconds.into()) {
                return Ok("timeout");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    save(
        &context.work.join("benchmark-attempt.json"),
        &json!({"outcome":result.as_ref().copied().unwrap_or("harness_failure"),"elapsed_seconds":start.elapsed().as_secs_f64(),"model":context.model,"usage":null,"cost":null}),
    )?;
    result.map(|_| ())
}

pub(super) fn retained(work: &Path) -> Result<AttemptOutput> {
    telemetry::retained(work)
}
