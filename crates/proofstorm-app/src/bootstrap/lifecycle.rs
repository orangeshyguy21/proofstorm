//! Reversible installation shutdown. Persist intent before changing runtime resources.
#[cfg(test)]
mod tests;
mod workloads;

use super::{docker, process, teardown};
use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
use workloads::Workload;

const JOURNAL: &str = "runtime-lifecycle.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Stopping,
    Stopped,
    Starting,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format_version: u32,
    installation_id: String,
    phase: Phase,
    controller: Option<Workload>,
    workloads: Option<Vec<Workload>>,
    workloads_stopped: bool,
    tools_running: bool,
}

fn read(home: &Path) -> Result<Option<Journal>> {
    let path = home.join(JOURNAL);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && metadata.len() <= 4 * 1024 * 1024,
        "invalid runtime lifecycle record"
    );
    let journal: Journal = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        journal.format_version == 1 && journal.installation_id == Installation::load(home)?.id,
        "runtime lifecycle record belongs to another installation"
    );
    Ok(Some(journal))
}

fn save(home: &Path, journal: &Journal) -> Result<()> {
    process::save(&home.join(JOURNAL), &serde_json::to_vec(journal)?)?;
    File::open(home)?.sync_all()?;
    Ok(())
}

/// Checked by long-lived clients for each runtime call, not just at startup.
pub fn ensure_available(home: &Path) -> Result<()> {
    if let Some(journal) = read(home)? {
        anyhow::bail!(
            "runtime is {}; run {} start to resume, or {} stop to finish shutdown",
            match journal.phase {
                Phase::Stopping => "stopping",
                Phase::Stopped => "stopped",
                Phase::Starting => "starting",
            },
            crate::command_name(),
            crate::command_name()
        );
    }
    Ok(())
}

/// A shared cross-process lease closes the check/submission race with shutdown.
/// The OS releases it on cancellation, normal completion, or process death.
#[derive(Debug)]
pub struct AccessGuard {
    _lock: Flock<File>,
}

fn lock(home: &Path, exclusive: bool) -> Result<Flock<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(home.join("runtime-access.lock"))?;
    ensure!(file.metadata()?.is_file(), "invalid runtime access lock");
    Flock::lock(
        file,
        if exclusive {
            FlockArg::LockExclusiveNonblock
        } else {
            FlockArg::LockSharedNonblock
        },
    )
    .map_err(|(_, error)| anyhow::anyhow!(error))
}

pub fn access(installation: Option<&Installation>) -> Result<Option<AccessGuard>> {
    let Some(installation) = installation else {
        return Ok(None);
    };
    ensure_available(&installation.home)?;
    let guard = lock(&installation.home, false)
        .context("runtime lifecycle transition in progress; retry")?;
    ensure_available(&installation.home)?;
    Ok(Some(AccessGuard { _lock: guard }))
}

