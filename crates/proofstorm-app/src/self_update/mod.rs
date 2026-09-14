//! Explicit managed file updates. Runtime refresh is a separate user action.
mod download;
mod managed;
mod process;
mod schema;

use anyhow::{Context, Result, ensure};
use download::Download;
use managed::Managed;
use process::Cancellation;
use serde::Serialize;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Serialize)]
pub struct Fault {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct FollowUp {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent observations in the public result schema"
)]
pub struct UpdateResult {
    pub schema_version: u32,
    pub status: String,
    pub previous_version: Option<String>,
    pub available_version: Option<String>,
    pub installed_version: Option<String>,
    pub channel: Option<String>,
    pub platform: Option<String>,
    pub installation_prefix: Option<PathBuf>,
    pub selected_release_id: Option<u64>,
    pub activation_changed: bool,
    pub activation_observed: bool,
    pub verification_passed: bool,
    pub runtime_refreshed: bool,
    pub required_actions: Vec<FollowUp>,
    pub error: Option<Fault>,
}
impl Default for UpdateResult {
    fn default() -> Self {
        Self {
            schema_version: 1,
            status: "failed".into(),
            previous_version: None,
            available_version: None,
            installed_version: None,
            channel: None,
            platform: None,
            installation_prefix: None,
            selected_release_id: None,
            activation_changed: false,
            activation_observed: false,
            verification_passed: false,
            runtime_refreshed: false,
            required_actions: vec![],
            error: None,
        }
    }
}
impl UpdateResult {
    #[must_use]
    pub fn success(&self) -> bool {
        self.error.is_none() && self.status != "failed" && self.status != "activated_with_error"
    }
}

pub async fn run(check: bool, home: Option<&Path>, progress: &dyn Fn(&str)) -> UpdateResult {
    let mut result = UpdateResult::default();
    let outcome = async {
        let installation =
            Managed::resolve(&std::env::current_exe()?, &crate::release::describe())?;
        let http = download::Http::new()?;
        let mut cancel = Cancellation::new()?;
        execute(
            &installation,
            &http,
            check,
            home,
            progress,
            &mut cancel,
            &mut result,
        )
        .await
    }
    .await;
    if let Err(error) = outcome {
        if result.error.is_none() {
            result.error = Some(Fault {
                code: "update_unavailable".into(),
                message: format!("{error:#}"),
            });
        }
    }
    result
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one update transaction retains selection, activation and outcome across failures"
)]
async fn execute(
    installation: &Managed,
    http: &impl Download,
    check: bool,
    home: Option<&Path>,
    progress: &dyn Fn(&str),
    cancel: &mut Cancellation,
    result: &mut UpdateResult,
) -> Result<()> {
    result.previous_version = Some(installation.version.clone());
    result.installed_version = Some(installation.version.clone());
    result.channel = Some(installation.channel.clone());
    result.installation_prefix = Some(installation.prefix.clone());
    result.platform = Some(schema::platform()?.into());
    let mut stage = "release_discovery_failed";
    let work = async {
        progress("Checking official release");
        let bytes = http.metadata(cancel).await?;
        stage = "invalid_release_metadata";
        let release: schema::Release = serde_json::from_slice(&bytes)?;
        let platform = release.selected(schema::platform()?, &installation.channel)?;
        result.available_version = Some(release.version.clone());
        result.selected_release_id = Some(release.release_id);
        let ordering = semver::Version::parse(&installation.version)?.cmp_precedence(&semver::Version::parse(&release.version)?);
        let (active_id, active) = installation.active()?;
        result.activation_observed = true;
        result.activation_changed = active_id != installation.id;
        let active_info = managed::json(&active.join("release-info.json"), 2 * 1024 * 1024)?;
        result.installed_version = active_info["version"].as_str().map(str::to_owned);
        result.required_actions = follow_ups(installation, home, &active_info, result.activation_changed);
        ensure!(active == installation.bundle, "active installation changed; retry from its current launcher");
        if check {
            result.status = match ordering { std::cmp::Ordering::Less => "update_available", std::cmp::Ordering::Equal => "up_to_date", std::cmp::Ordering::Greater => "installed_newer" }.into();
            return Ok(());
        }
        stage = "installation_verification_failed";
        if !ordering.is_lt() {
            match installation.verify(&active).await {
                Ok(_) => {
                    ensure!(installation.active()?.0 == installation.id, "activation changed during verification; retry");
                    result.verification_passed = true;
                    result.status = if ordering.is_eq() { "up_to_date" } else { "installed_newer" }.into();
                    return Ok(());
                }
                Err(error) if ordering.is_gt() => anyhow::bail!("installed files need repair but the feed is older; use the official installer with --version {} --prefix {}: {error:#}", installation.version, installation.prefix.display()),
                Err(_) => { progress("Repairing current installation"); }
            }
        }
        stage = "release_download_failed";
        let scratch = tempfile::tempdir()?;
        // Fetch through one restricted HTTPS client. The existing installer still
        // owns archive verification, extraction and activation.
        for asset in [&release.installer, &platform.archive, &platform.checksum] {
            progress(if asset.name == "install.sh" { "Downloading verified installer" } else { "Downloading release files" });
            http.asset(asset, &scratch.path().join(&asset.name), cancel).await?;
        }
        progress("Verifying downloaded release");
        for asset in [&release.installer, &platform.archive, &platform.checksum] {
            let path = scratch.path().join(&asset.name);
            ensure!(std::fs::metadata(&path)?.len() == asset.bytes && crate::artifacts::hash(&path)? == asset.sha256, "download differs from captured release metadata");
        }
        stage = "installation_failed";
        progress("Installing release files");
        let mut command = Command::new("/bin/sh");
        command.arg(scratch.path().join("install.sh"))
            .arg("--prefix").arg(&installation.prefix)
            .arg("--version").arg(&release.version)
            .arg("--archive").arg(&platform.archive.name)
            .arg("--artifact-dir").arg(scratch.path())
            .arg("--expected-sha256").arg(&platform.archive.sha256)
            .arg("--expected-bytes").arg(platform.archive.bytes.to_string())
            .arg("--expected-current").arg(&installation.id)
            .arg("--report-json");
        result.activation_observed = false;
        result.installed_version = None;
        result.required_actions.clear();
        let installed = process::run(command, Duration::from_secs(300), Some(cancel)).await;
        // Inspect even after interruption or a late installer failure.
        progress("Verifying installed release");
        let pointer = std::fs::read_link(installation.root.join("current"))?;
        result.activation_changed = pointer != PathBuf::from("versions").join(&installation.id);
        let (active_id, active) = installation.active()?;
        result.activation_observed = true;
        result.activation_changed = active_id != installation.id;
        let info = managed::json(&active.join("release-info.json"), 2 * 1024 * 1024)?;
        result.installed_version = info["version"].as_str().map(str::to_owned);
        result.required_actions = follow_ups(installation, home, &info, result.activation_changed);
        let verification = installation.verify(&active).await;
        result.verification_passed = verification.is_ok();
        let installed = installed?;
        ensure!(installed.success, "installer {}: {} {}", installed.failure.unwrap_or("failed"), installed.stderr, installed.stdout);
        let receipt: Value = serde_json::from_str(&installed.stdout).context("installer returned an invalid receipt")?;
        ensure!(receipt["installed"] == true && receipt["bundle_id"] == active_id && receipt["version"] == release.version && receipt["prefix"] == serde_json::json!(installation.prefix), "installer receipt differs from selected installation");
        stage = "installation_verification_failed";
        verification?;
        ensure!(info["version"] == release.version && info["target"] == crate::platform::target(), "activated release differs from selected release");
        ensure!(installation.active()?.0 == active_id, "another activation occurred during verification; retry");
        result.status = "updated".into();
        Ok(())
    }.await;
    if let Err(error) = work {
        result.status = if result.activation_changed {
            "activated_with_error"
        } else {
            "failed"
        }
        .into();
        result.error = Some(Fault {
            code: stage.into(),
            message: format!("{error:#}"),
        });
    }
    Ok(())
}

