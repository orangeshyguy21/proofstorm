//! Split a plan into CI shards and run one shard's cases.
//!
//! A failed case is retried once in a fresh work directory. A case that an
//! earlier attempt of the same run already passed keeps its receipt, so
//! re-running a failed shard repeats only its failed cases. Only redacted
//! receipts leave the runner; failed attempts' private logs are encrypted to
//! the maintainers' age key when one is configured.
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{Case, Plan, Receipt, Scenario, receipt};

/// Shards per architecture. Main also runs two bundle jobs beside these, and
/// GitHub's free plan runs 20 jobs at once, so 9 + 9 + 2 never wait for a slot.
const SHARDS: usize = 9;

/// A runtime case starts its own cluster and takes minutes; an image or
/// Lightning check takes seconds.
fn weight(case: &Case) -> u32 {
    match case.scenario {
        Scenario::Image { .. } | Scenario::Lightning { .. } => 1,
        _ => 40,
    }
}

/// The GitHub Actions matrix for a plan. Heaviest cases first, each goes to
/// the shard with the least weight so far, so shards finish close together.
/// Within a shard, cases run in plan order.
#[must_use]
pub fn matrix(plan: &Plan) -> Value {
    let mut entries = Vec::new();
    for (platform, runner) in [
        ("linux/amd64", "ubuntu-24.04"),
        ("linux/arm64", "ubuntu-24.04-arm"),
    ] {
        let mut cases: Vec<_> = plan
            .cases
            .iter()
            .enumerate()
            .filter(|(_, case)| case.required && case.platform == platform)
            .collect();
        cases.sort_by_key(|(index, case)| (std::cmp::Reverse(weight(case)), *index));
        let mut shards = vec![(0, Vec::new()); SHARDS.min(cases.len())];
        for (index, case) in cases {
            if let Some(lightest) = shards.iter_mut().min_by_key(|(load, _)| *load) {
                lightest.0 += weight(case);
                lightest.1.push(index);
            }
        }
        let arch = platform.trim_start_matches("linux/");
        for (number, (_, mut indices)) in shards.into_iter().enumerate() {
            indices.sort_unstable();
            let ids: Vec<_> = indices.iter().map(|&i| plan.cases[i].id.as_str()).collect();
            entries.push(json!({"platform":platform,"arch":arch,"runner":runner,"shard":format!("{arch}-{number}"),"cases":ids}));
        }
    }
    json!({ "include": entries })
}

/// Run every case of one shard from the checkout in the current directory.
/// `previous` holds this shard's receipts from an earlier attempt, if any.
/// Returns whether every case passed.
///
/// # Errors
/// Rejects an invalid plan or shard, and fails when a case cannot be started.
pub fn run_shard(
    plan_path: &Path,
    plan: &Plan,
    ids: &[String],
    work: &Path,
    receipts: &Path,
    previous: Option<&Path>,
) -> Result<bool> {
    plan.validate()?;
    ensure!(!ids.is_empty(), "empty qualification shard");
    for id in ids {
        ensure!(
            plan.case(id)?.required,
            "unscheduled qualification case {id}"
        );
    }
    let root = std::env::current_dir()?.canonicalize()?;
    let acceptance = root.join("target/check/debug/proofstorm-acceptance");
    fs::create_dir_all(work)?;
    fs::create_dir_all(receipts)?;
    let earlier = earlier_passes(plan, ids, previous)?;
    let (mut retried, mut failed) = (Vec::new(), Vec::new());
    for id in ids {
        let output = receipts.join(format!("{id}.json"));
        if let Some(bytes) = earlier.get(id) {
            fs::write(&output, bytes)?;
            println!("{id} passed in an earlier attempt of this run; kept its receipt");
            continue;
        }
        let started = Instant::now();
        let mut dir = work.join(id);
        let mut outcome = attempt(&root, &acceptance, plan_path, id, &dir)?;
        if let Some(first) = outcome {
            println!("::warning::Qualification {id} attempt 1: {first}; retrying once");
            seal(id, &dir, 1);
            dir = work.join(format!("{id}-retry"));
            outcome = attempt(&root, &acceptance, plan_path, id, &dir)?;
            if outcome.is_some() {
                seal(id, &dir, 2);
            } else {
                retried.push(format!("{id}: {first}"));
            }
        }
        let receipt = dir.join("qualification-receipt.json");
        if receipt.is_file() && fs::copy(&receipt, &output).is_err() {
            outcome = Some(outcome.map_or("receipt copy failed".into(), |reason| {
                format!("{reason}; receipt copy failed")
            }));
        }
        if let Some(reason) = outcome {
            println!("::error::Qualification {id}: {reason}");
            failed.push(format!("{id}: {reason}"));
        }
        println!("{id} completed in {} seconds", started.elapsed().as_secs());
    }
    report(ids.len(), earlier.len(), &retried, &failed)?;
    Ok(failed.is_empty())
}

