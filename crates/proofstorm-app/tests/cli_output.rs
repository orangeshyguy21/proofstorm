use std::process::Command;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_proofstorm"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PROOFSTORM_") {
            command.env_remove(key);
        }
    }
    command
}

#[test]
fn human_default_and_explicit_json_work_before_and_after_subcommand() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let human = cli().arg("--home").arg(&home).arg("init").output().unwrap();
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert_eq!(human.stdout, b"Local permissions configured.\n");
    assert_eq!(human.stderr, b"Configuring local permissions...\n");
    for args in [["--json", "init"], ["init", "--json"]] {
        let output = cli().arg("--home").arg(&home).args(args).output().unwrap();
        assert!(output.status.success());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["principal"], "developer");
        assert!(!output.stderr.contains(&b'\r'));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Configuring"));
    }
}

#[test]
fn early_failure_preserves_error_and_has_no_success_output() {
    for (command, label) in [
        ("setup", "Checking installation"),
        ("gui", "Checking Proofstorm files"),
        ("stop", "Stopping GUI"),
    ] {
        let human = cli().arg(command).output().unwrap();
        assert!(!human.status.success());
        assert!(human.stdout.is_empty());
        let stderr = String::from_utf8(human.stderr).unwrap();
        assert!(stderr.starts_with(&format!("{label}...\n")));
        assert!(stderr.contains("Error:") && stderr.contains("--home"));
        assert!(!stderr.contains(['\r', '\x1b']));
        let machine = cli().args([command, "--json"]).output().unwrap();
        assert!(!machine.status.success());
        assert!(machine.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&machine.stderr).contains(label));
    }
}

#[test]
fn every_public_command_accepts_global_json() {
    for command in [
        "setup",
        "gui",
        "stop",
        "doctor",
        "open",
        "attach",
        "init",
        "up",
        "down",
        "status",
        "environment",
        "exec",
        "result",
        "connect",
        "sync",
        "serve",
        "install-bundle",
    ] {
        let output = cli().args([command, "--help"]).output().unwrap();
        assert!(output.status.success(), "{command}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("--json"),
            "{command}"
        );
    }
}