async fn exclusive(home: &Path, deadline: Instant) -> Result<Flock<File>> {
    loop {
        match lock(home, true) {
            Ok(guard) => return Ok(guard),
            Err(error)
                if error.downcast_ref::<nix::errno::Errno>()
                    == Some(&nix::errno::Errno::EWOULDBLOCK) => {}
            Err(error) => return Err(error),
        }
        ensure!(
            Instant::now() < deadline,
            "active client calls have not finished; retry stop or run start to cancel shutdown"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn running(home: &Path, id: &str) -> Result<bool> {
    let output = docker(
        home,
        &[
            "inspect",
            "--type",
            "container",
            "--format",
            "{{.State.Running}}",
            id,
        ],
        15,
    )?;
    match output.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!("invalid container state"),
    }
}

/// A stopped runtime is healthy but unavailable. Partial transitions remain explicit.
pub(super) fn doctor(installation: &Installation) -> Result<Option<Value>> {
    let Some(journal) = read(&installation.home)? else {
        return Ok(None);
    };
    ensure!(
        journal.phase == Phase::Stopped,
        "runtime transition incomplete; retry stop or start"
    );
    let containers = teardown::preserved_runtime(installation)?;
    for id in containers.values() {
        ensure!(
            !running(&installation.home, id)?,
            "stopped runtime has a running container; retry stop"
        );
    }
    Ok(Some(
        json!({"state":"stopped","controller_ready":false,"storage_preserved":true,"next":"Run storm start"}),
    ))
}

/// Start never rebuilds, pulls images, runs Helm, or creates replacement resources.
pub async fn run(
    home: &Path,
    start: bool,
    timeout_seconds: u32,
    progress: &dyn Fn(&str),
) -> Result<Value> {
    let installation = Installation::load(home)?;
    let _installation = Installation::lock(&installation.home)?;
    let containers = teardown::preserved_runtime(&installation)?;
    let previous = read(home)?;
    let deadline = Instant::now() + Duration::from_secs(u64::from(timeout_seconds));
    if start
        && previous.is_none()
        && containers
            .iter()
            .filter(|(name, _)| !name.ends_with("-tools"))
            .map(|(_, id)| running(home, id))
            .collect::<Result<Vec<_>>>()?
            .iter()
            .all(|running| *running)
    {
        let unready = workloads::wait_ready(&installation, None, deadline).await?;
        return Ok(
            json!({"state":"running","ready":unready.is_empty(),"unready_workloads":unready,"already_running":true,"home":home,
            "installation_id":installation.id,"storage_preserved":true,"gui_running":crate::gui::status(home).await?["state"] == "running"}),
        );
    }
    let mut journal = previous.unwrap_or(Journal {
        format_version: 1,
        installation_id: installation.id.clone(),
        phase: Phase::Stopping,
        controller: None,
        workloads: None,
        workloads_stopped: false,
        tools_running: containers
            .iter()
            .find(|(name, _)| name.ends_with("-tools"))
            .map(|(_, id)| running(home, id))
            .transpose()?
            .unwrap_or(false),
    });
    if !start {
        ensure!(
            journal.phase != Phase::Starting,
            "start is incomplete; retry start before stopping again"
        );
    }
    journal.phase = if start {
        Phase::Starting
    } else {
        Phase::Stopping
    };
    save(home, &journal)?;
    progress("Waiting for active client calls");
    // Existing connection processes observe the marker and close their tunnels.
    let _access = exclusive(home, deadline).await?;
    progress("Stopping GUI");
    crate::gui::stop(home).await?;
    let result = if start {
        resume(&installation, &mut journal, deadline, progress).await
    } else {
        suspend(&installation, &mut journal, deadline, progress)
            .await
            .map(|()| Vec::new())
    };
    let unready = result.with_context(|| {
        format!(
            "runtime {} incomplete; retry {} {} (saved storage is retained)",
            if start { "start" } else { "stop" },
            crate::command_name(),
            if start { "start" } else { "stop" }
        )
    })?;
    // Recheck exact ownership and all states before claiming completion.
    ensure!(
        containers == teardown::preserved_runtime(&installation)?,
        "runtime identity changed during transition"
    );
    for (name, id) in &containers {
        ensure!(
            running(home, id)? == (start && (!name.ends_with("-tools") || journal.tools_running)),
            "runtime container {name} did not reach the requested state"
        );
    }
    if start {
        fs::remove_file(home.join(JOURNAL))?;
        File::open(home)?.sync_all()?;
    } else {
        journal.phase = Phase::Stopped;
        save(home, &journal)?;
    }
    Ok(
        json!({"state":if start {"running"} else {"stopped"},"ready":start && unready.is_empty(),"unready_workloads":unready,
        "home":home,"installation_id":installation.id,"storage_preserved":true,
        "gui_running":false,"next":if start {"Run storm gui to open the GUI"} else {"Run storm start to resume"}}),
    )
}

async fn suspend(
    installation: &Installation,
    journal: &mut Journal,
    deadline: Instant,
    progress: &dyn Fn(&str),
) -> Result<()> {
    if !journal.workloads_stopped {
        progress("Waiting for active operations to finish");
        if journal.controller.is_none() {
            workloads::drain(installation, deadline).await?;
            journal.controller = Some(workloads::controller(installation)?);
            save(&installation.home, journal)?;
        }
        let controller = journal
            .controller
            .as_ref()
            .context("controller snapshot missing")?;
        progress("Stopping controller");
        workloads::scale(installation, controller, 0)?;
        workloads::wait_stopped(installation, std::slice::from_ref(controller), deadline).await?;
        if journal.workloads.is_none() {
            // Recheck after the controller exits; no admitted action may be abandoned.
            workloads::drain(installation, deadline).await?;
            journal.workloads = Some(workloads::inventory(installation)?);
            save(&installation.home, journal)?;
        }
        progress("Shutting down cell services; preserving volumes");
        let workloads = journal
            .workloads
            .as_ref()
            .context("workload snapshot missing")?;
        for workload in workloads {
            workloads::scale(installation, workload, 0)?;
        }
        workloads::wait_stopped(installation, workloads, deadline).await?;
        journal.workloads_stopped = true;
        save(&installation.home, journal)?;
    }
    progress("Stopping runtime containers and registry");
    let containers = teardown::preserved_runtime(installation)?;
    // Kubernetes workloads are already shut down. Keep the server and registry last.
    for suffix in ["tools", "serverlb", "agent-0", "server-0", "registry"] {
        for (name, id) in &containers {
            if name.ends_with(&format!("-{suffix}")) && running(&installation.home, id)? {
                docker(&installation.home, &["stop", "--time", "60", id], 75)?;
            }
        }
    }
    Ok(())
}

async fn resume(
    installation: &Installation,
    journal: &mut Journal,
    deadline: Instant,
    progress: &dyn Fn(&str),
) -> Result<Vec<String>> {
    progress("Starting saved runtime containers");
    let containers = teardown::preserved_runtime(installation)?;
    for suffix in ["registry", "server-0", "agent-0", "serverlb", "tools"] {
        if suffix == "tools" && !journal.tools_running {
            continue;
        }
        for (name, id) in &containers {
            if name.ends_with(&format!("-{suffix}")) && !running(&installation.home, id)? {
                docker(&installation.home, &["start", id], 30)?;
            }
        }
    }
    progress("Waiting for Kubernetes");
    workloads::wait_api(installation, deadline).await?;
    progress("Restoring saved cell services");
    if let Some(workloads) = &journal.workloads {
        // A retry may have reached controller startup. Quiesce it before restoring placement.
        let controller = journal
            .controller
            .as_ref()
            .context("controller snapshot missing")?;
        workloads::scale(installation, controller, 0)?;
        workloads::wait_stopped(installation, std::slice::from_ref(controller), deadline).await?;
        let (persistent, stateless): (Vec<_>, Vec<_>) = workloads
            .iter()
            .cloned()
            .partition(|workload| workload.persistent);
        for workload in &stateless {
            workloads::scale(installation, workload, 0)?;
        }
        workloads::wait_stopped(installation, &stateless, deadline).await?;
        // Cell pods share node affinity. Let retained volumes establish placement before
        // stateless services can pull the cell onto a node that cannot mount its storage.
        for workload in &persistent {
            workloads::scale(installation, workload, workload.replicas)?;
        }
        workloads::wait_scheduled(installation, &persistent, deadline).await?;
        for workload in &stateless {
            workloads::scale(installation, workload, workload.replicas)?;
        }
    }
    if let Some(controller) = &journal.controller {
        workloads::scale(installation, controller, controller.replicas)?;
    }
    progress("Waiting for controller and cell services");
    workloads::wait_ready(installation, journal.workloads.as_deref(), deadline).await
}
