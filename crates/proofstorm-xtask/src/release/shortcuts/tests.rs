use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for directory in ["crates/app", "tools", "charts/proofstorm", "release"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    for (file, contents) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/app\"]\n[workspace.package]\nversion = \"0.1.0-alpha.1\"\n",
        ),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"proofstorm-app\"\nversion.workspace = true\n",
        ),
        (
            "Cargo.lock",
            "version = 4\n\n[[package]]\nname = \"proofstorm-app\"\nversion = \"0.1.0-alpha.1\"\n\n[[package]]\nname = \"other\"\nversion = \"0.1.0-alpha.1\"\nsource = \"registry+https://example.invalid\"\ndependencies = [\"proofstorm-app 0.1.0-alpha.1\"]\n",
        ),
        (
            "install.sh",
            "#!/bin/sh\ninstall_version=\"0.1.0-alpha.1\"\n",
        ),
        (
            "tools/versions.env",
            "OTHER_VERSION=0.1.0-alpha.1\nPROOFSTORM_VERSION=0.1.0-alpha.1\n",
        ),
        (
            "charts/proofstorm/Chart.yaml",
            "version: 0.1.0-alpha.1\nappVersion: 0.1.0-alpha.1\n",
        ),
        (
            "charts/proofstorm/values.yaml",
            "image:\n  tag: 0.1.0-alpha.1\n  digest: pinned\n",
        ),
        (
            "release/controller.json",
            "{\"metadata\":{\"version\":\"0.1.0-alpha.1\"},\"image\":\"do-not-change\"}",
        ),
    ] {
        fs::write(root.join(file), contents).unwrap();
    }
    fs::set_permissions(root.join("install.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    temp
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn version_preparation_updates_only_source_identity_and_preserves_image_evidence() {
    let fixture = fixture();
    let root = fixture.path().canonicalize().unwrap();
    let receipt = fs::read(root.join("release/controller.json")).unwrap();
    let edits = changes(&root, "0.1.0-alpha.2").unwrap();
    assert_eq!(edits.len(), 6);
    prepare(&root, "0.1.0-alpha.2").unwrap();
    assert_eq!(workspace(&root).unwrap().0, "0.1.0-alpha.2");
    changes(&root, "0.1.0-alpha.2").unwrap();
    let lock: toml::Value = toml::from_str(&read(&root.join("Cargo.lock")).unwrap()).unwrap();
    assert_eq!(
        lock["package"][0]["version"].as_str(),
        Some("0.1.0-alpha.2")
    );
    assert_eq!(
        lock["package"][1]["version"].as_str(),
        Some("0.1.0-alpha.1")
    );
    assert_eq!(
        lock["package"][1]["dependencies"][0].as_str(),
        Some("proofstorm-app 0.1.0-alpha.2")
    );
    assert_eq!(
        fs::metadata(root.join("install.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::read(root.join("release/controller.json")).unwrap(),
        receipt
    );
    assert!(
        read(&root.join("tools/versions.env"))
            .unwrap()
            .contains("OTHER_VERSION=0.1.0-alpha.1")
    );
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 6);
}

#[test]
fn dirty_checkout_bad_versions_and_inconsistent_inputs_never_get_rewritten() {
    let fixture = fixture();
    let root = fixture.path().canonicalize().unwrap();
    let before = read(&root.join("Cargo.toml")).unwrap();
    for version in [
        "0.1.0-alpha.1",
        "0.1.0-alpha.0",
        "1.0.0",
        "v0.1.0-alpha.2",
        "0.01.0-alpha.2",
        "0.1.0-alpha.2;echo x",
    ] {
        assert!(prepare(&root, version).is_err(), "accepted {version}");
    }
    fs::write(root.join("notes.txt"), "user work").unwrap();
    assert!(prepare(&root, "0.1.0-alpha.2").is_err());
    fs::remove_file(root.join("notes.txt")).unwrap();
    for name in ["install.sh", "Cargo.lock", "charts/proofstorm/Chart.yaml"] {
        let original = read(&root.join(name)).unwrap();
        fs::write(
            root.join(name),
            original.replace("0.1.0-alpha.1", "0.1.0-alpha.0"),
        )
        .unwrap();
        assert!(changes(&root, "0.1.0-alpha.2").is_err());
        fs::write(root.join(name), original).unwrap();
    }
    assert_eq!(read(&root.join("Cargo.toml")).unwrap(), before);
    assert!(parts("0.1.0-alpha.10").unwrap() > parts("0.1.0-alpha.9").unwrap());
}

#[test]
fn linked_version_files_and_ambiguous_fields_are_refused() {
    let fixture = fixture();
    let root = fixture.path().canonicalize().unwrap();
    fs::rename(root.join("install.sh"), root.join("saved-installer")).unwrap();
    symlink(root.join("saved-installer"), root.join("install.sh")).unwrap();
    assert!(changes(&root, "0.1.0-alpha.2").is_err());
    assert!(
        replace_line(
            "version: old\nversion: old\n",
            "version: old",
            "version: new"
        )
        .is_err()
    );
    assert!(replace_line("missing\n", "version: old", "version: new").is_err());
}

#[test]
fn release_selection_never_silently_falls_back_to_old_or_failed_runs() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().canonicalize().unwrap().join("runs.json");
    let run = |id, status, conclusion| json!({"id":id,"status":status,"conclusion":conclusion,"repository":{"full_name":"owner/repo"},"head_sha":"a".repeat(40),"head_branch":"main","event":"push"});
    for (value, expected) in [
        (json!([{"workflow_runs":[]}]), None),
        (
            json!([{"workflow_runs":[run(42,"completed","success")]}]),
            Some(42),
        ),
        (
            json!([{"workflow_runs":[run(42,"completed","success")]},{"workflow_runs":[run(43,"completed","success")]}]),
            Some(43),
        ),
        (
            json!([{"workflow_runs":[run(42,"completed","success"),run(43,"in_progress","")]}]),
            None,
        ),
        (
            json!([{"workflow_runs":[run(42,"completed","success"),run(43,"completed","failure")]}]),
            None,
        ),
        (json!({"message":"unauthorized"}), None),
    ] {
        fs::write(&path, value.to_string()).unwrap();
        assert_eq!(
            select_run(&path, "owner/repo", &"a".repeat(40)).ok(),
            expected
        );
    }
    fs::write(
        &path,
        json!([{"workflow_runs":[run(42,"completed","success")]}]).to_string(),
    )
    .unwrap();
    assert!(select_run(&path, "other/repo", &"a".repeat(40)).is_err());
    assert!(select_run(&path, "owner/repo", &"b".repeat(40)).is_err());
}
