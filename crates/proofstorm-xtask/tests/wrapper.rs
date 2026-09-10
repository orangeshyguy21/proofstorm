use std::{path::Path, process::Command};

#[test]
fn bash_wrapper_uses_real_metadata_safeguards_without_a_runtime() {
    check_wrapper("test-develop.sh");
}

#[test]
fn bash_release_build_uses_real_snapshots_and_packaging_without_compilers() {
    check_wrapper("test-release-build.sh");
}

#[test]
fn bash_linux_installer_uses_real_input_checks_without_docker() {
    check_wrapper("test-linux-install-smoke.sh");
}

#[test]
fn bash_linux_build_and_install_use_real_snapshots_without_docker() {
    check_wrapper("test-linux-build.sh");
}

fn check_wrapper(script: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let result = Command::new("bash")
        .arg(root.join("scripts").join(script))
        .arg(env!("CARGO_BIN_EXE_proofstorm-xtask"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