/// Receipts an earlier attempt of this run uploaded for this shard that still
/// verify as passes of their exact case. Anything else is run again.
fn earlier_passes(
    plan: &Plan,
    ids: &[String],
    previous: Option<&Path>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut passes = BTreeMap::new();
    let Some(directory) = previous.filter(|directory| directory.is_dir()) else {
        return Ok(passes);
    };
    let digest = plan.digest();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() <= 1 << 20) {
            continue;
        }
        let bytes = fs::read(&path)?;
        let Ok(receipt) = serde_json::from_slice::<Receipt>(&bytes) else {
            continue;
        };
        if ids.contains(&receipt.case_id) && receipt::check(plan, &digest, &receipt).is_ok() {
            passes.insert(receipt.case_id, bytes);
        }
    }
    Ok(passes)
}

/// Run one attempt in a new work directory. `None` means it passed; otherwise
/// the public reason, built only from fixed-format fields.
fn attempt(
    root: &Path,
    acceptance: &Path,
    plan: &Path,
    id: &str,
    dir: &Path,
) -> Result<Option<String>> {
    let mut command = Command::new(acceptance);
    command
        .arg("--root")
        .arg(root)
        .arg("--checkout-home")
        .arg(root.join(".proofstorm-dev/state"))
        .arg("--work-dir")
        .arg(dir)
        .arg("--qualification-plan")
        .arg(plan)
        .arg("--qualification-case")
        .arg(id)
        .args(["--timeout", "1200"]);
    if let Some(cache) = std::env::var_os("QUALIFICATION_IMAGE_CACHE").filter(|v| !v.is_empty()) {
        command.arg("--qualification-image-cache").arg(cache);
    }
    let status = command
        .arg("qualification")
        .status()
        .with_context(|| format!("start {}", acceptance.display()))?;
    let mut reasons = Vec::new();
    if !status.success() {
        reasons.push(
            status
                .code()
                .map_or("acceptance was killed".into(), |code| {
                    format!("acceptance exited {code}")
                }),
        );
        if let Some(failure) = read_json(&dir.join("gate-failure.json")) {
            reasons.extend(failure_detail(&failure));
        }
    }
    let receipt = dir.join("qualification-receipt.json");
    if !receipt.is_file() {
        reasons.push("receipt missing".into());
    } else if !reasons.is_empty()
        && let Some(stage) = read_json(&receipt).and_then(|r| slug(&r["stage"]).map(str::to_owned))
    {
        reasons.push(format!("stage={stage}"));
    }
    Ok((!reasons.is_empty()).then(|| reasons.join("; ")))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// The public part of `gate-failure.json`: its category, the failing source
/// location and typed status fields. Error text and native output stay private.
fn failure_detail(failure: &Value) -> Vec<String> {
    let reason = failure.get("reason").filter(|value| !value.is_null());
    let reason = reason.unwrap_or(&failure["native"]["reason"]);
    let mut parts = vec![slug(reason).unwrap_or("gate-failed").to_owned()];
    let locations: Vec<_> = failure["locations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|location| source_location(location))
        .collect();
    let location = locations
        .iter()
        .find(|location| location.contains("/gates/") || location.contains("/qualification/"))
        .or(locations.first());
    if let Some(location) = location {
        parts.push(format!("at {location}"));
    }
    let operation = &failure["operation"];
    if let Some(termination) = operation["termination_reason"].as_str().filter(|text| {
        (1..=32).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphabetic())
    }) {
        parts.push(termination.to_owned());
    }
    if let Some(exit) = operation["exit_code"].as_u64().filter(|code| *code <= 255) {
        parts.push(format!("exit {exit}"));
    }
    let tool = &failure["tool"];
    for (label, value) in [("tool", &tool["tool"]), ("code", &tool["code"])] {
        if let Some(name) = value.as_str().filter(|text| snake_case(text)) {
            parts.push(format!("{label} {name}"));
        }
    }
    if let Some(status) = tool["http_status"]
        .as_u64()
        .filter(|status| (100..=599).contains(status))
    {
        parts.push(format!("http {status}"));
    }
    parts
}

/// A fixed category or stage name: lowercase words joined by hyphens.
fn slug(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        text.len() <= 64
            && text.starts_with(|c: char| c.is_ascii_lowercase())
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// A tool name or problem code assigned in source, such as `cell_remove`.
fn snake_case(text: &str) -> bool {
    text.len() <= 64
        && text.contains('_')
        && text
            .split('_')
            .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()))
}

