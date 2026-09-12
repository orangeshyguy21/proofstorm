use std::{fs, process::Command};

#[test]
fn invalid_bundle_fails_before_allocating_test_state() {
    let root = tempfile::tempdir().unwrap();
    let bundle = root.path().join("bundle");
    fs::create_dir(&bundle).unwrap();
    fs::write(bundle.join("manifest.json"), "{}").unwrap();
    let work = root.path().join("must-not-exist");
    let output = Command::new(env!("CARGO_BIN_EXE_proofstorm-acceptance"))
        .arg("--bundle")
        .arg(bundle)
        .arg("--work-dir")
        .arg(&work)
        .arg("onboarding")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!work.exists());
}

#[test]
fn ambient_runtime_selection_cannot_start_a_live_gate() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    fs::write(&config, "do not change").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_proofstorm-acceptance"))
        .arg("smoke")
        .env("PROOFSTORM_HOME", root.path())
        .env("PROOFSTORM_DB", root.path().join("must-not-exist"))
        .env("PROOFSTORM_MODE", "memory")
        .env("PROOFSTORM_CAPABILITIES", "cell.materialize")
        .env("KUBECONFIG", &config)
        .env("K3D_CLUSTER_NAME", "foreign-cluster")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("select --checkout-home or --bundle explicitly")
    );
    assert_eq!(fs::read_to_string(config).unwrap(), "do not change");
    assert!(!root.path().join("must-not-exist").exists());
    assert!(!root.path().join("installation.json").exists());
}

#[test]
fn discovery_and_bad_gate_names_do_not_require_a_runtime() {
    let output = Command::new(env!("CARGO_BIN_EXE_proofstorm-acceptance"))
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "smoke")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_proofstorm-acceptance"))
        .arg("typo")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown gate typo"));
}
