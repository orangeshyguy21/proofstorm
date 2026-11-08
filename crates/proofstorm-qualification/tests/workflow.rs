use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn workflow(source: &str) -> Value {
    serde_saphyr::from_str(source).unwrap()
}

/// Run a gate step's own `jq -e '...' <<< "$NEEDS"` against job results.
fn gate_passes(run: &str, needs: &Value) -> bool {
    let program = run
        .strip_prefix("jq -e '")
        .and_then(|rest| rest.strip_suffix("' <<< \"$NEEDS\""))
        .unwrap_or_else(|| panic!("unexpected gate step: {run}"));
    let mut jq = Command::new("jq")
        .args(["-e", program])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    jq.stdin
        .take()
        .unwrap()
        .write_all(needs.to_string().as_bytes())
        .unwrap();
    jq.wait().unwrap().success()
}

/// A gate that requires every dependency, by exact name, to succeed.
fn assert_gate_requires_every_dependency(job: &Value, run: &str) {
    assert_eq!(job["if"], "always()");
    let needs = job["needs"].as_array().unwrap();
    let good: Value = needs
        .iter()
        .map(|name| {
            (
                name.as_str().unwrap().to_owned(),
                json!({"result":"success"}),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    assert!(gate_passes(run, &good));
    for name in needs {
        let name = name.as_str().unwrap();
        for result in ["failure", "cancelled", "skipped", ""] {
            let mut broken = good.clone();
            broken[name]["result"] = result.into();
            assert!(!gate_passes(run, &broken), "{name}: {result}");
        }
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove(name);
        assert!(!gate_passes(run, &missing), "{name} missing");
    }
    let mut unknown = good;
    unknown["unknown"] = json!({"result":"success"});
    assert!(!gate_passes(run, &unknown));
}

#[test]
fn actual_merge_dependencies_reject_failure_cancellation_skip_and_missing_results() {
    let checks = workflow(include_str!("../../../.github/workflows/check.yml"));
    let job = &checks["jobs"]["merge-qualification"];
    assert_eq!(job["name"], "Merge qualification");
    assert_gate_requires_every_dependency(job, job["steps"][0]["run"].as_str().unwrap());
    assert_eq!(
        checks["jobs"]["qualification"]["uses"],
        "./.github/workflows/qualification.yml"
    );
    assert!(checks["jobs"]["qualification"]["if"].is_null());
    assert_eq!(checks["on"]["push"]["branches"], json!(["main"]));
    assert_eq!(checks["on"]["pull_request"]["branches"], json!(["main"]));
    for event in ["pull_request", "push"] {
        assert!(checks["on"].get(event).is_some());
        assert!(checks["on"][event]["paths"].is_null());
        assert!(checks["on"][event]["paths-ignore"].is_null());
    }
    assert_eq!(
        checks["concurrency"]["cancel-in-progress"],
        "${{ github.event_name == 'pull_request' }}"
    );
    assert!(
        checks["concurrency"]["group"]
            .as_str()
            .unwrap()
            .contains("github.run_id")
    );
}

#[test]
fn native_execution_and_evidence_cannot_be_optional_or_publish_packages() {
    let qualification = workflow(include_str!("../../../.github/workflows/qualification.yml"));
    assert_eq!(qualification["permissions"], json!({"contents":"read"}));
    let jobs = qualification["jobs"].as_object().unwrap();
    for job in jobs.values() {
        assert!(job["continue-on-error"].is_null());
        assert!(job["permissions"].is_null());
        for step in job["steps"].as_array().unwrap() {
            assert!(step["continue-on-error"].is_null());
        }
    }
    assert_eq!(jobs["execute"]["strategy"]["fail-fast"], false);
    let needs: BTreeSet<_> = jobs["verify"]["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(needs, ["plan", "build", "images", "execute"].into());
    assert_gate_requires_every_dependency(
        &jobs["verify"],
        jobs["verify"]["steps"][0]["run"].as_str().unwrap(),
    );
    let platforms: BTreeSet<_> = jobs["build"]["strategy"]["matrix"]["include"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["platform"].as_str().unwrap())
        .collect();
    assert_eq!(platforms, ["linux/amd64", "linux/arm64"].into());
    // Steps may not continue on error, so the earlier-receipts download must
    // use a pattern: a pattern that matches nothing succeeds and downloads nothing.
    let steps = jobs["execute"]["steps"].as_array().unwrap();
    let earlier = steps
        .iter()
        .find(|step| step["with"]["path"] == "${{ runner.temp }}/earlier-receipts")
        .unwrap();
    assert!(earlier["with"]["name"].is_null());
    assert_eq!(
        earlier["with"]["pattern"],
        "qualification-receipts-${{ matrix.shard }}-${{ github.sha }}"
    );
    assert_eq!(jobs["execute"]["needs"], json!(["plan", "build", "images"]));
}

#[test]
fn image_preparation_is_shared_and_required_before_native_execution() {
    let workflow = workflow(include_str!("../../../.github/workflows/qualification.yml"));
    let jobs = &workflow["jobs"];
    assert_eq!(jobs["images"]["needs"], "plan");
    assert!(jobs["images"]["strategy"].is_null());
    {
        let steps = jobs["execute"]["steps"].as_array().unwrap();
        let download = steps
            .iter()
            .position(|step| step["with"]["name"] == "qualification-images-${{ github.sha }}")
            .unwrap();
        let consumer = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("qualification-cache.sh with"))
            })
            .unwrap();
        assert!(download < consumer);
        assert!(
            steps
                .iter()
                .any(|step| step["run"].as_str().is_some_and(|run| {
                    run.contains(
                        "docker image load --input \"$RUNNER_TEMP/artifacts/runtime-images.tar\"",
                    )
                }))
        );
    }
    assert!(
        jobs["verify"]["steps"][0]["run"]
            .as_str()
            .unwrap()
            .contains("\"images\"")
    );
}

#[test]
fn upstream_behavioral_suite_is_manual_and_independent_of_required_checks() {
    let checks = workflow(include_str!("../../../.github/workflows/check.yml"));
    let behavioral = workflow(include_str!("../../../.github/workflows/behavioral.yml"));
    let qualification = workflow(include_str!("../../../.github/workflows/qualification.yml"));
    assert_eq!(
        checks["jobs"]["qualification"]["with"]["suite"],
        "compatibility"
    );
    assert_eq!(
        qualification["on"]["workflow_call"]["inputs"]["suite"]["default"],
        "compatibility"
    );
    assert_eq!(
        behavioral["on"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["workflow_dispatch"]
    );
    assert_eq!(
        behavioral["jobs"]["qualification"]["uses"],
        "./.github/workflows/qualification.yml"
    );
    assert_eq!(behavioral["jobs"]["qualification"]["with"]["suite"], "full");
    assert!(
        checks["jobs"]
            .as_object()
            .unwrap()
            .values()
            .all(|job| job["uses"] != "./.github/workflows/behavioral.yml")
    );
}

/// GitHub's "re-run failed jobs" keeps what passed jobs uploaded but reruns
/// only the failed ones. Every artifact a later job downloads must therefore
/// be named without the attempt number, and each producer must replace its own
/// upload when it re-runs; otherwise one lost runner costs a full re-run.
#[test]
fn downloaded_artifacts_survive_a_rerun_of_failed_jobs() {
    for (file, source) in [
        (
            "qualification.yml",
            include_str!("../../../.github/workflows/qualification.yml"),
        ),
        (
            "check.yml",
            include_str!("../../../.github/workflows/check.yml"),
        ),
    ] {
        let workflow = workflow(source);
        let mut uploads = BTreeMap::new();
        let mut downloads = Vec::new();
        for job in workflow["jobs"].as_object().unwrap().values() {
            // A reusable-workflow call has no steps of its own.
            let Some(steps) = job["steps"].as_array() else {
                continue;
            };
            for step in steps {
                let uses = step["uses"].as_str().unwrap_or_default();
                let with = &step["with"];
                if uses.starts_with("actions/upload-artifact@") {
                    uploads.insert(with["name"].as_str().unwrap(), with["overwrite"] == true);
                } else if uses.starts_with("actions/download-artifact@") {
                    downloads.push(with["name"].as_str().or(with["pattern"].as_str()).unwrap());
                }
            }
        }
        assert!(!downloads.is_empty(), "{file}");
        for wanted in downloads {
            assert!(!wanted.contains("run_attempt"), "{file}: {wanted}");
            let producers: Vec<_> = uploads
                .iter()
                .filter(|(name, _)| match wanted.split_once('*') {
                    Some((prefix, suffix)) => name.starts_with(prefix) && name.ends_with(suffix),
                    None => **name == wanted,
                })
                .collect();
            assert!(!producers.is_empty(), "{file}: nothing uploads {wanted}");
            for (name, overwrite) in producers {
                assert!(
                    overwrite,
                    "{file}: {name} must replace its upload on re-run"
                );
            }
        }
    }
    // Verify checks the plan's own attempt, which re-runs carry over.
    let qualification = workflow(include_str!("../../../.github/workflows/qualification.yml"));
    let jobs = &qualification["jobs"];
    assert_eq!(
        jobs["plan"]["outputs"]["attempt"],
        "${{ steps.plan.outputs.attempt }}"
    );
    let verify = serde_json::to_string(&jobs["verify"]).unwrap();
    assert!(!verify.contains("github.run_attempt"));
    assert!(verify.contains("needs.plan.outputs.attempt"));
}
