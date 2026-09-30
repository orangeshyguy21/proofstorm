//! Pinned image preparation belongs to setup, before any model or task timer.
use super::{read, save, task};
use anyhow::{Context, Result, ensure};
use proofstorm_app::installation::Installation;
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

fn expected_images(task: &task::Task) -> Result<std::collections::BTreeSet<String>> {
    let lock = proofstorm_core::resolve_lock(
        &serde_json::from_value(task.document())?,
        proofstorm_core::default_catalog(),
    )
    .map_err(anyhow::Error::msg)?;
    let mut images: std::collections::BTreeSet<_> =
        lock.entries.into_iter().map(|entry| entry.image).collect();
    images.insert(proofstorm_kube::images::PROBE_IMAGE.to_string());
    Ok(images)
}

pub const PROFILE: &str = "selected-task-images-v2";

pub fn selected(name: &str) -> Option<&'static task::Task> {
    match name {
        "benchmark-o1" | "benchmark-oracle" | "benchmark-o1-calibration" => Some(task::o1()),
        "benchmark-o5" | "benchmark-o5-oracle" | "benchmark-o5-calibration" => Some(task::o5()),
        "benchmark-o1-diagnostic" => Some(task::o1_diagnostic()),
        "benchmark-o5-diagnostic" => Some(task::o5_diagnostic()),
        _ => None,
    }
}

pub fn run(home: &Path, name: &str) -> Result<()> {
    let work = home
        .parent()
        .context("preparation work directory missing")?;
    let (installation, acceptance) = crate::runner::read(work)?;
    ensure!(
        installation.home == home.canonicalize()?,
        "preparation home mismatch"
    );
    ensure!(
        acceptance["setup"] == "running",
        "preparation is only allowed during setup"
    );
    let task = selected(name).context("not a benchmark gate")?;
    let path = work.join("benchmark-image-preparation.json");
    let mut receipt = json!({"format_version":1,"profile":PROFILE,"status":"running",
        "registry_copy_policy":"go-http2client-disabled-v1",
        "installation_id":installation.id,"task_hash":proofstorm_core::digest_json(task),
        "model_attempt":false,"scope":"pinned task and probe images; no cell creation"});
    ensure!(!path.exists(), "image preparation already attempted");
    save(&path, &receipt)?;
    let started = Instant::now();
    let result = proofstorm_app::bootstrap::prefetch_cell_images(
        &installation,
        &serde_json::from_value(task.document())?,
    );
    receipt["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
    match &result {
        Ok(images) => {
            receipt["status"] = json!("passed");
            receipt["images"] = json!(images);
        }
        Err(error) => {
            receipt["status"] = json!("failed");
            receipt["error"] = json!(format!("{error:#}"));
        }
    }
    save(&path, &receipt)?;
    result.map(|_| ())
}

pub fn verify(work: &Path, installation: &Installation, name: &str) -> Result<Value> {
    let task = selected(name).context("not a benchmark gate")?;
    let path = work.join("benchmark-image-preparation.json");
    ensure!(
        std::fs::symlink_metadata(&path)?.is_file(),
        "linked image preparation receipt"
    );
    let receipt = read(&path)?;
    ensure!(
        receipt["format_version"] == 1
            && receipt["profile"] == PROFILE
            && receipt["registry_copy_policy"] == "go-http2client-disabled-v1"
            && receipt["status"] == "passed"
            && receipt["model_attempt"] == false
            && receipt["installation_id"] == installation.id
            && receipt["task_hash"] == proofstorm_core::digest_json(task)
            && receipt["images"] == json!(expected_images(task)?)
            && receipt["elapsed_seconds"]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= 0.),
        "benchmark image preparation missing, failed, or belongs to another run/task"
    );
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_benchmark_gate_prepares_its_exact_task() {
        for name in crate::gates::NAMES {
            assert_eq!(selected(name).is_some(), name.starts_with("benchmark-"));
        }
        for (standard, reference) in [
            ("benchmark-o1", "benchmark-o1-calibration"),
            ("benchmark-o5", "benchmark-o5-calibration"),
        ] {
            assert_eq!(
                json!(selected(standard).unwrap()),
                json!(selected(reference).unwrap())
            );
        }
        assert_ne!(
            json!(selected("benchmark-o1").unwrap()),
            json!(selected("benchmark-o1-diagnostic").unwrap())
        );
    }

    #[test]
    fn preparation_requires_matching_identity_task_images_and_success() -> Result<()> {
        let root = tempfile::tempdir()?;
        let installation = Installation::initialize(&root.path().join("state"), None, None)?;
        let task = task::o1();
        let valid = json!({"format_version":1,"profile":PROFILE,"status":"passed","model_attempt":false,
            "registry_copy_policy":"go-http2client-disabled-v1",
            "installation_id":installation.id,"task_hash":proofstorm_core::digest_json(task),
            "images":expected_images(task)?,"elapsed_seconds":1.});
        let path = root.path().join("benchmark-image-preparation.json");
        assert!(verify(root.path(), &installation, "benchmark-o1").is_err());
        save(&path, &valid)?;
        verify(root.path(), &installation, "benchmark-o1")?;
        verify(root.path(), &installation, "benchmark-o1-calibration")?;
        assert!(verify(root.path(), &installation, "benchmark-o5").is_err());
        for (key, value) in [
            ("status", json!("failed")),
            ("installation_id", json!("foreign")),
            ("images", json!([])),
            ("task_hash", json!("changed")),
            ("elapsed_seconds", json!(-1)),
            ("model_attempt", json!(true)),
            ("profile", json!("unknown")),
            ("registry_copy_policy", json!("unknown")),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            save(&path, &invalid)?;
            assert!(
                verify(root.path(), &installation, "benchmark-o1").is_err(),
                "{key}"
            );
        }
        Ok(())
    }
}
