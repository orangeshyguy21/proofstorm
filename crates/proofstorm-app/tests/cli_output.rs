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
        "dev reset",
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
fn doctor_and_unregistered_setup_do_not_initialize_a_home() {
    let root = tempfile::tempdir().unwrap();
    for action in ["doctor", "setup"] {
        let home = root.path().join(action);
        let result = cli()
            .arg("--home")
            .arg(&home)
            .args([action, "--json"])
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "unregistered {action} unexpectedly succeeded"
        );
        assert!(
            !home.exists(),
            "{action} created state before verifying its installation"
        );
    }
}

#[test]
fn missing_runtime_selection_fails_before_local_state_is_created() {
    let root = tempfile::tempdir().unwrap();
    let ambient = root.path().join("ambient.yaml");
    std::fs::write(&ambient, "must not be read or rewritten").unwrap();
    for args in [
        vec!["ls"],
        vec!["--context", "legacy", "ls"],
        vec!["--kubeconfig", "explicit.yaml", "ls"],
    ] {
        let output = cli()
            .current_dir(root.path())
            .env("KUBECONFIG", &ambient)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires both"));
        assert!(!root.path().join(".proofstorm").exists());
    }
    assert_eq!(
        std::fs::read_to_string(ambient).unwrap(),
        "must not be read or rewritten"
    );
}

#[test]
fn operation_listing_uses_recorded_state_and_requires_read_access() {
    use proofstorm_core::Capability;
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("history.sqlite3");
    let store = proofstorm_store::Store::open(&database).unwrap();
    proofstorm_app::developer::configure(&store, "local-cell", "developer").unwrap();
    store
        .reserve_cell("local-cell", "developer", "demo", "fixture")
        .unwrap();
    let read = || {
        cli()
            .arg("--database")
            .arg(&database)
            .arg("--kubeconfig")
            .arg(root.path().join("missing-kubeconfig"))
            .args(["--context", "explicit-test-context"])
            .args(["--json", "ops", "ls", "demo"])
            .output()
            .unwrap()
    };
    store
        .replace_grants(
            "local-cell",
            "developer",
            [Capability::CellStatus, Capability::ExperimentRead],
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
        .replace_grants("local-cell", "developer", [Capability::CellStatus])
        .unwrap();
    assert!(!read().status.success());
    assert!(!root.path().join("missing-kubeconfig").exists());
}

fn recorded_native_operations(
    store: &proofstorm_store::Store,
) -> Vec<(proofstorm_core::CellOperation, &'static str)> {
    use proofstorm_core::{Capability, OperationKind, OperationPhase};
    use serde_json::json;

    proofstorm_app::developer::configure(store, "local-cell", "developer").unwrap();
    let spec = serde_json::from_value(json!({
        "api_version":"proofstorm/v1alpha1", "name":"demo", "links":[],
        "components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core",
            "version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]
    }))
    .unwrap();
    store
        .create_draft("local-cell", "developer", "draft", &spec, "draft")
        .unwrap();
    let revision = store
        .publish("local-cell", "developer", "draft", 1, "publish")
        .unwrap();
    store
        .materialize("local-cell", "developer", "demo", &revision.digest, "apply")
        .unwrap();
    [
        (
            "failed-command",
            OperationPhase::Succeeded,
            json!({"exit_code":1,"exit_signal":null,"timed_out":false,
                "cancelled":false,"cleanup_verified":true,"stderr":"[lncli] FAILED"}),
            "execution completed; command failed (exit 1)",
        ),
        (
            "history-query",
            OperationPhase::Succeeded,
            json!({"exit_code":0,"exit_signal":null,"timed_out":false,
                "cancelled":false,"cleanup_verified":true,"stdout":"{\"status\":\"FAILED\"}"}),
            "execution completed; command succeeded (exit 0)",
        ),
        (
            "lost-receipt",
            OperationPhase::Failed,
            json!({"code":"native_receipt_unavailable"}),
            "execution failed; command outcome unknown",
        ),
    ]
    .into_iter()
    .map(|(id, phase, receipt, summary)| {
        store
            .create_operation(
                "local-cell",
                "developer",
                "demo",
                "",
                "",
                id,
                OperationKind::ComponentExecLive,
                &json!({"component":"chain","argv":["fixture-command"]}),
                id,
                Capability::ComponentExecLive,
            )
            .unwrap();
        let operation = store
            .record_operation_result("local-cell", id, phase, receipt)
            .unwrap();
        (operation, summary)
    })
    .collect()
}

#[test]
fn native_result_headlines_preserve_inspection_exit_status_and_json_receipts() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("history.sqlite3");
    let store = proofstorm_store::Store::open(&database).unwrap();
    let operations = recorded_native_operations(&store);
    let read = |args: &[&str]| {
        cli()
            .arg("--database")
            .arg(&database)
            .arg("--kubeconfig")
            .arg(root.path().join("missing-kubeconfig"))
            .args(["--context", "explicit-test-context"])
            .args(args)
            .output()
            .unwrap()
    };
    let listed = read(&["ops", "ls", "demo"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed = String::from_utf8(listed.stdout).unwrap();
    for (operation, summary) in operations {
        assert!(
            listed
                .lines()
                .any(|line| line == format!("{}  {summary}", operation.id)),
            "{listed}"
        );
        let output = read(&["ops", "show", &operation.id]);
        // Inspecting a failed native command is still a successful read.
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            format!("Operation {}: {summary}", operation.id)
        );
        let machine = read(&["--json", "ops", "show", &operation.id]);
        assert!(machine.status.success());
        assert!(!String::from_utf8_lossy(&machine.stderr).contains("Reading operation result"));
        let value: serde_json::Value = serde_json::from_slice(&machine.stdout).unwrap();
        assert_eq!(value, serde_json::to_value(&operation).unwrap());
    }
    assert!(!root.path().join("missing-kubeconfig").exists());
}

#[test]
fn updater_alias_and_json_refuse_unmanaged_binaries_without_initializing_state() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("custom home");
    for action in ["update", "upgrade"] {
        for check in [false, true] {
            let mut command = cli();
            command.arg("--home").arg(&home).args([action, "--json"]);
            if check {
                command.arg("--check");
            }
            let output = command.output().unwrap();
            assert!(!output.status.success());
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["status"], "failed");
            assert_eq!(result["error"]["code"], "update_unavailable");
            assert!(!output.stdout.contains(&b'\x1b'));
            assert!(!home.exists());
        }
    }
    let help = cli().arg("--help").output().unwrap();
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("update") && text.contains("upgrade"));
}
