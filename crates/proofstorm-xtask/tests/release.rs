use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_proofstorm-xtask"))
        .arg("release-check")
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn metadata_command_has_human_and_machine_output_without_changing_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("release info.json");
    let fixture = include_str!("fixtures/release-info.json");
    fs::write(&path, fixture).unwrap();
    let human = run(&path, &["--alpha"]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let output = String::from_utf8(human.stdout).unwrap();
    assert!(output.contains("Release metadata valid"));
    assert!(output.contains("Not release acceptance"));
    let machine = run(&path, &["--alpha", "--json"]);
    assert!(machine.status.success());
    let receipt: Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(receipt["metadata_valid"], true);
    assert_eq!(receipt["release_ready"], false);
    assert_eq!(fs::read_to_string(&path).unwrap(), fixture);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn invalid_metadata_exits_unsuccessfully_without_a_success_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("release-info.json");
    fs::write(&path, "{broken").unwrap();
    let bad_json = run(&path, &["--json"]);
    assert!(!bad_json.status.success());
    assert!(bad_json.stdout.is_empty());
    assert!(String::from_utf8_lossy(&bad_json.stderr).contains("invalid release metadata JSON"));
    let mut info: Value = serde_json::from_str(include_str!("fixtures/release-info.json")).unwrap();
    info["workload_images"] = json!(["example/app:latest"]);
    fs::write(&path, info.to_string()).unwrap();
    let invalid = run(&path, &["--json"]);
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("not pinned"));
}

#[test]
fn invalid_arguments_missing_files_and_non_files_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["--unknown"][..],
        &["--alpha", "--alpha"],
        &["--json", "--json"],
    ] {
        let result = run(dir.path(), args);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("usage:"));
    }
    let missing = run(&dir.path().join("missing.json"), &[]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot read"));
    let directory = run(dir.path(), &[]);
    assert!(!directory.status.success());
    assert!(String::from_utf8_lossy(&directory.stderr).contains("regular file"));
}

#[test]
fn oversized_metadata_is_rejected_before_json_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.json");
    fs::File::create(&path)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let result = run(&path, &["--json"]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("exceeds 4 MiB"));
}

#[test]
fn bundle_verifier_reports_invalid_inputs_without_success_output() {
    let dir = tempfile::tempdir().unwrap();
    for args in [vec![], vec!["--unknown"], vec!["--json", "extra"]] {
        let result = Command::new(env!("CARGO_BIN_EXE_proofstorm-xtask"))
            .arg("release-verify")
            .arg(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.is_empty());
    }
}
