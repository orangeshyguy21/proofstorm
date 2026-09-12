use proofstorm_app::installation::Installation;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    installation: Installation,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root
            .path()
            .canonicalize()
            .unwrap()
            .join("checkout 'with spaces'");
        let home = source.join(".proofstorm-dev/state");
        fs::create_dir_all(source.join("crates/proofstorm-app")).unwrap();
        fs::write(source.join("crates/proofstorm-app/Cargo.toml"), "fixture").unwrap();
        fs::create_dir(source.join(".proofstorm-dev")).unwrap();
        fs::write(
            source.join(".proofstorm-dev/owner.json"),
            json!({"source":source}).to_string(),
        )
        .unwrap();
        let installation = Installation::initialize(&home, None, None).unwrap();
        let record = home.join("checkout-artifacts.json");
        fs::write(
            &record,
            json!({"format_version":1,"installation_id":installation.id,
            "source":source,"resources":source.join("resources"),"web_dist":source.join("web"),
            "cli":source.join("target/proofstorm"),"mcp":source.join("target/proofstorm-mcp"),
            "cli_sha256":"stale","mcp_sha256":"stale","files":{},"metadata":{}})
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(record, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(installation.database(), "old state").unwrap();
        let bin = source.join("fake-bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("docker"), "#!/bin/sh\n[ \"${FAKE_DOCKER_FAIL:-0}\" = 0 ] || exit 3\ncase \"$*\" in\n 'info --format {{.ID}}') echo fixture-daemon;;\n 'ps '*|'network ls '*|'volume ls '*) :;;\n *) echo unexpected-docker-command >&2; exit 97;;\nesac\n").unwrap();
        fs::set_permissions(bin.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _root: root,
            source,
            home,
            bin,
            installation,
        }
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_proofstorm"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("PROOFSTORM_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(&self.source)
            .arg("--home")
            .arg(&self.home)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            );
        command
    }
}

#[test]
fn noninteractive_reset_requires_confirmation_and_refuses_runtime_overrides() {
    let fixture = Fixture::new();
    for args in [
        vec!["dev", "reset", "--json"],
        vec!["--database", "/foreign", "dev", "reset", "--yes"],
    ] {
        let output = fixture.cli().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            !fixture
                .source
                .join(".proofstorm-dev/reset-pending.json")
                .exists()
        );
        assert_eq!(
            fs::read_to_string(fixture.installation.database()).unwrap(),
            "old state"
        );
    }
}

#[test]
fn failed_reset_preserves_state_and_explicit_retry_finishes_with_clean_json() {
    let fixture = Fixture::new();
    let output = fixture
        .cli()
        .args(["dev", "reset", "--yes", "--json"])
        .env("FAKE_DOCKER_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(fixture.installation.database()).unwrap(),
        "old state"
    );
    assert!(
        fixture
            .source
            .join(".proofstorm-dev/reset-pending.json")
            .exists()
    );
    let output = fixture
        .cli()
        .args(["dev", "reset", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["reset"], true);
    assert_eq!(result["runtime_started"], false);
    assert_eq!(result["previous_installation_id"], fixture.installation.id);
    assert_ne!(result["installation_id"], fixture.installation.id);
    assert!(!fixture.installation.database().exists());
    assert!(
        !fixture
            .source
            .join(".proofstorm-dev/reset-pending.json")
            .exists()
    );
    assert!(
        PathBuf::from(result["diagnostics"].as_str().unwrap())
            .join("proofstorm.sqlite3")
            .is_file()
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
