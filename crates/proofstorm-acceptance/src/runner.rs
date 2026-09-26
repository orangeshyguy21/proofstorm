//! Disposable live-test lifecycle. Setup uses the verified product CLI;
//! teardown uses the shared installation resource-receipt implementation.
use crate::{GateContext, client::clear_runtime_environment, gates};
use anyhow::{Context, Result, ensure};
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::{Pid, getpgrp},
};
use proofstorm_app::{artifacts::TestArtifacts, installation::Installation};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub struct Selection {
    pub benchmark_model: Option<String>,
    pub benchmark_opencode: PathBuf,
    pub qualification: Option<(PathBuf, String)>,
    pub checkout_home: Option<PathBuf>,
    pub bundle: Option<PathBuf>,
    pub allow_development: bool,
}

impl Selection {
    fn artifacts(&self) -> Result<TestArtifacts> {
        match (&self.checkout_home, &self.bundle) {
            (Some(home), None) => TestArtifacts::checkout(home),
            (None, Some(bundle)) => TestArtifacts::bundle(bundle, self.allow_development),
            _ => anyhow::bail!(
                "select --checkout-home or --bundle explicitly; existing runtimes and ambient PROOFSTORM_HOME are never used"
            ),
        }
    }

    fn arguments(&self, command: &mut Command) {
        if let Some(model) = &self.benchmark_model {
            command.arg("--benchmark-model").arg(model);
            command
                .arg("--benchmark-opencode")
                .arg(&self.benchmark_opencode);
        }
        if let Some((plan, case)) = &self.qualification {
            command
                .arg("--qualification-plan")
                .arg(plan)
                .arg("--qualification-case")
                .arg(case);
        }
        if let Some(home) = &self.checkout_home {
            command.arg("--checkout-home").arg(home);
        }
        if let Some(bundle) = &self.bundle {
            command.arg("--bundle").arg(bundle);
        }
        if self.allow_development {
            command.arg("--allow-development");
        }
    }
}

pub fn validate_gates(names: &[String]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        ensure!(
            gates::NAMES.contains(&name.as_str()),
            "unknown gate {name}; use --list"
        );
        ensure!(seen.insert(name), "duplicate gate {name}");
    }
    if names.iter().any(|name| name == "onboarding") {
        ensure!(
            names.first().is_some_and(|name| name == "onboarding"),
            "onboarding must be the first gate"
        );
        ensure!(
            !names.iter().any(|name| name == "slice2"),
            "onboarding's on-demand check cannot share slice2's prefetched setup"
        );
    }
    Ok(())
}

fn save(work: &Path, report: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(work)?;
    file.write_all(&serde_json::to_vec_pretty(report)?)?;
    file.as_file().sync_all()?;
    file.persist(work.join("acceptance.json"))?;
    Ok(())
}

pub(crate) fn read(work: &Path) -> Result<(Installation, Value)> {
    let work = work.canonicalize()?;
    let path = work.join("acceptance.json");
    ensure!(
        fs::symlink_metadata(&path)?.is_file(),
        "linked acceptance report refused"
    );
    let report: Value = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        report["format_version"] == 1 && report["work"] == work.to_string_lossy().as_ref(),
        "acceptance report belongs to another work directory"
    );
    let installation = Installation::load(&work.join("state"))?;
    ensure!(
        report["installation_id"] == installation.id,
        "acceptance installation identity changed"
    );
    Ok((installation, report))
}

pub(crate) fn read_peer(work: &Path) -> Result<(Installation, Value)> {
    let (parent, _) = read(work)?;
    let peer = work.join("peer");
    ensure!(
        fs::symlink_metadata(&peer)?.is_dir(),
        "linked peer directory refused"
    );
    let (installation, report) = read(&peer)?;
    ensure!(
        report["parent_installation_id"] == parent.id,
        "peer belongs to another acceptance run"
    );
    ensure!(installation.id != parent.id, "peer shares parent identity");
    Ok((installation, report))
}

