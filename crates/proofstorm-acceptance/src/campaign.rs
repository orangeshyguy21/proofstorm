//! Local sequential campaigns. Model execution is opt-in; tests use fake runners.
mod records;
#[cfg(test)]
mod tests;

use crate::benchmark::{read, save, task};
use anyhow::{Context, Result, ensure};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    format_version: u32,
    root: PathBuf,
    work: PathBuf,
    runner: PathBuf,
    runner_sha256: String,
    revision: String,
    checkout_home: PathBuf,
    gate_timeout_seconds: u64,
    max_setup_attempts: u32,
    runs: Vec<Run>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Run {
    id: String,
    model: String,
    harness: String,
    executable: PathBuf,
    executable_sha256: String,
    task: String,
    task_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    run: usize,
    setup_attempt: u32,
    finished: bool,
    exit_code: Option<i32>,
    process_error: Option<String>,
    receipts: Option<Value>,
}

pub fn contracts() -> Value {
    json!({"benchmark-o1": contract(task::o1()), "benchmark-o5": contract(task::o5())})
}

fn contract(task: &task::Task) -> Value {
    json!({"id":task.id,"version":task.version,"sha256":proofstorm_core::digest_json(task),"model_wall_seconds":task.deadline_seconds})
}

fn hash(path: &Path) -> Result<String> {
    use std::io::Read;
    ensure!(
        fs::metadata(path)?.is_file(),
        "cannot hash a non-file: {}",
        path.display()
    );
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = vec![0; 65536];
    loop {
        let size = file.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        digest.update(&bytes[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn regular_read(path: &Path) -> Result<Value> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "record is not a regular file: {}",
        path.display()
    );
    read(path)
}

impl Plan {
    fn validate(&self) -> Result<()> {
        ensure!(self.format_version == 1, "unsupported campaign format");
        ensure!(
            (1..=3).contains(&self.max_setup_attempts),
            "setup attempts must be 1..=3"
        );
        ensure!(
            (1200..=14400).contains(&self.gate_timeout_seconds),
            "gate timeout must be 1200..=14400 seconds"
        );
        ensure!(!self.runs.is_empty(), "campaign has no runs");
        for path in [&self.root, &self.work, &self.runner, &self.checkout_home] {
            ensure!(path.is_absolute(), "campaign paths must be absolute");
        }
        let contracts = contracts();
        let mut ids = BTreeSet::new();
        for run in &self.runs {
            ensure!(
                !run.id.is_empty()
                    && run.id.len() <= 64
                    && run
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "invalid run ID"
            );
            ensure!(ids.insert(&run.id), "duplicate run ID");
            ensure!(
                !run.model.is_empty()
                    && !run.model.starts_with('-')
                    && !run.model.chars().any(char::is_control),
                "invalid model ID"
            );
            ensure!(
                matches!(run.harness.as_str(), "codex" | "claude-code" | "opencode"),
                "unsupported CLI"
            );
            ensure!(run.executable.is_absolute(), "CLI path must be absolute");
            ensure!(
                contracts[&run.task]["sha256"].as_str() == Some(&run.task_sha256),
                "task contract changed or unknown task: {}",
                run.task
            );
        }
        Ok(())
    }

    fn verify_source(&self) -> Result<()> {
        ensure!(
            hash(&self.runner)? == self.runner_sha256,
            "acceptance runner changed"
        );
        for run in &self.runs {
            ensure!(
                hash(&run.executable)? == run.executable_sha256,
                "CLI changed for {}",
                run.id
            );
        }
        for (args, expected) in [
            (["rev-parse", "HEAD"], self.revision.as_str()),
            (["status", "--porcelain"], ""),
        ] {
            let mut command = Command::new("git");
            command.current_dir(&self.root).args(args);
            let output = crate::process::capture(command, 30)?;
            ensure!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).trim() == expected,
                "campaign requires the pinned clean source revision"
            );
        }
        Ok(())
    }

    fn attempt_path(&self, entry: &Entry) -> PathBuf {
        self.work.join(format!(
            "{}-setup-{:03}",
            self.runs[entry.run].id, entry.setup_attempt
        ))
    }
}

/// Start from a frozen plan or explicitly continue after inspecting a stop.
/// A model launch is never retried, even when its result is missing.
pub fn run(path: &Path, resume: bool, cancelled: &AtomicBool) -> Result<()> {
    let plan: Plan = serde_json::from_value(regular_read(path)?)?;
    plan.validate()?;
    plan.verify_source()?;
    if resume {
        ensure!(
            fs::symlink_metadata(&plan.work)?.is_dir(),
            "campaign directory must not be linked"
        );
    } else {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&plan.work)
            .context("campaign directory must be new")?;
    }
    let lock_path = plan.work.join("campaign.lock");
    if lock_path.exists() {
        ensure!(
            fs::symlink_metadata(&lock_path)?.is_file(),
            "linked campaign lock"
        );
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    let _lock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, error)| anyhow::anyhow!("campaign already active: {error}"))?;
    let driver = json!({"sha256":hash(&std::env::current_exe()?)?});
    if resume {
        ensure!(
            regular_read(&plan.work.join("plan.json"))? == json!(plan),
            "frozen plan changed"
        );
        ensure!(
            regular_read(&plan.work.join("driver.json"))? == driver,
            "campaign driver changed"
        );
    } else {
        save(&plan.work.join("plan.json"), &json!(plan))?;
        save(&plan.work.join("driver.json"), &driver)?;
        save(&plan.work.join("journal.json"), &json!([]))?;
    }
    let result = drive(
        &plan,
        cancelled,
        || plan.verify_source(),
        |entry| execute(&plan, entry, cancelled),
    );
    if let Err(error) = &result {
        let mut progress = regular_read(&plan.work.join("progress.json")).unwrap_or(json!({}));
        progress["state"] = json!("stopped");
        progress["error"] = json!(format!("{error:#}"));
        save(&plan.work.join("progress.json"), &progress)?;
    }
    result
}

