//! Tests the real native isolation boundary, not a simulated success receipt.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

fn helper(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_proofstorm-xtask"))
        .args(args)
        .output()
        .unwrap()
}

fn invoke(args: &[&str]) {
    let output = helper(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn prepare(root: &Path, work: &Path, installer: &str) {
    for source in ["source", "snapshot"] {
        fs::create_dir_all(root.join(source)).unwrap();
        fs::write(root.join(source).join("Cargo.toml"), "source sentinel").unwrap();
    }
    // Input staging checks checksums; product archive validation runs in the full bundle job.
    let name = "proofstorm-0.1.0-alpha.1-macos-arm64.tar.gz";
    fs::write(root.join(name), b"policy fixture").unwrap();
    fs::write(
        root.join(format!("{name}.sha256")),
        format!("{:x}  {name}\n", Sha256::digest(b"policy fixture")),
    )
    .unwrap();
    fs::write(root.join("install.sh"), installer).unwrap();
    invoke(&[
        "macos-install",
        "prepare",
        root.join("source").to_str().unwrap(),
        root.join("snapshot").to_str().unwrap(),
        root.join(name).to_str().unwrap(),
        root.join("install.sh").to_str().unwrap(),
        work.to_str().unwrap(),
    ]);
}

#[test]
fn native_policy_blocks_source_network_compilers_and_outside_writes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let work = root.join("isolated work");
    prepare(&root, &work, "#!/bin/sh\nexit 0\n");
    let path = work.to_str().unwrap();
    // No finish receipt is allowed without successful isolation and worker evidence.
    assert!(
        !helper(&["macos-install", "finish", path, "0"])
            .status
            .success()
    );
    invoke(&["macos-install", "isolation", path]);
    let evidence: Value =
        serde_json::from_slice(&fs::read(work.join("isolation.json")).unwrap()).unwrap();
    for key in [
        "source_read_denied",
        "network_denied",
        "compiler_denied",
        "outside_write_denied",
    ] {
        assert_eq!(evidence[key], true, "{key}");
    }
    assert!(
        !helper(&["macos-install", "finish", path, "1"])
            .status
            .success()
    );
    assert!(!work.join("install-smoke-report.json").exists());
    fs::write(work.join("isolation.sb"), "(version 1) (allow default)").unwrap();
    assert!(
        !helper(&["macos-install", "finish", path, "0"])
            .status
            .success()
    );
    assert!(!work.join("install-smoke-report.json").exists());
}

#[test]
fn permissive_policy_is_detected_before_installation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let work = root.join("work");
    prepare(&root, &work, "#!/bin/sh\nexit 0\n");
    fs::write(work.join("isolation.sb"), "(version 1) (allow default)").unwrap();
    assert!(
        !helper(&["macos-install", "isolation", work.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!work.join("isolation.json").exists());
}

#[test]
fn native_worker_reinstalls_fixture_payload_inside_the_verified_policy() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let work = root.join("work with 'quotes'");
    // Only the payload is fake: exercise the real worker, policy, staging and receipt helper.
    let installer = r#"#!/bin/sh
set -eu
case "$1" in
  --version) echo fixture ;;
  --help) : ;;
  release-info|--release-info) echo '{"fixture":true}' ;;
  --artifact-dir)
    [ "$#" = 6 ] && [ "$3" = --archive ] && [ "$5" = --prefix ]
    mkdir -p "$6/bin"
    cp "$0" "$6/bin/proofstorm"
    cp "$0" "$6/bin/proofstorm-mcp"
    chmod 755 "$6/bin/proofstorm" "$6/bin/proofstorm-mcp" ;;
  *) exit 97 ;;
esac
"#;
    prepare(&root, &work, installer);
    let path = work.to_str().unwrap();
    invoke(&["macos-install", "isolation", path]);
    fs::write(
        work.join("worker.sh"),
        include_str!("../../../scripts/macos-install-check.sh"),
    )
    .unwrap();
    let output = Command::new("/usr/bin/sandbox-exec")
        .current_dir(&work)
        .env_clear()
        .env("HOME", work.join("home"))
        .env("TMPDIR", work.join("tmp"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("PROOFSTORM_HOME", work.join("must-not-exist"))
        .arg("-f")
        .arg(work.join("isolation.sb"))
        .arg("/bin/bash")
        .arg(work.join("worker.sh"))
        .arg(&work)
        .arg("proofstorm-0.1.0-alpha.1-macos-arm64.tar.gz")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    invoke(&["macos-install", "finish", path, "0"]);
    let report: Value =
        serde_json::from_slice(&fs::read(work.join("install-smoke-report.json")).unwrap()).unwrap();
    assert_eq!(report["reinstall"], true);
    assert_eq!(report["runtime_tested"], false);
    assert!(
        !helper(&["macos-install", "finish", path, "0"])
            .status
            .success(),
        "receipt was overwritten"
    );
}
