//! Opt-in O1 benchmark pilot, sharing acceptance's owned runtime lifecycle.
mod observer;
mod opencode;
pub mod proxy;
pub mod reference;
mod report;
pub mod score;
mod telemetry;
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Context {
    pub root: PathBuf,
    pub work: PathBuf,
    pub home: PathBuf,
    pub mcp: PathBuf,
    pub model: String,
    pub opencode: PathBuf,
}

pub fn allowed(name: &str) -> bool {
    matches!(
        name,
        "catalog_list"
            | "catalog_entry_read"
            | "catalog_config_schema_read"
            | "cell_plan"
            | "cell_read"
            | "cell_search"
            | "cell_up"
            | "cell_inspect"
            | "cell_wait"
            | "cell_exec"
            | "cell_remove"
            | "cell_component_status_list"
            | "cell_inventory_list"
            | "operation_status"
            | "operation_wait"
            | "operation_read"
            | "operation_cancel"
            | "activity_search"
            | "benchmark_checkpoint"
    )
}

pub fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("record parent")?)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
fn append(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn events(work: &Path) -> Result<Vec<Value>> {
    let path = work.join("events.jsonl");
    if !path.exists() {
        return Ok(vec![]);
    }
    let data = fs::read(path)?;
    let mut rows = Vec::new();
    let mut gaps = 0;
    for line in data.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        match serde_json::from_slice::<Value>(line) {
            Ok(row) => rows.push(row),
            Err(_) => gaps += 1,
        }
    }
    let mut next = rows
        .iter()
        .filter_map(|v| v["id"].as_u64())
        .max()
        .unwrap_or(0);
    for _ in 0..gaps {
        next = next.checked_add(1).context("event ID overflow")?;
        rows.push(json!({"kind":"start","id":next,"tool":"telemetry_gap","arguments":null}));
    }
    Ok(rows)
}
fn calls(events: &[Value]) -> Result<Vec<score::Call>> {
    let mut calls = BTreeMap::new();
    for event in events {
        let id = event["id"].as_u64().context("event ID")?;
        if event["kind"] == "start" {
            ensure!(!calls.contains_key(&id), "duplicate call start");
            calls.insert(
                id,
                score::Call {
                    id,
                    tool: event["tool"].as_str().context("tool name")?.into(),
                    arguments: event["arguments"].clone(),
                    success: None,
                    elapsed_ms: 0,
                },
            );
        } else {
            let call = calls.get_mut(&id).context("end without start")?;
            ensure!(call.success.is_none(), "duplicate call end");
            call.success = Some(event["success"].as_bool().context("tool outcome")?);
            call.elapsed_ms = event["elapsed_ms"].as_u64().context("tool duration")?;
        }
    }
    Ok(calls.into_values().collect())
}

pub fn run_gate(context: &crate::GateContext, model: &str, opencode: &Path) -> Result<()> {
    let config = Context {
        root: context.root.clone(),
        work: context.work().into(),
        home: context.installation.home.clone(),
        mcp: context.artifacts.mcp.clone(),
        model: model.into(),
        opencode: opencode.into(),
    };
    save(&config.work.join("benchmark-context.json"), &json!(config))?;
    save(&config.work.join("benchmark-task.json"), &score::task())?;
    save(
        &config.work.join("benchmark-artifacts.json"),
        &json!({
            "cli":context.artifacts.cli,"mcp":context.artifacts.mcp,"resources":context.artifacts.resources,
            "cli_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(&context.artifacts.cli)?)),
            "mcp_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(&context.artifacts.mcp)?)),
            "checkout_registration":read(&config.home.join("checkout-artifacts.json")).ok()
        }),
    )?;
    let result = opencode::run(&config);
    if let Err(error) = &result {
        let mut attempt = read(&config.work.join("benchmark-attempt.json")).unwrap_or(json!({}));
        attempt["outcome"] = json!(if attempt["outcome"] == "completed" {
            "grading_failure"
        } else {
            "harness_failure"
        });
        save(&config.work.join("benchmark-attempt.json"), &attempt)?;
        save(
            &config.work.join("benchmark-error.json"),
            &json!({"error":format!("{error:#}")}),
        )?;
    }
    result
}