fn execute(plan: &Plan, entry: &Entry, cancelled: &AtomicBool) -> Result<Option<i32>> {
    let run = &plan.runs[entry.run];
    let work = plan.attempt_path(entry);
    ensure!(
        !work.try_exists()?,
        "attempt directory already exists; refusing to reuse it"
    );
    let log = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(work.with_extension("log"))?;
    let option = match run.harness.as_str() {
        "codex" => "--benchmark-codex",
        "claude-code" => "--benchmark-claude",
        _ => "--benchmark-opencode",
    };
    let mut command = Command::new(&plan.runner);
    crate::client::clear_runtime_environment(&mut command);
    let mut child = command
        .current_dir(&plan.root)
        .arg("--checkout-home")
        .arg(&plan.checkout_home)
        .arg("--root")
        .arg(&plan.root)
        .arg("--work-dir")
        .arg(&work)
        .arg("--timeout")
        .arg(plan.gate_timeout_seconds.to_string())
        .args(["--benchmark-harness", &run.harness, option])
        .arg(&run.executable)
        .args(["--benchmark-model", &run.model, &run.task])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let mut signalled = false;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if cancelled.load(Ordering::SeqCst) && !signalled {
            // Acceptance owns bounded process-tree cancellation and teardown.
            let pid = i32::try_from(child.id()).context("child PID")?;
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGINT,
            );
            signalled = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn drive(
    plan: &Plan,
    cancelled: &AtomicBool,
    mut verify: impl FnMut() -> Result<()>,
    mut launch: impl FnMut(&Entry) -> Result<Option<i32>>,
) -> Result<()> {
    let mut journal: Vec<Entry> =
        serde_json::from_value(regular_read(&plan.work.join("journal.json"))?)?;
    let mut rows = Vec::new();
    let mut run_index = 0;
    let mut setup_attempt = 1;
    for entry in &journal {
        ensure!(
            entry.run == run_index
                && entry.setup_attempt == setup_attempt
                && run_index < plan.runs.len(),
            "campaign journal order is invalid"
        );
        let row = records::inspect(plan, entry);
        let classification = row["classification"].as_str().unwrap();
        rows.push(row.clone());
        records::report(plan, &rows)?;
        match classification {
            "model_result" => {
                run_index += 1;
                setup_attempt = 1;
            }
            "setup_failure" if row["safe_to_retry_setup"] == true => {
                setup_attempt += 1;
            }
            _ => anyhow::bail!(
                "{} needs inspection; no model retry or next run",
                plan.runs[run_index].id
            ),
        }
    }
    while run_index < plan.runs.len() {
        ensure!(
            !cancelled.load(Ordering::SeqCst),
            "campaign cancelled before next run"
        );
        ensure!(
            setup_attempt <= plan.max_setup_attempts,
            "setup attempt limit reached; campaign stopped"
        );
        verify()?;
        journal.push(Entry {
            run: run_index,
            setup_attempt,
            finished: false,
            exit_code: None,
            process_error: None,
            receipts: None,
        });
        save(&plan.work.join("journal.json"), &json!(journal))?;
        save(
            &plan.work.join("progress.json"),
            &json!({"state":"running","run":plan.runs[run_index].id,"setup_attempt":setup_attempt}),
        )?;
        let entry = journal.last_mut().unwrap();
        let result = launch(entry);
        entry.finished = true;
        entry.exit_code = result.as_ref().ok().copied().flatten();
        entry.process_error = result.as_ref().err().map(|error| format!("{error:#}"));
        save(&plan.work.join("journal.json"), &json!(journal))?;
        let row = records::inspect(plan, journal.last().unwrap());
        journal.last_mut().unwrap().receipts = Some(row["receipt_sha256"].clone());
        save(&plan.work.join("journal.json"), &json!(journal))?;
        rows.push(row.clone());
        records::report(plan, &rows)?;
        let may_continue = row["classification"] == "model_result"
            && result.is_ok()
            && !cancelled.load(Ordering::SeqCst);
        if !may_continue {
            save(
                &plan.work.join("progress.json"),
                &json!({"state":"stopped","run":plan.runs[run_index].id,"classification":row["classification"],"cancelled":cancelled.load(Ordering::SeqCst)}),
            )?;
            anyhow::bail!(
                "campaign stopped; inspect retained evidence before --resume (models are never retried)"
            );
        }
        run_index += 1;
        setup_attempt = 1;
    }
    save(
        &plan.work.join("progress.json"),
        &json!({"state":"completed","runs":plan.runs.len()}),
    )?;
    Ok(())
}