fn follow_ups(
    installation: &Managed,
    home: Option<&Path>,
    info: &Value,
    changed: bool,
) -> Vec<FollowUp> {
    let default_home = installation.root.join("state");
    let home = home.unwrap_or(&default_home);
    let mut actions = Vec::new();
    if !home.join("installation.json").exists() {
        return actions;
    }
    let Ok(state) = crate::installation::Installation::load(home) else {
        return vec![FollowUp {
            message:
                "Runtime state could not be inspected; run doctor for this home before using it."
                    .into(),
            command: Some(advice_command(installation, home, &["doctor"])),
        }];
    };
    // Advice must still name this installation when run from another directory.
    let home = &state.home;
    let deployed = managed::json(&home.join("deployment-inputs.json"), 65536).ok();
    let refresh = deployed.as_ref().is_none_or(|d| {
        d["installation_id"] != state.id || d["image"] != info["controller"]["image"]
    });
    let gui = managed::json(&home.join("gui-process.json"), 65536).ok();
    let old_gui = gui.as_ref().is_some_and(|g| {
        g["installation_id"] == state.id
            && g["executable"]
                != serde_json::json!(
                    installation
                        .root
                        .join("current/bin/proofstorm")
                        .canonicalize()
                        .unwrap_or_default()
                )
    });
    if refresh || changed {
        actions.push(FollowUp { message: "Disconnect existing Proofstorm MCP sessions before refreshing the runtime; reconnect them afterwards. Run agent configure again if the attachment requires reconfiguration.".into(), command: None });
    }
    if gui.is_some() && (refresh || old_gui) {
        actions.push(FollowUp {
            message: "Stop the recorded GUI before runtime refresh.".into(),
            command: Some(advice_command(installation, home, &["gui", "stop"])),
        });
    }
    if refresh {
        actions.push(FollowUp {
            message: "Refresh the controller and tools; installed files were updated separately."
                .into(),
            command: Some(advice_command(installation, home, &["setup"])),
        });
    }
    if gui.is_some() && (refresh || old_gui) {
        actions.push(FollowUp {
            message: "Start the GUI from the active release.".into(),
            command: Some(advice_command(installation, home, &["gui"])),
        });
    }
    actions
}

fn advice_command(installation: &Managed, home: &Path, args: &[&str]) -> Vec<String> {
    let mut command = vec![
        installation
            .prefix
            .join("bin/proofstorm")
            .display()
            .to_string(),
        "--home".into(),
        home.display().to_string(),
    ];
    command.extend(args.iter().map(|s| (*s).to_owned()));
    command
}

#[cfg(test)]
mod tests;
