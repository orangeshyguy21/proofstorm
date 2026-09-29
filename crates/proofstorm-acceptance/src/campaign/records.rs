//! Classify only completed, independently checked receipts as model results.
use super::{Entry, Plan, hash, regular_read};
use crate::benchmark::save;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path};

fn present(path: &Path) -> bool {
    // A broken link or unreadable entry is not evidence of absence.
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

fn model_result(plan: &Plan, entry: &Entry, acceptance: &Value, result: &Value) -> Result<()> {
    let work = plan.attempt_path(entry);
    let run = &plan.runs[entry.run];
    ensure!(
        acceptance["setup"] == "passed"
            && acceptance["cleanup"] == "passed"
            && acceptance["preservation"] == "passed",
        "runtime verification incomplete"
    );
    ensure!(
        result["environment_valid"] == true
            && matches!(result["status"].as_str(), Some("accepted" | "failed")),
        "model result is not scoreable"
    );
    ensure!(
        matches!(
            result["outcome"].as_str(),
            Some("completed" | "timeout" | "output_limit")
        ),
        "provider, harness or interrupted outcome needs inspection"
    );
    ensure!(
        result["accepted_score"]
            .as_f64()
            .is_some_and(|score| score.is_finite() && (0.0..=100.0).contains(&score)),
        "missing accepted score"
    );
    ensure!(
        result["task_hash"] == run.task_sha256,
        "result task contract changed"
    );
    let evidence = result["evidence_sha256"]
        .as_object()
        .context("missing evidence hashes")?;
    for required in [
        "acceptance.json",
        "benchmark-manifest.json",
        "model-launch.json",
        "benchmark-task.json",
        "benchmark-context.json",
        "benchmark-attempt.json",
        "harness-outcome.json",
        "normalized-calls.json",
    ] {
        ensure!(
            evidence.contains_key(required),
            "missing evidence: {required}"
        );
    }
    for name in crate::benchmark::EVIDENCE_FILES {
        ensure!(
            !present(&work.join(name)) || evidence.contains_key(*name),
            "unhashed evidence: {name}"
        );
    }
    for (name, expected) in evidence {
        ensure!(
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']),
            "invalid evidence name"
        );
        let path = work.join(name);
        ensure!(
            fs::symlink_metadata(&path)?.is_file(),
            "linked or non-file evidence: {name}"
        );
        ensure!(
            expected.as_str() == Some(hash(&path)?.as_str()),
            "evidence changed: {name}"
        );
    }
    let manifest = regular_read(&work.join("benchmark-manifest.json"))?;
    ensure!(
        manifest["model_requested"] == run.model
            && manifest["harness"] == run.harness
            && manifest["source_revision"] == plan.revision
            && manifest["source_dirty"] == false
            && manifest["runner_sha256"] == plan.runner_sha256,
        "manifest does not match frozen campaign"
    );
    let manifest_task: crate::benchmark::task::Task =
        serde_json::from_value(manifest["task"].clone())?;
    ensure!(
        proofstorm_core::digest_json(&manifest_task) == run.task_sha256,
        "manifest task changed"
    );
    let task = regular_read(&work.join("benchmark-task.json"))?;
    ensure!(
        task == manifest["task"],
        "retained task differs from manifest"
    );
    let marker = regular_read(&work.join("model-launch.json"))?;
    ensure!(
        marker["state"] == "launch_requested"
            && marker["model"] == run.model
            && marker["task"] == task["id"]
            && marker["task_version"] == task["version"],
        "launch marker differs from plan"
    );
    verify_preservation(&work, acceptance)?;
    Ok(())
}

fn verify_preservation(work: &Path, acceptance: &Value) -> Result<()> {
    let before = regular_read(&work.join("preservation-before.json"))?;
    let after = regular_read(&work.join("preservation-after.json"))?;
    let excluded = &acceptance["preservation_exclusions"];
    match acceptance["preservation_policy"].as_str() {
        Some(crate::preservation::ADDITIONS_POLICY) => {
            let additions = crate::preservation::verify_run(&before, &after, excluded)?;
            ensure!(
                acceptance["preservation_additions"] == additions,
                "preservation addition report differs from observed inventory"
            );
            Ok(())
        }
        None if acceptance["preservation_policy"].is_null() => {
            crate::preservation::verify_with_exclusions(&before, &after, excluded)
        }
        _ => anyhow::bail!("unknown preservation policy"),
    }
}

fn safe_setup_failure(work: &Path, acceptance: &Value) -> Result<()> {
    ensure!(
        acceptance["setup"] == "failed"
            && acceptance["gates"].as_array().is_some_and(Vec::is_empty)
            && acceptance["preservation"] == "passed",
        "setup failure not verified"
    );
    for name in [
        "model-launch.json",
        "benchmark-context.json",
        "benchmark-attempt.json",
        "benchmark-manifest.json",
        "benchmark-result.json",
    ] {
        ensure!(
            !present(&work.join(name)),
            "possible model preparation: {name}"
        );
    }
    verify_preservation(work, acceptance)?;
    if acceptance["cleanup"] != "passed" {
        let progress = regular_read(&work.join("state/setup-progress.json"))?;
        ensure!(
            acceptance["cleanup"] == "not_run"
                && progress["stage"] == "tools"
                && progress["status"] == "failed"
                && !present(&work.join("state/runtime-resources.json")),
            "setup may have left resources; inspect manually"
        );
    }
    Ok(())
}

pub(super) fn inspect(plan: &Plan, entry: &Entry) -> Value {
    let work = plan.attempt_path(entry);
    let run = &plan.runs[entry.run];
    let mut errors = Vec::new();
    let mut receipt = |name: &str| {
        let path = work.join(name);
        match regular_read(&path) {
            Ok(value) => value,
            Err(error) => {
                if present(&path) {
                    errors.push(format!("{name}: {error:#}"));
                }
                Value::Null
            }
        }
    };
    let acceptance = receipt("acceptance.json");
    let result = receipt("benchmark-result.json");
    let launched = present(&work.join("model-launch.json"));
    let mut hashes = serde_json::Map::new();
    for name in [
        "acceptance.json",
        "benchmark-result.json",
        "model-launch.json",
        "preservation-before.json",
        "preservation-after.json",
        "state/setup-progress.json",
        "state/runtime-resources.json",
    ] {
        hashes.insert(name.into(), json!(hash(&work.join(name)).ok()));
    }
    let hashes = Value::Object(hashes);
    if entry
        .receipts
        .as_ref()
        .is_some_and(|sealed| sealed != &hashes)
    {
        errors.push("receipts changed after campaign recorded this attempt".into());
    }
    let mut safe = false;
    let classification = if launched {
        match model_result(plan, entry, &acceptance, &result) {
            Ok(()) if errors.is_empty() => "model_result",
            check => {
                if let Err(error) = check {
                    errors.push(format!("{error:#}"));
                }
                "model_outcome_unverified"
            }
        }
    } else if acceptance["setup"] == "failed" {
        match safe_setup_failure(&work, &acceptance) {
            Ok(()) => safe = errors.is_empty(),
            Err(error) => errors.push(format!("{error:#}")),
        }
        "setup_failure"
    } else {
        "pre_model_failure"
    };
    json!({"run":run.id,"model":run.model,"harness":run.harness,"task":run.task,
        "setup_attempt":entry.setup_attempt,"work":work,"process_finished":entry.finished,"exit_code":entry.exit_code,"process_error":entry.process_error,
        "classification":classification,"model_launch_requested":launched,"safe_to_retry_setup":safe,
        "score":if classification == "model_result" {result["accepted_score"].clone()} else {Value::Null},
        "acceptance":acceptance,"result":result,"verification_errors":errors,
        "receipt_sha256":hashes})
}

pub(super) fn report(plan: &Plan, rows: &[Value]) -> Result<()> {
    save(&plan.work.join("results.json"), &json!(rows))?;
    save(
        &plan.work.join("summary.json"),
        &json!({
            "planned_runs":plan.runs.len(),
            "model_results":rows.iter().filter(|row| row["classification"] == "model_result").count(),
            "model_launch_intents":rows.iter().filter(|row| row["model_launch_requested"] == true).count(),
            "setup_failures":rows.iter().filter(|row| row["classification"] == "setup_failure").count(),
            "unverified_attempts":rows.iter().filter(|row| row["classification"] != "model_result").count()
        }),
    )?;
    let mut report = tempfile::NamedTempFile::new_in(&plan.work)?;
    writeln!(
        report,
        "# Local benchmark campaign\n\nSetup failures have no model score. Each launched model runs at most once.\n\n| Run | Model | Harness | Setup attempt | Classification | Score |\n| --- | --- | --- | --- | --- | --- |"
    )?;
    for row in rows {
        let cell = |key: &str| row[key].as_str().unwrap_or("").replace('|', "\\|");
        writeln!(
            report,
            "| {} | {} | {} | {} | {} | {} |",
            cell("run"),
            cell("model"),
            cell("harness"),
            row["setup_attempt"],
            cell("classification"),
            row["score"]
                .as_f64()
                .map_or_else(|| "—".into(), |score| format!("{score:.2}"))
        )?;
    }
    report.as_file().sync_all()?;
    report.persist(plan.work.join("report.md"))?;
    Ok(())
}
