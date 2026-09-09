use std::process::Command;

#[test]
fn release_metadata_and_version_do_not_resolve_installation_state() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_proofstorm"))
        .current_dir(root.path())
        .env("PROOFSTORM_HOME", root.path().join("missing"))
        .env("PROOFSTORM_CONTEXT", "")
        .arg("release-info")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert!(!info["workload_images"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let version = Command::new(env!("CARGO_BIN_EXE_proofstorm"))
        .current_dir(root.path())
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
}