/// Explicit recovery after interruption. Reports and database are never removed.
pub fn cleanup(work: &Path) -> Result<()> {
    let (installation, mut report) = read(work)?;
    let mut errors = Vec::new();
    let peer = work.join("peer");
    if peer.join("state/runtime-resources.json").exists() {
        let result = (|| -> Result<()> {
            read_peer(work)?;
            cleanup(&peer)
        })();
        if let Err(error) = result {
            errors.push(format!("peer: {error:#}"));
        }
    }
    if installation.home.join("gui-process.json").exists() {
        let home = installation.home.clone();
        let result = std::thread::spawn(move || -> Result<()> {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(proofstorm_app::gui::stop(&home))?;
            Ok(())
        })
        .join()
        .map_err(|_| anyhow::anyhow!("GUI cleanup worker failed"))
        .and_then(std::convert::identity);
        if let Err(error) = result {
            errors.push(format!("GUI: {error:#}"));
        }
    }
    let result = proofstorm_app::bootstrap::teardown::retire(
        &installation.home,
        &installation.id,
        &|label| eprintln!("{label}..."),
    );
    if let Err(error) = result {
        errors.push(format!("runtime: {error:#}"));
    }
    report["cleanup"] = json!(if errors.is_empty() {
        "passed"
    } else {
        "failed"
    });
    report["cleanup_errors"] = json!(errors);
    save(&work.canonicalize()?, &report)?;
    ensure!(
        errors.is_empty(),
        "cleanup incomplete: {}",
        errors.join("; ")
    );
    Ok(())
}

/// Internal worker entry. It cannot select the checkout's live state as its home.
pub fn worker(selection: &Selection, root: &Path, home: &Path, name: &str) -> Result<()> {
    let (installation, _) = read(home.parent().context("worker work directory missing")?)?;
    ensure!(
        installation.home == home.canonicalize()?,
        "worker home mismatch"
    );
    let mut context = GateContext::new(root, installation, selection.artifacts()?)?;
    if let Some((path, id)) = &selection.qualification {
        let plan: proofstorm_qualification::Plan = serde_json::from_slice(&fs::read(path)?)?;
        plan.validate()?;
        let case = plan.case(id)?.clone();
        ensure!(
            case.required && name == "qualification",
            "qualification worker gate mismatch"
        );
        crate::qualification::require_native(&case.platform)?;
        context.qualification_observer = Some(crate::qualification::Observer::new(case.clone()));
        context.qualification = Some(case);
    }
    context.benchmark =
        selection
            .benchmark_model
            .as_ref()
            .map(|model| crate::benchmark::Selection {
                model: model.clone(),
                executable: selection.benchmark_opencode.clone(),
            });
    let result = gates::run(name, &context).and_then(|()| {
        if let Some(observer) = &context.qualification_observer {
            observer.finish()?;
        }
        Ok(())
    });
    if let Err(error) = &result {
        context.record(
            "gate-failure.json",
            &crate::diagnostics::gate_failure(error),
        )?;
    }
    result
}

fn command(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    clear_runtime_environment(&mut command);
    command.env("KUBECONFIG", home.join("kubeconfig"));
    command.stdin(Stdio::null());
    command
}

// Keep cleanup tied to the child we spawned, including inspection/error paths.
// Do not shell out to `kill`: Linux utilities may parse a negative group ID as
// another option, leaving worker descendants alive after a timeout.
struct Worker {
    child: Child,
    group: Option<Pid>,
}

impl Worker {
    fn spawn(mut command: Command) -> Result<Self> {
        use std::os::unix::process::CommandExt;
        let mut child = command
            .process_group(0)
            .spawn()
            .context("start acceptance operation")?;
        let group = i32::try_from(child.id())
            .ok()
            .filter(|id| *id > 1 && *id != getpgrp().as_raw());
        let Some(group) = group else {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("acceptance worker has no distinct owned process group");
        };
        Ok(Self {
            child,
            group: Some(Pid::from_raw(group)),
        })
    }

