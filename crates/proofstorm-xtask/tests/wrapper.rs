use std::{path::Path, process::Command};

#[test]
fn bash_wrapper_uses_real_metadata_safeguards_without_a_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let result = Command::new("bash")
        .arg(root.join("scripts/test-develop.sh"))
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
