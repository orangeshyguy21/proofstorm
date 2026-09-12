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
    let human = cli()
        .arg("--home")
        .arg(&home)
        .args(["dev", "init"])
        .output()
        .unwrap();
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert_eq!(human.stdout, b"Local permissions configured.\n");
    assert_eq!(human.stderr, b"Configuring local permissions...\n");
    for args in [["--json", "dev", "init"], ["dev", "init", "--json"]] {
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
        ("gui stop", "Stopping GUI"),
    ] {
        let human = cli().args(command.split_whitespace()).output().unwrap();
        assert!(!human.status.success());
        assert!(human.stdout.is_empty());
        let stderr = String::from_utf8(human.stderr).unwrap();
        assert!(stderr.starts_with(&format!("{label}...\n")));
        assert!(stderr.contains("Error:") && stderr.contains("--home"));
        assert!(!stderr.contains(['\r', '\x1b']));
        let machine = cli()
            .args(command.split_whitespace())
            .arg("--json")
            .output()
            .unwrap();
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
        "gui start",
        "gui stop",
        "gui status",
        "doctor",
        "agent open",
        "agent configure",
        "dev init",
        "dev serve",
        "up",
        "rm",
        "status",
        "ls",
        "exec",
        "ops show",
        "ops ls",
        "ops sync",
        "connect",
        "version",
        "internal install-bundle",
    ] {
        let output = cli()
            .args(command.split_whitespace())
            .arg("--help")
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("--json"),
            "{command}"
        );
    }
}

#[test]
fn operation_listing_uses_recorded_state_and_requires_read_access() {
    use proofstorm_core::Capability;
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("history.sqlite3");
    let store = proofstorm_store::Store::open(&database).unwrap();
    proofstorm_app::developer::configure(&store, "local-lab", "developer").unwrap();
    store
        .reserve_lab("local-lab", "developer", "demo", "fixture")
        .unwrap();
    let read = || {
        cli()
            .arg("--database")
            .arg(&database)
            .arg("--kubeconfig")
            .arg(root.path().join("missing-kubeconfig"))
            .args(["--json", "ops", "ls", "demo"])
            .output()
            .unwrap()
    };
    store
        .replace_grants(
            "local-lab",
            "developer",
            [Capability::LabStatus, Capability::ExperimentRead],
        )
        .unwrap();
    let output = read();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(page["items"], serde_json::json!([]));
    assert!(page["next_cursor"].is_null());
    store
        .replace_grants("local-lab", "developer", [Capability::LabStatus])
        .unwrap();
    assert!(!read().status.success());
    assert!(!root.path().join("missing-kubeconfig").exists());
}
