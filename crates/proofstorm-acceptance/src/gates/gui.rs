//! Managed HTTP/lifecycle gate. Never opens a browser, folder picker or agent app.
use crate::{GateContext, process};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    process::Command,
};

fn api(
    record: &Value,
    path: &str,
    body: Option<&Value>,
    auth: bool,
    extra: &[&str],
) -> Result<(u16, Vec<u8>)> {
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "--noproxy",
        "*",
        "--silent",
        "--show-error",
        "--max-time",
        "30",
        "--write-out",
        "\n%{http_code}",
    ]);
    if auth {
        command.args([
            "-H",
            &format!(
                "Authorization: Bearer {}",
                record["token"].as_str().context("GUI token missing")?
            ),
        ]);
    }
    for header in extra {
        command.args(["-H", header]);
    }
    if let Some(body) = body {
        command.args([
            "-H",
            "Content-Type: application/json",
            "--data",
            &body.to_string(),
        ]);
    }
    command.arg(format!(
        "http://127.0.0.1:{}{path}",
        record["port"].as_u64().context("GUI port missing")?
    ));
    let output = process::capture(command, 40)?;
    ensure!(output.status.success(), "GUI HTTP request failed");
    let split = output
        .stdout
        .iter()
        .rposition(|b| *b == b'\n')
        .context("HTTP status missing")?;
    let status = std::str::from_utf8(&output.stdout[split + 1..])?.parse()?;
    Ok((status, output.stdout[..split].to_vec()))
}

pub fn run(context: &GateContext) -> Result<()> {
    let path = context.installation.home.join("gui-process.json");
    ensure!(!path.exists(), "gate requires its owned GUI to be stopped");
    let before = super::onboarding::runtime(context)?;
    eprintln!("Checking GUI HTTP, authentication, and reuse...");
    let outcome = (|| -> Result<()> {
        let project = tempfile::Builder::new()
            .prefix("gui project with spaces ")
            .tempdir_in(context.work())?;
        let project = project.path();
        let mut command = context.command(&["--json", "gui", "start", "--allow-development"])?;
        command.current_dir(project);
        let first = process::json(command, 240)?;
        let bytes = fs::read(&path)?;
        let record: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            fs::metadata(&path)?.permissions().mode() & 0o777 == 0o600,
            "GUI record is not private"
        );
        ensure!(
            !first
                .to_string()
                .contains(record["token"].as_str().context("token missing")?),
            "session leaked into result"
        );
        let second = context.cli(&["gui", "start", "--allow-development"])?;
        ensure!(
            second["reused_server"] == true
                && second["url"] == first["url"]
                && fs::read(&path)? == bytes,
            "GUI was not reused"
        );
        ensure!(
            !project.join(".codex").exists(),
            "opening GUI attached an agent"
        );
        let (status, html) = api(&record, "/", None, false, &[])?;
        ensure!(
            status == 200
                && String::from_utf8_lossy(&html)
                    .to_lowercase()
                    .contains("<html"),
            "embedded UI not served"
        );
        ensure!(
            api(&record, "/v1/environment", None, false, &[])?.0 == 401,
            "anonymous API accepted"
        );
        let action = json!({"project":project});
        ensure!(
            api(&record, "/v1/gui/open", Some(&action), false, &[])?.0 == 403,
            "anonymous launch accepted"
        );
        ensure!(
            api(
                &record,
                "/v1/gui/open",
                Some(&action),
                true,
                &["Origin: https://evil.invalid"]
            )?
            .0 == 403,
            "foreign origin accepted"
        );
        ensure!(
            api(&record, "/v1/environment", None, true, &[])?.0 == 200,
            "authenticated environment failed"
        );
        ensure!(
            api(&record, "/v1/gui/context", None, true, &[])?.0 == 200,
            "GUI context failed"
        );
        let stopped = context.cli(&["gui", "stop"])?;
        ensure!(
            stopped["stopped"] == true && stopped["cells_stopped"] == false && !path.exists(),
            "GUI stop failed"
        );
        ensure!(
            context.cli(&["gui", "stop"])?["reason"] == "not_running",
            "GUI stop not repeatable"
        );
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?
            .write_all(&bytes)?;
        eprintln!("Checking stale GUI recovery and session rotation...");
        ensure!(
            context.cli(&["gui", "start", "--allow-development"])?["reused_server"] == false,
            "stale record not recovered"
        );
        let fresh: Value = serde_json::from_slice(&fs::read(&path)?)?;
        ensure!(
            fresh["instance"] != record["instance"] && fresh["token"] != record["token"],
            "session not rotated"
        );
        let old_auth = format!(
            "Authorization: Bearer {}",
            record["token"].as_str().unwrap()
        );
        ensure!(
            api(&fresh, "/v1/environment", None, false, &[&old_auth])?.0 == 401,
            "old GUI session accepted"
        );
        Ok(())
    })();
    let stopped = context.cli(&["gui", "stop"]);
    stopped?;
    outcome?;
    ensure!(
        before == super::onboarding::runtime(context)?,
        "GUI changed controller/cell workloads"
    );
    context.record(
        "gui.json",
        &json!({"passed":true,"http_and_lifecycle":true,"session_rotation":true,
        "browser_visual_test":"not_run","native_app_launch":"not_run","model_tool_call":false}),
    )
}
