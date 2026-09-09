//! Installed GUI lifecycle and authenticated, project-scoped onboarding.
mod server;
mod state;
#[cfg(test)]
mod tests;
pub(crate) mod transport;

use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
pub(crate) use server::Session;
use state::{RECORD, Record};
use std::os::unix::process::CommandExt;
use std::{
    io::{Read, Seek},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?)
}

async fn health(record: &Record) -> Result<bool> {
    if record.port == 0 || record.pid == 0 {
        return Ok(false);
    }
    let response = client()?
        .get(format!("{}/v1/gui/health", record.url()))
        .bearer_auth(&record.token)
        .send()
        .await?;
    Ok(response.status().is_success() && response.json::<Value>().await? == record.health())
}

pub async fn open(
    home: &Path,
    project: &Path,
    bundle: &Path,
    allow_development: bool,
    no_open: bool,
) -> Result<Value> {
    let installation = Installation::load(home)?;
    let allow_development = crate::artifacts::verify(home, bundle, allow_development)?;
    let project = project
        .canonicalize()
        .context("GUI project directory must exist")?;
    ensure!(project.is_dir(), "GUI project must be a directory");
    let _control = state::lease(&installation.home, "gui-control-lock.sqlite3")?;
    let executable = std::env::current_exe()?.canonicalize()?;
    let previous = state::record(&installation.home, &installation.id)?;
    let (record, reused) = if let Some(record) = previous.as_ref().filter(|r| r.port != 0) {
        if health(record).await.unwrap_or(false) {
            ensure!(
                record.executable == executable
                    && record
                        .build_sha256
                        .as_ref()
                        .is_none_or(|sha| crate::artifacts::hash(&executable)
                            .is_ok_and(|current| &current == sha)),
                "GUI uses an older bundle; run proofstorm stop, then proofstorm gui"
            );
            (record.clone(), true)
        } else {
            (
                start(&installation, &executable, allow_development).await?,
                false,
            )
        }
    } else {
        (
            start(&installation, &executable, allow_development).await?,
            false,
        )
    };
    let browser = if no_open {
        "not_requested"
    } else if activate(&record, &project).await.unwrap_or(false) {
        "existing_tab_focused"
    } else {
        let fragment = serde_urlencoded::to_string([
            ("session", record.token.as_str()),
            ("project", project.to_str().context("non-UTF-8 project")?),
        ])?;
        let url = format!("{}/#{fragment}", record.url());
        // Delegate to macOS's default URL handler; never pick a browser or install one.
        let status = Command::new("/usr/bin/open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        ensure!(
            status.success(),
            "default browser could not be opened; GUI server is still running"
        );
        "opened_default_browser"
    };
    Ok(
        json!({"url":record.url(),"reused_server":reused,"browser":browser,"project":project,
        "attached":false,"tab_focus":"best_effort","note":"Opening the GUI does not attach tools. Stop only this GUI with proofstorm stop; labs keep running."}),
    )
}

async fn start(
    installation: &Installation,
    executable: &Path,
    allow_development: bool,
) -> Result<Record> {
    // A held lifetime lease prevents replacing an unresponsive but live GUI.
    let lifetime = state::lease(&installation.home, "gui-runtime-lock.sqlite3")
        .context("GUI is running but not responding; no process was stopped or replaced")?;
    let record = Record {
        format_version: 1,
        installation_id: installation.id.clone(),
        instance: state::random::<16>()?,
        token: state::random::<32>()?,
        executable: executable.to_path_buf(),
        build_sha256: Some(crate::artifacts::hash(executable)?),
        pid: 0,
        port: 0,
    };
    state::save(&installation.home.join(RECORD), &record)?;
    drop(lifetime);
    let mut command = tokio::process::Command::new(executable);
    command.as_std_mut().process_group(0);
    command.current_dir(&installation.home);
    command
        .arg("--home")
        .arg(&installation.home)
        .arg("gui-serve")
        .arg("--instance")
        .arg(&record.instance);
    if allow_development {
        command.arg("--allow-development");
    }
    // Never carry developer runtime overrides into the installed GUI process.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PROOFSTORM_") {
            command.env_remove(key);
        }
    }
    let mut startup_errors = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(startup_errors.try_clone()?);
    let mut child = command.spawn().context("start installed GUI")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if child.try_wait()?.is_some() {
            startup_errors.rewind()?;
            let mut details = String::new();
            startup_errors.take(8192).read_to_string(&mut details)?;
            anyhow::bail!(
                "GUI startup failed: {}. Run proofstorm doctor, then retry proofstorm gui",
                details.trim()
            );
        }
        if let Some(ready) = state::record(&installation.home, &installation.id)? {
            ensure!(
                ready.instance == record.instance,
                "GUI owner changed during startup"
            );
            if health(&ready).await.unwrap_or(false) {
                return Ok(ready);
            }
        }
        if tokio::time::Instant::now() > deadline {
            // This handle refers to our own startup child, never a recorded/recycled PID.
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("GUI startup timed out; run proofstorm doctor, then retry");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn activate(record: &Record, project: &Path) -> Result<bool> {
    let response = client()?
        .post(format!("{}/v1/gui/activate", record.url()))
        .bearer_auth(&record.token)
        .json(&json!({"project":project}))
        .send()
        .await?;
    Ok(response.status().is_success() && response.json::<Value>().await?["focused"] == true)
}

pub async fn stop(home: &Path) -> Result<Value> {
    let installation = Installation::load(home)?;
    let _control = state::lease(&installation.home, "gui-control-lock.sqlite3")?;
    let Some(record) = state::record(&installation.home, &installation.id)? else {
        return Ok(json!({"stopped":false,"labs_stopped":false,"reason":"not_running"}));
    };
    if !health(&record).await.unwrap_or(false) {
        let _lifetime = state::lease(&installation.home, "gui-runtime-lock.sqlite3")
            .context("GUI ownership could not be verified; no process was stopped")?;
        state::remove_owned(&installation.home, &record)?;
        return Ok(json!({"stopped":false,"labs_stopped":false,"stale_record_removed":true}));
    }
    let response = client()?
        .post(format!("{}/v1/gui/stop", record.url()))
        .bearer_auth(&record.token)
        .send()
        .await;
    if let Ok(response) = response {
        ensure!(
            response.status().is_success(),
            "GUI is busy; retry stop after the attachment finishes"
        );
    }
    // The owned server can exit before its final HTTP response is flushed.
    // Confirm that its lifetime lease was released even if the connection closed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(_lifetime) = state::lease(&installation.home, "gui-runtime-lock.sqlite3") {
            state::remove_owned(&installation.home, &record)?;
            return Ok(json!({"stopped":true,"labs_stopped":false}));
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "GUI shutdown could not be confirmed; no PID was killed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn serve(
    home: &Path,
    bundle: &Path,
    instance: &str,
    allow_development: bool,
) -> Result<()> {
    server::serve(home, bundle, instance, allow_development).await
}
