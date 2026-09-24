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
    for mode in ["full", "compatibility", "documentation", "pull"] {
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
    assert!(scheduled["pull"] > 0);
    assert!(scheduled["pull"] < scheduled["documentation"]);
    assert!(scheduled["documentation"] > 0);
    assert!(scheduled["documentation"] < scheduled["compatibility"]);
    assert!(scheduled["compatibility"] < scheduled["full"]);

    let paths = root.path().join("changed-paths");
    for (contents, expected) in [
        ("README.md\ndocs/merge-qualification.md\n", "pull"),
        ("README.md\nCargo.toml\n", "pull"),
        ("Cargo.toml\ndocker/mint/cdk/Dockerfile\n", "compatibility"),
        ("", "compatibility"),
    ] {
        fs::write(&paths, contents).unwrap();
        let output = successful(&["policy", paths.to_str().unwrap()]);
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn unsupported_mint_auth_does_not_remove_identity_provider_coverage() {
    let root = TempDir::new().unwrap();
    let (_, plan) = planned(root.path(), "full");
    let cases = serde_json::to_value(&plan.cases).unwrap();
    for platform in ["linux/amd64", "linux/arm64"] {
        assert!(cases.as_array().unwrap().iter().any(|case| {
            case["platform"] == platform
                && case["required"] == true
                && case["scenario"]["name"] == "keycloak"
                && case["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["implementation"] == "postgresql")
        }));
    }
    assert!(!cases.to_string().contains("nutshell-oidc"));
    assert!(!cases.to_string().contains("nut21_clear"));
    assert!(!cases.to_string().contains("nut22_blind"));
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

/// Stands in for the acceptance binary: records each case it was asked to
/// run, emits the fixture receipt, and writes one public gate diagnostic per
/// category the shard script has to classify.
#[cfg(unix)]
const ACCEPTANCE_STUB: &str = r#"#!/usr/bin/env bash
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
if [[ "$id" == "${QUALIFICATION_TEST_FLAKY:-}" && ! -e "$QUALIFICATION_TEST_CALLS.flaked" ]]; then
  : > "$QUALIFICATION_TEST_CALLS.flaked"
  jq '.passed=false | .stage="teardown"' "$QUALIFICATION_TEST_FIXTURES/$id.json" > "$work/qualification-receipt.json"
  printf '%s\n' '{"reason":"tool-rpc-error","locations":["crates/proofstorm-acceptance/src/cell.rs:135:5"],"tool":{"tool":"cell_remove","code":"runtime_failure","http_status":409}}' > "$work/gate-failure.json"
  exit 7
fi
[[ "$id" != "$QUALIFICATION_TEST_MISSING" ]] || exit 0
cp "$QUALIFICATION_TEST_FIXTURES/$id.json" "$work/qualification-receipt.json"
if [[ "$id" == "$QUALIFICATION_TEST_FAILED" ]]; then
  jq '.passed=false | .stage="funding"' "$work/qualification-receipt.json" > "$work/changed.json"
  mv "$work/changed.json" "$work/qualification-receipt.json"
  printf '%s\n' '{"native":{"reason":"channel-request-rejected","stdout":"private fixture output"},"locations":["private fixture output","crates/proofstorm-acceptance/src/native.rs:100:5","crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:400:5"]}' > "$work/gate-failure.json"
  if [[ "$QUALIFICATION_TEST_CATEGORY" == "container-exited" ]]; then
    jq '.reason="container-exited" | .native=null' "$work/gate-failure.json" > "$work/changed.json"
    mv "$work/changed.json" "$work/gate-failure.json"
  fi
  if [[ "$QUALIFICATION_TEST_CATEGORY" == "operation-container-failed" ]]; then
    jq '.reason="operation-container-failed" | .native=null | .operation={"reason":"operation-container-failed","code":"container_failed","phase":"failed","container":"component","exit_code":137,"termination_reason":"OOMKilled"}' "$work/gate-failure.json" > "$work/changed.json"
    mv "$work/changed.json" "$work/gate-failure.json"
  fi
  if [[ "$QUALIFICATION_TEST_CATEGORY" == "tool-rpc-error" ]]; then
    jq '.reason="tool-rpc-error" | .native=null | .tool={"reason":"tool-rpc-error","tool":"cell_remove","rpc_code":-32603,"code":"runtime_failure","http_status":409}' "$work/gate-failure.json" > "$work/changed.json"
    mv "$work/changed.json" "$work/gate-failure.json"
  fi
  exit 7
fi
"#;

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
    fs::write(&stub, ACCEPTANCE_STUB).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    for category in [
        "successful",
        "channel-request-rejected",
        "container-exited",
        "operation-container-failed",
        "tool-rpc-error",
        "flaky",
    ] {
        let fail = !matches!(category, "successful" | "flaky");
        let directory = root.path().join(category);
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
            .env("QUALIFICATION_TEST_CATEGORY", category)
            .env(
                "QUALIFICATION_TEST_FLAKY",
                if category == "flaky" { ids[0] } else { "" },
            )
            .output()
            .unwrap();
        assert_eq!(output.status.success(), !fail, "{output:?}");
        assert_shard_log(
            &String::from_utf8(output.stdout).unwrap(),
            fail,
            category,
            &ids,
        );
        // Every failed case is retried exactly once in a fresh work directory.
        let expected: Vec<&String> = match category {
            "successful" => ids.clone(),
            "flaky" => vec![ids[0], ids[0], ids[1], ids[2]],
            _ => vec![ids[0], ids[0], ids[1], ids[1], ids[2]],
        };
        assert_eq!(
            fs::read_to_string(calls)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            expected
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
fn assert_shard_log(log: &str, fail: bool, category: &str, ids: &[&String]) {
    assert!(!log.contains("private fixture output"));
    if fail {
        for (id, reason) in [(ids[0], "acceptance exited 7"), (ids[1], "receipt missing")] {
            assert!(log.contains(&format!("::error::Qualification {id}: {reason}")));
        }
        let summary = log.split("Qualification shard failed:").nth(1).unwrap();
        assert!(summary.starts_with(" 2 of 3 cases failed."));
        assert!(summary.contains(&format!("{}: acceptance exited 7", ids[0])));
        assert!(summary.contains(&format!(
            "{category}; at crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:400:5"
        )));
        assert!(summary.contains("stage=funding"));
        if category == "operation-container-failed" {
            // The kubelet's termination reason is what separates an undersized
            // container limit from a component that failed on its own terms.
            assert!(summary.contains(&format!(
                "{category}; at crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:400:5; OOMKilled; exit 137"
            )));
        }
        if category == "tool-rpc-error" {
            // The failing tool and its typed code are what separate a teardown
            // conflict from a product failure when error text stays private.
            assert!(summary.contains(&format!(
                "{category}; at crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:400:5; tool cell_remove; code runtime_failure; http 409"
            )));
        }
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
        for id in [ids[0], ids[1]] {
            assert!(log.contains(&format!("::warning::Qualification {id} attempt 1: ")));
        }
    } else {
        assert!(log.ends_with("Qualification shard passed: 3 cases.\n"));
        assert!(!log.contains("::error::"));
        if category == "flaky" {
            // A retry pass is visible, with the first attempt's public diagnostic.
            let first = format!(
                "{}: acceptance exited 7; tool-rpc-error; at crates/proofstorm-acceptance/src/cell.rs:135:5; tool cell_remove; code runtime_failure; http 409; stage=teardown",
                ids[0]
            );
            assert!(log.contains(&format!("::warning::Qualification {} attempt 1: ", ids[0])));
            assert!(log.contains(&format!("Passed only on retry (1):\n - {first}\n")));
        } else {
            assert!(!log.contains("::warning::"));
        }
    }
}

#[test]
fn compatibility_covers_every_catalog_claim_without_adversarial_or_concurrent_gates() {
    use proofstorm_qualification::Scenario;
    let root = TempDir::new().unwrap();
    let (_, compatibility) = planned(root.path(), "compatibility");
    let (_, full) = planned(root.path(), "full");
    let (_, documentation) = planned(root.path(), "documentation");
    for platform in ["linux/amd64", "linux/arm64"] {
        let selected: Vec<_> = compatibility
            .cases
            .iter()
            .filter(|case| case.required && case.platform == platform)
            .collect();
        let covered: BTreeSet<_> = selected
            .iter()
            .flat_map(|case| case.claims.iter().cloned())
            .collect();
        assert_eq!(covered, compatibility.obligations[platform]);
        for name in [
            "cashu-double-spend",
            "cdk-bdk-stress",
            "cdk-bdk-postgres-stress",
        ] {
            let matching = |plan: &Plan| {
                plan.cases.iter().filter(|case|
                case.platform == platform && matches!(&case.scenario, Scenario::Gate {name: gate, ..} if gate == name))
                .map(|case| case.required).collect::<Vec<_>>()
            };
            assert!(!matching(&full).is_empty(), "{name}");
            assert!(matching(&full).iter().all(|required| *required));
            assert!(matching(&compatibility).iter().all(|required| !required));
            assert!(matching(&documentation).iter().all(|required| !required));
        }
        for name in [
            "cdk-bdk",
            "cdk-bdk-postgres",
            "failed-melt",
            "quote-composition",
            "controller-recovery",
        ] {
            assert!(
                selected.iter().any(
                    |case| matches!(&case.scenario, Scenario::Gate {name: gate, ..} if gate == name)
                ),
                "{name}"
            );
        }
    }
    // A suite change cannot reuse receipts or silently turn off a required case.
    let receipts = synthetic_receipts(&compatibility);
    proofstorm_qualification::verify_receipts(&compatibility, &receipts).unwrap();
    assert!(proofstorm_qualification::verify_receipts(&full, &receipts).is_err());
    let mut tampered = compatibility;
    tampered
        .cases
        .iter_mut()
        .find(|case| case.required)
        .unwrap()
        .required = false;
    assert!(tampered.validate().is_err());
}
