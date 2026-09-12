//! Disposable live-test lifecycle. Setup uses the verified product CLI;
//! teardown uses the shared installation resource-receipt implementation.
use crate::{GateContext, client::clear_runtime_environment, gates};
use anyhow::{Context, Result, ensure};
use proofstorm_app::{artifacts::TestArtifacts, installation::Installation};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub struct Selection {
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
    let context = GateContext::new(root, installation, selection.artifacts()?)?;
    gates::run(name, &context)
}

fn command(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    clear_runtime_environment(&mut command);
    command.env("KUBECONFIG", home.join("kubeconfig"));
    command.stdin(Stdio::null());
    command
}

fn execute(
    mut command: Command,
    log: &Path,
    seconds: u64,
    cancelled: Option<&AtomicBool>,
) -> Result<()> {
    use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(log)?;
    command.stdout(output.try_clone()?).stderr(output);
    // Workers and their MCP/port-forward children share a fresh process group.
    command.process_group(0);
    let mut child = command.spawn().context("start acceptance operation")?;
    let start = Instant::now();
    let mut last_progress = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            // A failed/finished worker must not leave port-forwards or helper children.
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &format!("-{}", child.id())])
                .stderr(Stdio::null())
                .status();
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
            // Exact child process-group ID, not a name search or shared shell group.
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &format!("-{}", child.id())])
                .status();
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "acceptance operation interrupted or timed out; see {}",
                log.display()
            );
        }
        if last_progress.elapsed() >= Duration::from_secs(30) {
            eprintln!("Still running; progress log: {}", log.display());
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
    let mut report =
        json!({"format_version":1,"work":work,"setup":"not_run","gates":[],"cleanup":"not_run"});
    save(&work, &report)?;
    let before = crate::preservation::snapshot(selection.checkout_home.as_deref())?;
    private_json(&work.join("preservation-before.json"), &before)?;
    let operation = (|| -> Result<()> {
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
        crate::preservation::verify(&before, &after)
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
}
