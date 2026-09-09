use std::{path::Path, process::Command};

fn cli(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_proofstorm"));
    command.current_dir(cwd);
    command.arg("--json");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PROOFSTORM_") {
            command.env_remove(key);
        }
    }
    command
}

#[test]
fn init_is_cwd_independent_and_home_flags_override_environment() {
    let root = tempfile::tempdir().unwrap();
    let first_cwd = root.path().join("first project");
    let second_cwd = root.path().join("second project");
    std::fs::create_dir(&first_cwd).unwrap();
    std::fs::create_dir(&second_cwd).unwrap();
    let home = root.path().join("isolated home");
    let first = cli(&first_cwd)
        .env("PROOFSTORM_HOME", root.path().join("must not be created"))
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = cli(&second_cwd)
        .env("PROOFSTORM_HOME", &home)
        .arg("init")
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(first, second);
    assert!(home.join("proofstorm.sqlite3").exists());
    assert!(!home.join("kubeconfig").exists());
    assert!(!first_cwd.join(".proofstorm").exists());
    assert!(!second_cwd.join(".proofstorm").exists());
    assert!(!root.path().join("must not be created").exists());
}

#[test]
fn uninitialized_home_fails_without_creating_state_or_using_development_defaults() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("missing");
    let result = cli(root.path())
        .arg("--home")
        .arg(&home)
        .args(["status", "demo"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("installation home"));
    assert!(!home.exists());
    assert!(!root.path().join(".proofstorm").exists());
}

#[test]
fn setup_prefetch_is_explicit_and_incompatible_with_prepare_only() {
    let root = tempfile::tempdir().unwrap();
    let help = cli(root.path()).args(["setup", "--help"]).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--prefetch-all"));
    let conflict = cli(root.path())
        .args(["setup", "--prefetch-all", "--prepare-only"])
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}