/// `crates/proofstorm-acceptance/src/PATH.rs:LINE[:COLUMN]`, nothing else.
fn source_location(text: &str) -> bool {
    let Some((path, position)) = text
        .strip_prefix("crates/proofstorm-acceptance/src/")
        .and_then(|rest| rest.split_once(".rs:"))
    else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mut numbers = position.split(':');
    let line = numbers.next().unwrap_or_default();
    path.split('/').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) && digits(line)
        && !line.starts_with('0')
        && numbers.next().is_none_or(digits)
        && numbers.next().is_none()
}

/// Encrypt one failed attempt's logs and JSON records. Plaintext never leaves
/// the runner; without a recipient this does nothing.
fn seal(id: &str, dir: &Path, attempt: u8) {
    let variable = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let (Some(recipient), Some(output)) = (
        variable("QUALIFICATION_LOG_RECIPIENT"),
        variable("QUALIFICATION_LOG_DIR"),
    ) else {
        return;
    };
    if !dir.is_dir() {
        return;
    }
    let bundle = PathBuf::from(output).join(format!("{id}-attempt-{attempt}.tar.gz.age"));
    if encrypt(dir, &recipient, &bundle).is_ok() {
        println!("Encrypted private logs: {id}-attempt-{attempt}.tar.gz.age");
    } else {
        let _ = fs::remove_file(&bundle);
        println!("::notice::could not encrypt private logs for {id}");
    }
}

fn encrypt(dir: &Path, recipient: &std::ffi::OsStr, bundle: &Path) -> Result<()> {
    let mut files = Vec::new();
    collect(dir, Path::new(""), &mut files)?;
    fs::create_dir_all(bundle.parent().context("bundle directory")?)?;
    let mut tar = Command::new("tar")
        .current_dir(dir)
        .args(["--null", "-czf", "-", "-T", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let age = Command::new("age")
        .arg("-r")
        .arg(recipient)
        .arg("-o")
        .arg(bundle)
        .stdin(tar.stdout.take().context("tar output")?)
        .spawn();
    let mut age = match age {
        Ok(age) => age,
        Err(error) => {
            let _ = tar.kill();
            let _ = tar.wait();
            return Err(error.into());
        }
    };
    let mut names = tar.stdin.take().context("tar input")?;
    for file in &files {
        names.write_all(file.as_os_str().as_bytes())?;
        names.write_all(b"\0")?;
    }
    drop(names);
    let archived = tar.wait()?.success();
    ensure!(age.wait()?.success() && archived, "tar or age failed");
    Ok(())
}

/// Regular `*.log` and `*.json` files under 20 MiB; links are not followed.
fn collect(root: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let meta = fs::symlink_metadata(root.join(&path))?;
        if meta.is_dir() {
            collect(root, &path, files)?;
        } else if meta.is_file()
            && meta.len() < 20 << 20
            && path
                .extension()
                .is_some_and(|extension| extension == "log" || extension == "json")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// The shard's closing summary, also written to the job summary on GitHub so
/// retries stay visible on a green run.
fn report(total: usize, kept: usize, retried: &[String], failed: &[String]) -> Result<()> {
    let mut summary = String::new();
    if !retried.is_empty() {
        println!("\nPassed only on retry ({}):", retried.len());
        summary.push_str(
            "**Passed only on retry.** A pass on retry can still hide a product bug:\n\n",
        );
        for line in retried {
            println!(" - {line}");
            writeln!(summary, "- `{line}`")?;
        }
    }
    if !failed.is_empty() {
        println!(
            "\nQualification shard failed: {} of {total} cases failed.",
            failed.len()
        );
        summary.push_str("\n**Failed:**\n\n");
        for line in failed {
            println!(" - {line}");
            writeln!(summary, "- `{line}`")?;
        }
    } else if kept > 0 {
        println!(
            "\nQualification shard passed: {total} cases, {kept} kept from an earlier attempt."
        );
    } else {
        println!("\nQualification shard passed: {total} cases.");
    }
    if kept > 0 {
        writeln!(
            summary,
            "\n{kept} cases kept their pass from an earlier attempt."
        )?;
    }
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|_| !summary.is_empty()) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        writeln!(file, "### Qualification shard\n\n{summary}")?;
    }
    Ok(())
}