/// Verify the original evidence before regrading. No model or payment calls.
pub fn regrade(work: &Path) -> Result<Value> {
    let previous = read(&work.join("benchmark-result.json"))?;
    let evidence = previous["evidence_sha256"]
        .as_object()
        .context("missing evidence manifest")?;
    ensure!(!evidence.is_empty(), "empty evidence manifest");
    for name in EVIDENCE_FILES {
        ensure!(
            !work.join(name).exists() || evidence.contains_key(*name),
            "evidence added after grading: {name}"
        );
    }
    for (name, expected) in evidence {
        ensure!(
            !name.contains('/') && !name.contains('\\') && name != "..",
            "invalid evidence path"
        );
        let actual = format!("{:x}", sha2::Sha256::digest(fs::read(work.join(name))?));
        ensure!(
            expected.as_str() == Some(actual.as_str()),
            "evidence changed: {name}"
        );
    }
    finalize(work)
}

/// Finalize only retained observations; never execute model or payment calls here.
pub fn finalize(work: &Path) -> Result<Value> {
    let mut attempt =
        read(&work.join("benchmark-attempt.json")).unwrap_or(json!({"outcome":"not_started"}));
    if attempt["outcome"] == "running" {
        attempt["outcome"] = json!("interrupted");
    }
    let acceptance = read(&work.join("acceptance.json"))?;
    let observations = read(&work.join("benchmark-observations.json")).unwrap_or(json!({}));
    let task = read(&work.join("benchmark-task.json")).unwrap_or(score::task());
    ensure!(
        task == score::task(),
        "task/scorer changed; use the original scorer for this attempt"
    );
    let transcript = fs::read_to_string(work.join("harness.jsonl")).unwrap_or_default();
    let mut transcript_rows = Vec::new();
    let mut transcript_complete = !transcript.is_empty();
    for line in transcript.lines() {
        match serde_json::from_str(line) {
            Ok(row) => transcript_rows.push(row),
            Err(_) => transcript_complete = false,
        }
    }
    let captured = events(work).and_then(|rows| calls(&rows));
    let telemetry_error = captured.as_ref().err().map(ToString::to_string);
    let captured = captured.unwrap_or_else(|_| {
        vec![score::Call {
            id: 1,
            tool: "telemetry_gap".into(),
            arguments: Value::Null,
            success: None,
            elapsed_ms: 0,
        }]
    });
    let (mut normalized, unauthorized) = telemetry::reconcile(captured, &transcript_rows);
    if !transcript_complete {
        let id = normalized
            .iter()
            .map(|call| call.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("call ID overflow")?;
        normalized.push(score::Call {
            id,
            tool: "telemetry_gap".into(),
            arguments: Value::Null,
            success: None,
            elapsed_ms: 0,
        });
    }
    let mut observations = observations;
    save(&work.join("normalized-calls.json"), &json!(normalized))?;
    if unauthorized {
        observations["autonomy"] = json!(false);
    }
    let mut record = score::grade(
        &observations,
        &normalized,
        attempt["elapsed_seconds"].as_f64(),
        attempt["outcome"].as_str().unwrap_or("interrupted"),
        acceptance["cleanup"] == "passed",
        match acceptance["preservation"].as_str() {
            Some("passed") => Some(true),
            Some("failed") => Some(false),
            _ => None,
        },
    );
    let mut attempt_summary = attempt;
    if let Some(fields) = attempt_summary.as_object_mut()
        && let Some(usage) = fields.remove("usage")
    {
        fields.insert("usage_steps".into(), json!(usage.as_array().map(Vec::len)));
        fields.insert("usage_evidence".into(), json!("benchmark-attempt.json"));
    }
    record["attempt"] = attempt_summary;
    record["runner_cleanup"] = json!(acceptance["cleanup"] == "passed");
    record["preservation"] = acceptance["preservation"].clone();
    record["telemetry_error"] = json!(telemetry_error);
    let mut evidence = BTreeMap::new();
    for name in EVIDENCE_FILES {
        if let Ok(bytes) = fs::read(work.join(name)) {
            evidence.insert(name, format!("{:x}", sha2::Sha256::digest(bytes)));
        }
    }
    record["evidence_sha256"] = json!(evidence);
    save(&work.join("benchmark-result.json"), &record)?;
    write_report(work, &record)?;
    Ok(record)
}

const EVIDENCE_FILES: &[&str] = &[
    "benchmark-task.json",
    "benchmark-context.json",
    "benchmark-artifacts.json",
    "benchmark-manifest.json",
    "benchmark-attempt.json",
    "benchmark-observations.json",
    "funded.json",
    "paid.json",
    "events.jsonl",
    "normalized-calls.json",
    "harness.jsonl",
    "harness.stderr",
    "tools.json",
    "benchmark-error.json",
    "acceptance.json",
    "agent-final.json",
    "agent-report.json",
    "operation-observations.jsonl",
    "terminal-observation.json",
    "cleanup-observation.json",
    "harness-config.private.json",
    "prompt.txt",
    "proxy-ready.json",
    "harness-preflight.private.txt",
];

fn write_report(work: &Path, record: &Value) -> Result<()> {
    use std::fmt::Write as _;
    let number = |value: &Value| {
        value
            .as_f64()
            .map_or_else(|| "unscored".to_owned(), |n| format!("{n:.2}"))
    };
    let summary = format!(
        "# O1 benchmark development pilot\n\nStatus: {}. Accepted score: {} / 100.\n\nOperational task complete: {}. Structured claims valid: {}. JSON-only format: {}.\n\nEnvironment valid: {}. Runner cleanup: {}. Preservation: {}.\n\nDiagnostic quality: {} / 100; quality points: {} / 70; tool points: {} / 15; time points: {} / 15. Task score before environment validation: {} / 100.\n\nTool calls: {} successes, {} failures, {} pending; {} successful calls excluded from scoring.\n\nElapsed seconds: {}. Agent cleanup: {}.\n\nInvalid environments are excluded from model comparisons; a diagnostic task score is not an accepted score. This single-task pilot is not a model ranking. Time targets remain uncalibrated. See benchmark-result.json and private retained evidence.\n",
        record["status"].as_str().unwrap_or("unknown"),
        number(&record["accepted_score"]),
        record["task_success"],
        record["report_valid"],
        record["report_format"],
        record["environment_valid"],
        record["runner_cleanup"],
        record["preservation"].as_str().unwrap_or("unknown"),
        record["diagnostic_score"],
        number(&record["quality_points"]),
        number(&record["tool_points"]),
        number(&record["time_points"]),
        number(&record["task_score"]),
        record["tools"]["raw_successes"],
        record["tools"]["raw_failures"],
        record["tools"]["pending"],
        record["tools"]["excluded_successes"],
        number(&record["elapsed_seconds"]),
        record["assertions"]["agent_cleanup"],
    );
    let mut assertions = String::new();
    if let Some(values) = record["assertions"].as_object() {
        for (name, passed) in values {
            writeln!(assertions, "- {name}: {passed}")?;
        }
    }
    fs::write(
        work.join("benchmark-report.md"),
        format!("{summary}\nAssertions:\n\n{assertions}"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_calls_and_truncated_tail_remain_unknown() -> Result<()> {
        let work = tempfile::tempdir()?;
        fs::write(
            work.path().join("events.jsonl"),
            "{\"kind\":\"start\",\"id\":1,\"tool\":\"cell_up\",\"arguments\":{}}\n{\"kind\":",
        )?;
        let calls = calls(&events(work.path())?)?;
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.success.is_none()));
        Ok(())
    }

    #[test]
    fn offline_regrade_is_repeatable_and_rejects_changed_evidence() -> Result<()> {
        let work = tempfile::tempdir()?;
        save(
            &work.path().join("acceptance.json"),
            &json!({"cleanup":"passed","preservation":"passed"}),
        )?;
        save(&work.path().join("benchmark-task.json"), &score::task())?;
        let original = finalize(work.path())?;
        assert_eq!(original, regrade(work.path())?);
        save(&work.path().join("paid.json"), &json!({}))?;
        assert!(regrade(work.path()).is_err());
        fs::remove_file(work.path().join("paid.json"))?;
        fs::write(work.path().join("benchmark-task.json"), "{}")?;
        assert!(regrade(work.path()).is_err());
        Ok(())
    }

    #[test]
    fn older_contract_is_rejected_before_any_result_is_overwritten() -> Result<()> {
        let work = tempfile::tempdir()?;
        save(&work.path().join("acceptance.json"), &json!({}))?;
        save(
            &work.path().join("benchmark-task.json"),
            &json!({"version":"0.2"}),
        )?;
        let path = work.path().join("benchmark-result.json");
        fs::write(&path, "original result")?;
        assert!(finalize(work.path()).is_err());
        assert_eq!(fs::read_to_string(path)?, "original result");
        assert!(!work.path().join("normalized-calls.json").exists());
        Ok(())
    }
}
