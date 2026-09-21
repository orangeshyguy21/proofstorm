//! Exercise the CI process/file boundaries with synthetic receipts. These tests
//! validate orchestration only; they are not evidence of live payment behavior.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use proofstorm_qualification::{ImageEvidence, Plan, Receipt};
use serde_json::{Value, json};
use tempfile::TempDir;

fn qualifier(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_proofstorm-qualification"))
        .args(args)
        .output()
        .unwrap()
}

fn successful(args: &[&str]) -> Output {
    let output = qualifier(args);
    assert!(output.status.success(), "{output:?}");
    output
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn planned(root: &Path, mode: &str) -> (String, Plan) {
    let path = root.join(format!("{mode}.json"));
    let path = path.to_str().unwrap().to_owned();
    successful(&["plan", &"a".repeat(40), "123", "1", mode, &path]);
    let plan = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    (path, plan)
}

fn synthetic_receipts(plan: &Plan) -> Vec<Receipt> {
    plan.cases
        .iter()
        .filter(|case| case.required)
        .map(|case| Receipt {
            format_version: 1,
            identity: plan.identity.clone(),
            plan_digest: plan.digest(),
            case_id: case.id.clone(),
            platform: case.platform.clone(),
            components: case.components.clone(),
            claims: case.claims.clone(),
            images: case
                .components
                .iter()
                .map(|component| {
                    (
                        component.source.clone(),
                        ImageEvidence {
                            manifest: format!("sha256:{}", "a".repeat(64)),
                            config: format!("sha256:{}", "b".repeat(64)),
                        },
                    )
                })
                .collect(),
            passed: true,
            cleanup_verified: true,
            preservation_verified: true,
            stage: "complete".into(),
            elapsed_seconds: 1,
        })
        .collect()
}

#[test]
fn cli_policy_and_matrix_schedule_each_required_case_once_on_its_native_runner() {
    let root = TempDir::new().unwrap();
    let mut scheduled = BTreeMap::new();
    for mode in ["full", "documentation"] {
        let (path, plan) = planned(root.path(), mode);
        let matrix: Value = serde_json::from_slice(&successful(&["matrix", &path]).stdout).unwrap();
        let mut seen = BTreeSet::new();
        let mut platforms = BTreeSet::new();
        let mut shards = BTreeSet::new();
        for entry in matrix["include"].as_array().unwrap() {
            assert!(shards.insert(entry["shard"].as_str().unwrap()));
            let platform = entry["platform"].as_str().unwrap();
            platforms.insert(platform);
            assert_eq!(
                entry["runner"],
                match platform {
                    "linux/amd64" => "ubuntu-24.04",
                    "linux/arm64" => "ubuntu-24.04-arm",
                    _ => panic!("unexpected platform"),
                }
            );
            assert_eq!(entry["arch"], platform.strip_prefix("linux/").unwrap());
            let ids = entry["cases"].as_array().unwrap();
            assert!((1..=8).contains(&ids.len()));
            for id in ids {
                let id = id.as_str().unwrap();
                assert!(seen.insert(id));
                let case = plan.case(id).unwrap();
                assert!(case.required);
                assert_eq!(case.platform, platform);
            }
        }
        assert_eq!(platforms, ["linux/amd64", "linux/arm64"].into());
        assert_eq!(
            seen,
            plan.cases
                .iter()
                .filter(|case| case.required)
                .map(|case| case.id.as_str())
                .collect()
        );
        scheduled.insert(mode, seen.len());
        let id = seen.first().unwrap();
        let case: Value = serde_json::from_slice(&successful(&["case", &path, id]).stdout).unwrap();
        assert_eq!(case, serde_json::to_value(plan.case(id).unwrap()).unwrap());
    }
    assert!(scheduled["documentation"] > 0);
    assert!(scheduled["documentation"] < scheduled["full"]);

    let paths = root.path().join("changed-paths");
    for (contents, expected) in [
        ("README.md\ndocs/merge-qualification.md\n", "documentation"),
        ("README.md\nCargo.toml\n", "full"),
        ("", "full"),
    ] {
        fs::write(&paths, contents).unwrap();
        let output = successful(&["policy", paths.to_str().unwrap()]);
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn downloaded_artifacts_and_aggregate_reject_failed_missing_duplicate_and_stale_evidence() {
    let root = TempDir::new().unwrap();
    let (plan_path, plan) = planned(root.path(), "full");
    let receipts = synthetic_receipts(&plan);
    let directory = root.path().join("receipts");
    for receipt in &receipts {
        let artifact = directory.join(receipt.platform.replace('/', "-"));
        fs::create_dir_all(&artifact).unwrap();
        write_json(&artifact.join(format!("{}.json", receipt.case_id)), receipt);
    }
    let verify = ["verify", &plan_path, directory.to_str().unwrap()];
    successful(&verify);

    let receipt = &receipts[0];
    let path = directory
        .join(receipt.platform.replace('/', "-"))
        .join(format!("{}.json", receipt.case_id));
    fs::remove_file(&path).unwrap();
    assert!(!qualifier(&verify).status.success());
    write_json(&path, receipt);

    let duplicate = directory.join("duplicate-artifact");
    fs::create_dir(&duplicate).unwrap();
    write_json(&duplicate.join(path.file_name().unwrap()), receipt);
    assert!(!qualifier(&verify).status.success());
    fs::remove_dir_all(duplicate).unwrap();

    for mutate in [
        |value: &mut Receipt| value.identity.attempt += 1,
        |value: &mut Receipt| value.passed = false,
        |value: &mut Receipt| value.cleanup_verified = false,
        |value: &mut Receipt| value.preservation_verified = false,
    ] {
        let mut changed = receipt.clone();
        mutate(&mut changed);
        write_json(&path, &changed);
        assert!(!qualifier(&verify).status.success());
    }
    write_json(&path, receipt);
    successful(&verify);

    let needs_path = root.path().join("needs.json");
    let checks: Value =
        serde_saphyr::from_str(include_str!("../../../.github/workflows/check.yml")).unwrap();
    let mut needs: Value = checks["jobs"]["merge-qualification"]["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| {
            (
                name.as_str().unwrap().to_owned(),
                json!({"result":"success"}),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    write_json(&needs_path, &needs);
    successful(&["aggregate", needs_path.to_str().unwrap()]);
    for result in ["failure", "skipped", "cancelled"] {
        needs["qualification"]["result"] = result.into();
        write_json(&needs_path, &needs);
        assert!(
            !qualifier(&["aggregate", needs_path.to_str().unwrap()])
                .status
                .success()
        );
    }
}

#[cfg(unix)]
#[test]
fn real_shard_continues_after_failures_and_exports_only_receipts() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = TempDir::new().unwrap();
    let (plan_path, plan) = planned(root.path(), "documentation");
    let receipts: Vec<_> = synthetic_receipts(&plan).into_iter().take(3).collect();
    let ids: Vec<_> = receipts.iter().map(|receipt| &receipt.case_id).collect();
    let cases = root.path().join("cases.json");
    write_json(&cases, &ids);
    let scripts = root.path().join("scripts");
    let binaries = root.path().join("target/check/debug");
    let fixtures = root.path().join("fixtures");
    for path in [&scripts, &binaries, &fixtures] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(
        scripts.join("qualification-shard.sh"),
        include_str!("../../../scripts/qualification-shard.sh"),
    )
    .unwrap();
    symlink(
        env!("CARGO_BIN_EXE_proofstorm-qualification"),
        binaries.join("proofstorm-qualification"),
    )
    .unwrap();
    for receipt in &receipts {
        write_json(&fixtures.join(format!("{}.json", receipt.case_id)), receipt);
    }
    let stub = binaries.join("proofstorm-acceptance");
    fs::write(
        &stub,
        r#"#!/usr/bin/env bash
set -euo pipefail
while (( $# )); do
  case "$1" in
    --work-dir) work=$2; shift ;;
    --qualification-case) id=$2; shift ;;
  esac
  shift
done
mkdir "$work"
printf '%s\n' "$id" >> "$QUALIFICATION_TEST_CALLS"
printf 'private fixture output\n' > "$work/private.log"
[[ "$id" != "$QUALIFICATION_TEST_MISSING" ]] || exit 0
cp "$QUALIFICATION_TEST_FIXTURES/$id.json" "$work/qualification-receipt.json"
if [[ "$id" == "$QUALIFICATION_TEST_FAILED" ]]; then
  jq '.passed=false | .stage="funding"' "$work/qualification-receipt.json" > "$work/changed.json"
  mv "$work/changed.json" "$work/qualification-receipt.json"
  printf '%s\n' '{"native":{"reason":"channel-request-rejected","stdout":"private fixture output"},"locations":["private fixture output","crates/proofstorm-acceptance/src/native.rs:100:5","crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:400:5"]}' > "$work/gate-failure.json"
  exit 7
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    for fail in [false, true] {
        let directory = root.path().join(if fail { "failed" } else { "successful" });
        fs::create_dir(&directory).unwrap();
        let calls = directory.join("calls");
        let output = Command::new("bash")
            .arg(scripts.join("qualification-shard.sh"))
            .arg(&plan_path)
            .arg(&cases)
            .arg(directory.join("work"))
            .arg(directory.join("receipts"))
            .env("QUALIFICATION_TEST_CALLS", &calls)
            .env("QUALIFICATION_TEST_FIXTURES", &fixtures)
            .env("QUALIFICATION_TEST_FAILED", if fail { ids[0] } else { "" })
            .env("QUALIFICATION_TEST_MISSING", if fail { ids[1] } else { "" })
            .output()
            .unwrap();
        assert_eq!(output.status.success(), !fail, "{output:?}");
        assert_shard_log(&String::from_utf8(output.stdout).unwrap(), fail, &ids);
        assert_eq!(
            fs::read_to_string(calls)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ids
        );
        let files: Vec<_> = fs::read_dir(directory.join("receipts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), if fail { 2 } else { 3 });
        for file in files {
            assert_eq!(file.extension().unwrap(), "json");
            let receipt: Receipt = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
            assert_eq!(receipt.passed, !(fail && receipt.case_id == *ids[0]));
        }
    }
}

#[cfg(unix)]
fn assert_shard_log(log: &str, fail: bool, ids: &[&String]) {
    assert!(!log.contains("private fixture output"));
    if fail {
        for (id, reason) in [(ids[0], "acceptance exited 7"), (ids[1], "receipt missing")] {
            assert!(log.contains(&format!("::error::Qualification {id}: {reason}")));
        }
        let summary = log.split("Qualification shard failed:").nth(1).unwrap();
        assert!(summary.starts_with(" 2 of 3 cases failed."));
        assert!(summary.contains(&format!("{}: acceptance exited 7", ids[0])));
        assert!(summary.contains(
            "channel-request-rejected; at crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:400:5"
        ));
        assert!(summary.contains("stage=funding"));
        assert!(summary.contains(&format!("{}: receipt missing", ids[1])));
        assert!(
            !summary.contains(ids[2]),
            "the final passing case must not be blamed"
        );
        assert!(
            log.find(&format!("{} completed", ids[2])).unwrap()
                < log.find("Qualification shard failed:").unwrap(),
            "the failure summary must remain visible after the final passing case"
        );
    } else {
        assert!(log.ends_with("Qualification shard passed: 3 cases.\n"));
        assert!(!log.contains("::error::"));
    }
}