    fn stop(&mut self) -> Result<()> {
        let Some(group) = self.group.take() else {
            return Ok(());
        };
        let signal = match killpg(group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(error),
        };
        if signal.is_err() {
            let _ = self.child.kill();
        }
        let reaped = self.child.wait();
        signal.context("terminate owned acceptance process group")?;
        reaped.context("reap acceptance worker")?;
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn execute(
    mut command: Command,
    log: &Path,
    seconds: u64,
    cancelled: Option<&AtomicBool>,
) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(log)?;
    command
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    // Workers and their MCP/port-forward children share a fresh process group.
    let mut worker = Worker::spawn(command)?;
    let start = Instant::now();
    let mut last_progress = Instant::now();
    loop {
        if let Some(status) = worker.child.try_wait()? {
            // A failed/finished worker must not leave port-forwards or helper children.
            worker.stop()?;
            if !status.success() && log.file_name().is_some_and(|name| name == "setup.log") {
                eprintln!(
                    "Setup failure: {}",
                    crate::diagnostics::setup_failure(
                        log.parent().context("setup log parent missing")?
                    )
                );
            }
            ensure!(
                status.success(),
                "acceptance operation failed ({status}); see {}",
                log.display()
            );
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(seconds)
            || cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            worker.stop()?;
            anyhow::bail!(
                "acceptance operation interrupted or timed out; see {}",
                log.display()
            );
        }
        if last_progress.elapsed() >= Duration::from_secs(30) {
            eprintln!(
                "Still running; progress log: {}; progress: {}",
                log.display(),
                crate::diagnostics::progress(
                    log.parent().context("progress log parent missing")?,
                    log,
                    start.elapsed().as_secs()
                )
            );
            last_progress = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn cli(artifacts: &TestArtifacts, home: &Path) -> Command {
    let mut command = command(&artifacts.cli, home);
    command.arg("--home").arg(home).arg("--json");
    command
}

fn start_runtime(
    artifacts: &TestArtifacts,
    work: &Path,
    names: &[String],
    allow_development: bool,
    cancelled: &AtomicBool,
    report: &mut Value,
) -> Result<()> {
    let home = work.join("state");
    if let Some((source, web)) = &artifacts.checkout {
        let mut register = cli(artifacts, &home);
        register
            .args(["internal", "checkout-register", "--source"])
            .arg(source)
            .arg("--resources")
            .arg(&artifacts.resources)
            .arg("--mcp")
            .arg(&artifacts.mcp)
            .arg("--web-dist")
            .arg(web);
        execute(register, &work.join("registration.log"), 180, None)?;
    } else {
        Installation::initialize(&home, None, None)?;
    }
    let installation = Installation::load(&home)?;
    report["installation_id"] = json!(installation.id);
    report["setup"] = json!("running");
    save(work, report)?;
    ensure!(
        !cancelled.load(Ordering::SeqCst),
        "acceptance run interrupted before setup"
    );
    if names.iter().any(|name| name == "onboarding") {
        for attempt in 0..2 {
            let mut prepare = cli(artifacts, &home);
            prepare.args(["setup", "--prepare-only", "--allow-development"]);
            execute(
                prepare,
                &work.join(format!("prepare-{attempt}.log")),
                600,
                None,
            )?;
            ensure!(
                !home.join("runtime-owner.json").exists()
                    && !home.join("proofstorm.sqlite3").exists(),
                "prepare-only started runtime/state"
            );
            let tools = tool_snapshot(&home)?;
            if attempt == 0 {
                private_json(&work.join("prepared-tools.json"), &tools)?;
            } else {
                ensure!(
                    tools
                        == serde_json::from_slice::<Value>(&fs::read(
                            work.join("prepared-tools.json")
                        )?)?,
                    "prepare-only replaced verified tools"
                );
            }
        }
    }
    let mut setup = cli(artifacts, &home);
    setup.arg("setup");
    // This gate applies raw CRDs rather than going through MCP image preparation.
    if names.iter().any(|name| name == "slice2") {
        setup.arg("--prefetch-all");
    }
    if allow_development || artifacts.checkout.is_some() {
        setup.arg("--allow-development");
    }
    eprintln!("Setting up an isolated runtime (the checkout runtime is not used)...");
    // Let setup's bounded operation finish on Ctrl-C so it can record resources.
    execute(setup, &work.join("setup.log"), 5400, None)?;
    report["setup"] = json!("passed");
    save(work, report)?;
    Ok(())
}

fn tool_snapshot(home: &Path) -> Result<Value> {
    let mut tools = std::collections::BTreeMap::new();
    for entry in fs::read_dir(home.join("tools"))? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(metadata.is_file(), "non-file helper");
        tools.insert(
            path.file_name().unwrap().to_string_lossy().into_owned(),
            json!([
                gates::onboarding::hash(&path)?,
                metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
                    .to_string()
            ]),
        );
    }
    Ok(json!(tools))
}

pub fn run(
    selection: &Selection,
    root: &Path,
    destination: Option<&Path>,
    names: &[String],
    timeout: u64,
    cancelled: &AtomicBool,
) -> Result<()> {
    validate_gates(names)?;
    let started = Instant::now();
    ensure!(
        selection.qualification.is_some() == (names == ["qualification"]),
        "qualification requires one exact planned case"
    );
    let qualification = if let Some((path, id)) = &selection.qualification {
        let plan: proofstorm_qualification::Plan = serde_json::from_slice(&fs::read(path)?)?;
        plan.validate()?;
        let case = plan.case(id)?.clone();
        ensure!(case.required, "qualification case was not scheduled");
        crate::qualification::require_native(&case.platform)?;
        Some((plan, case))
    } else {
        None
    };
    let artifacts = selection.artifacts()?; // Verify before creating state or contacting Docker.
    let root = root.canonicalize()?;
    let work = if let Some(destination) = destination {
        ensure!(
            destination.is_absolute(),
            "--work-dir must be an absolute new directory"
        );
        fs::DirBuilder::new()
            .mode(0o700)
            .create(destination)
            .context("work directory must not already exist")?;
        destination.canonicalize()?
    } else {
        tempfile::Builder::new()
            .prefix("proofstorm-acceptance-")
            .tempdir()?
            .keep()
            .canonicalize()?
    };
    let home = work.join("state");
    eprintln!("Acceptance run: {}", work.display());
    eprintln!("Host resources: {}", crate::diagnostics::resources(&work));
    let mut report =
        json!({"format_version":1,"work":work,"setup":"not_run","gates":[],"cleanup":"not_run"});
    save(&work, &report)?;
    // The new installation has not been initialized yet, so none of these
    // preexisting resources can belong to this run. Never adopt existing state.
    let mut samples = Vec::new();
    let (before, exclusions) =
        crate::preservation::baseline(selection.checkout_home.as_deref(), |index, value| {
            let name = format!("preservation-before-{index:02}.json");
            private_json(&work.join(&name), value)?;
            samples.push(name);
            Ok(())
        })?;
    private_json(&work.join("preservation-before.json"), &before)?;
    report["preservation_baseline_samples"] = json!(samples);
    report["preservation_exclusions"] = exclusions.clone();
    report["preservation_config_scope"] = json!({"claude":"top-level and project mcpServers; normalized JSON","other_configuration":"whole-file sha256"});
    save(&work, &report)?;
    let operation = (|| -> Result<()> {
        if let Some((plan, case)) = &qualification {
            private_json(
                &work.join("qualification-plan.json"),
                &serde_json::to_value(plan)?,
            )?;
            private_json(
                &work.join("qualification-case.json"),
                &serde_json::to_value(case)?,
            )?;
            let mut check = Command::new("bash");
            check
                .arg(root.join("scripts/qualification-images.sh"))
                .arg(work.join("qualification-case.json"))
                .arg(&work);
            execute(
                check,
                &work.join("image-qualification.log"),
                1200,
                Some(cancelled),
            )?;
            if matches!(
                case.scenario,
                proofstorm_qualification::Scenario::Image { .. }
                    | proofstorm_qualification::Scenario::Lightning { .. }
            ) {
                report["setup"] = json!("not_required");
                report["gates"] = json!([{"name":"qualification","status":"passed"}]);
                return Ok(());
            }
        }
        start_runtime(
            &artifacts,
            &work,
            names,
            selection.allow_development,
            cancelled,
            &mut report,
        )?;
        if names.iter().any(|name| name == "installation-isolation") {
            let peer = work.join("peer");
            fs::DirBuilder::new().mode(0o700).create(&peer)?;
            let mut peer_report = json!({"format_version":1,"work":peer,"setup":"not_run","gates":[],"cleanup":"not_run"});
            peer_report["parent_installation_id"] = report["installation_id"].clone();
            save(&peer, &peer_report)?;
            let result = start_runtime(
                &artifacts,
                &peer,
                &[],
                selection.allow_development,
                cancelled,
                &mut peer_report,
            );
            if let Err(error) = &result {
                peer_report["setup"] = json!("failed");
                peer_report["error"] = json!(format!("{error:#}"));
                save(&peer, &peer_report)?;
            }
            result?;
        }
        for (index, name) in names.iter().enumerate() {
            ensure!(
                !cancelled.load(Ordering::SeqCst),
                "acceptance run interrupted"
            );
            eprintln!("Running gate {name}...");
            report["gates"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":name,"status":"running"}));
            save(&work, &report)?;
            let mut worker = command(&std::env::current_exe()?, &home);
            // Capture failing assertion locations even when the caller disabled
            // backtraces. The public summary excludes error text and arguments.
            worker.env("RUST_LIB_BACKTRACE", "1");
            let failure_path = work.join("gate-failure.json");
            for path in [&failure_path, &work.join("qualification-stage.json")] {
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
            selection.arguments(&mut worker);
            worker
                .arg("--root")
                .arg(&root)
                .arg("--worker-home")
                .arg(&home)
                .arg(name);
            let result = execute(
                worker,
                &work.join(format!("gate-{index}-{name}.log")),
                timeout,
                Some(cancelled),
            );
            if result.is_err()
                && let Ok(bytes) = fs::read(&failure_path)
                && let Ok(failure) = serde_json::from_slice::<Value>(&bytes)
            {
                eprintln!("Gate failure: {failure}");
                report["gates"][index]["failure"] = failure;
            }
            report["gates"][index]["status"] =
                json!(if result.is_ok() { "passed" } else { "failed" });
            save(&work, &report)?;
            result?;
        }
        Ok(())
    })();
    if let Err(error) = &operation {
        if report["setup"] == "running" {
            report["setup"] = json!("failed");
        }
        report["error"] = json!(format!("{error:#}"));
    }
    save(&work, &report)?;
    let cleanup_result = if home.join("runtime-resources.json").exists() {
        eprintln!("Removing only this run's recorded runtime and storage...");
        cleanup(&work)
    } else if qualification.as_ref().is_some_and(|(_, case)| {
        matches!(
            case.scenario,
            proofstorm_qualification::Scenario::Image { .. }
                | proofstorm_qualification::Scenario::Lightning { .. }
        )
    }) && operation.is_ok()
    {
        report["cleanup"] = json!("passed");
        save(&work, &report)
    } else {
        // No deletion authority: never discover/adopt a partly-created cluster here.
        report["cleanup"] = json!("not_run");
        report["cleanup_note"] = json!(
            "No resource receipt was created. No Docker deletion attempted; inspect setup evidence if creation was interrupted."
        );
        save(&work, &report)
    };
    let preservation = (|| -> Result<()> {
        let after = crate::preservation::snapshot(selection.checkout_home.as_deref())?;
        private_json(&work.join("preservation-after.json"), &after)?;
        crate::preservation::verify_with_exclusions(&before, &after, &exclusions)
    })();
    let mut report: Value = serde_json::from_slice(&fs::read(work.join("acceptance.json"))?)?;
    report["preservation"] = json!(if preservation.is_ok() {
        "passed"
    } else {
        "failed"
    });
    if let Err(error) = &preservation {
        report["preservation_error"] = json!(format!("{error:#}"));
    }
    save(&work, &report)?;
    if let Some((plan, case)) = &qualification {
        let images = fs::read(work.join("images.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let receipt = proofstorm_qualification::Receipt {
            format_version: 1,
            identity: plan.identity.clone(),
            plan_digest: plan.digest(),
            case_id: case.id.clone(),
            platform: case.platform.clone(),
            components: case.components.clone(),
            claims: case.claims.clone(),
            images,
            passed: operation.is_ok(),
            cleanup_verified: cleanup_result.is_ok() && report["cleanup"] == "passed",
            preservation_verified: preservation.is_ok(),
            stage: crate::diagnostics::qualification_stage(
                &work,
                &report,
                operation.is_ok(),
                cleanup_result.is_ok() && report["cleanup"] == "passed",
                preservation.is_ok(),
            ),
            elapsed_seconds: started.elapsed().as_secs(),
        };
        eprintln!(
            "Qualification {}: stage={}, passed={}, cleanup={}, preservation={}",
            case.id,
            receipt.stage,
            receipt.passed,
            receipt.cleanup_verified,
            receipt.preservation_verified
        );
        private_json(
            &work.join("qualification-receipt.json"),
            &serde_json::to_value(receipt)?,
        )?;
    }
    eprintln!("Report: {}", work.join("acceptance.json").display());
    if let Err(error) = cleanup_result {
        eprintln!(
            "Cleanup needs attention. Retry: proofstorm-acceptance --cleanup {}",
            work.display()
        );
        return Err(error);
    }
    preservation?;
    operation
}

fn private_json(path: &Path, value: &Value) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_artifact_source_and_unknown_gates_fail_before_work_is_created() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("must-not-exist");
        let selection = Selection {
            benchmark_model: None,
            benchmark_opencode: "opencode".into(),
            qualification: None,
            checkout_home: None,
            bundle: None,
            allow_development: false,
        };
        assert!(
            run(
                &selection,
                root.path(),
                Some(&work),
                &["smoke".into()],
                1,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(!work.exists());
        assert!(validate_gates(&["typo".into()]).is_err());
        assert!(validate_gates(&["smoke".into(), "smoke".into()]).is_err());
        assert!(validate_gates(&["gui".into(), "onboarding".into()]).is_err());
        assert!(validate_gates(&["onboarding".into(), "slice2".into()]).is_err());
        validate_gates(&["onboarding".into(), "agent-config".into(), "gui".into()]).unwrap();
    }
    #[test]
    fn worker_cannot_adopt_an_existing_installation_without_run_identity() {
        let root = tempfile::tempdir().unwrap();
        Installation::initialize(&root.path().join("state"), None, None).unwrap();
        assert!(read(root.path()).is_err());
        save(root.path(), &json!({"format_version":1,"work":root.path().canonicalize().unwrap(),"installation_id":"foreign"})).unwrap();
        assert!(read(root.path()).is_err());
    }

    #[test]
    fn peers_require_their_parent_receipt_and_cannot_be_links() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let work = root.path().canonicalize().unwrap();
        let parent = Installation::initialize(&work.join("state"), None, None).unwrap();
        save(
            &work,
            &json!({"format_version":1,"work":work,"installation_id":parent.id}),
        )
        .unwrap();
        let peer = work.join("peer");
        fs::create_dir(&peer).unwrap();
        let identity = Installation::initialize(&peer.join("state"), None, None).unwrap();
        let mut report = json!({"format_version":1,"work":peer,"installation_id":identity.id,"parent_installation_id":"foreign"});
        save(&peer, &report).unwrap();
        assert!(read_peer(&work).is_err());
        report["parent_installation_id"] = json!(parent.id);
        save(&peer, &report).unwrap();
        read_peer(&work).unwrap();
        fs::rename(&peer, work.join("other-run")).unwrap();
        symlink(work.join("other-run"), &peer).unwrap();
        assert!(read_peer(&work).is_err());
    }

    #[test]
    fn failed_and_cancelled_children_keep_evidence_and_return_failure() {
        let root = tempfile::tempdir().unwrap();
        let mut failed = Command::new("/bin/sh");
        failed.args(["-c", "printf 'test failure' >&2; exit 7"]);
        let log = root.path().join("failed.log");
        assert!(execute(failed, &log, 5, None).is_err());
        assert_eq!(fs::read_to_string(log).unwrap(), "test failure");
        let mut sleeping = Command::new("/bin/sleep");
        sleeping.arg("30");
        let started = Instant::now();
        assert!(
            execute(
                sleeping,
                &root.path().join("cancelled.log"),
                5,
                Some(&AtomicBool::new(true))
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn worker_cleanup_stops_descendants_on_exit_failure_timeout_and_cancellation() {
        struct Sentinel(Child);
        impl Drop for Sentinel {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct Descendant(Pid);
        impl Drop for Descendant {
            fn drop(&mut self) {
                let _ = nix::sys::signal::kill(self.0, Signal::SIGKILL);
            }
        }
        let mut unrelated = Sentinel(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        for mode in ["success", "failure", "timeout", "cancel"] {
            let root = tempfile::tempdir().unwrap();
            let pid_file = root.path().join("descendant.pid");
            let cancelled = AtomicBool::new(false);
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    r#"
sleep 60 &
printf '%s' "$!" > "$1"
case "$2" in
  success) exit 0 ;;
  failure) exit 7 ;;
  *) wait ;;
esac
"#,
                    "worker-fixture",
                ])
                .arg(&pid_file)
                .arg(mode);
            let result = std::thread::scope(|scope| {
                if mode == "cancel" {
                    scope.spawn(|| {
                        let started = Instant::now();
                        while fs::read_to_string(&pid_file)
                            .ok()
                            .and_then(|value| value.parse::<u32>().ok())
                            .is_none()
                            && started.elapsed() < Duration::from_secs(5)
                        {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        cancelled.store(true, Ordering::SeqCst);
                    });
                }
                execute(
                    command,
                    &root.path().join("worker.log"),
                    1,
                    Some(&cancelled),
                )
            });
            let descendant = Descendant(Pid::from_raw(
                fs::read_to_string(&pid_file).unwrap().parse().unwrap(),
            ));
            assert_eq!(result.is_ok(), mode == "success", "{mode}: {result:?}");
            assert!(
                unrelated.0.try_wait().unwrap().is_none(),
                "signalled an unrelated process"
            );
            let started = Instant::now();
            loop {
                let output = Command::new("ps")
                    .args(["-o", "stat=", "-p", &descendant.0.to_string()])
                    .output()
                    .unwrap();
                let state = String::from_utf8(output.stdout).unwrap();
                // An orphan can briefly remain as a zombie until init reaps it.
                if state.trim().is_empty() || state.trim().starts_with('Z') {
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "{mode}: worker descendant survived cleanup"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn dropping_a_worker_after_an_inspection_error_reaps_it() {
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        let worker = Worker::spawn(command).unwrap();
        let pid = Pid::from_raw(i32::try_from(worker.child.id()).unwrap());
        drop(worker);
        assert_eq!(nix::sys::signal::kill(pid, None), Err(Errno::ESRCH));
    }
}
