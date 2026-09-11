use super::*;
use std::os::unix::fs::symlink;

fn inputs(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let archive = root.join("proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz");
    fs::write(&archive, b"fixture archive").unwrap();
    let digest = checksum(&archive, 1024).unwrap();
    fs::write(
        root.join(format!(
            "{}.sha256",
            archive.file_name().unwrap().to_str().unwrap()
        )),
        format!(
            "{digest}  {}\n",
            archive.file_name().unwrap().to_str().unwrap()
        ),
    )
    .unwrap();
    let installer = root.join("install.sh");
    fs::write(&installer, "#!/bin/sh\nexit 0\n").unwrap();
    (archive, installer)
}

#[test]
fn friendly_and_legacy_input_names_keep_strict_platform_and_checksum_checks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (_, installer) = inputs(&root);
    for (target, platform, other) in [
        (
            "x86_64-unknown-linux-gnu",
            "linux-amd64",
            "aarch64-apple-darwin",
        ),
        (
            "aarch64-apple-darwin",
            "macos-arm64",
            "x86_64-unknown-linux-gnu",
        ),
    ] {
        for suffix in [target, platform] {
            let name = format!("proofstorm-0.1.0-alpha.1-{suffix}.tar.gz");
            let archive = root.join(&name);
            fs::write(&archive, "fixture").unwrap();
            let digest = checksum(&archive, 1024).unwrap();
            let receipt = root.join(format!("{name}.sha256"));
            fs::write(&receipt, format!("{digest}  {name}\n")).unwrap();
            input_digests_for(&archive, &installer, target).unwrap();
            assert!(input_digests_for(&archive, &installer, other).is_err());
            fs::write(&receipt, format!("{digest}  different-name.tar.gz\n")).unwrap();
            assert!(input_digests_for(&archive, &installer, target).is_err());
        }
    }
}

#[test]
fn staged_inputs_are_public_bounded_and_receipt_requires_success() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("checkout");
    fs::create_dir(&source).unwrap();
    let (archive, installer) = inputs(&root);
    let work = root.join("quoted 'work' directory");
    let plan = prepare(&source, &archive, &installer, &work, true).unwrap();
    assert_eq!(plan.len(), 4);
    assert!(
        plan[1]
            .to_str()
            .unwrap()
            .starts_with("proofstorm-linux-install-")
    );
    assert!(
        plan[2]
            .to_str()
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-:".contains(&b))
    );
    assert_eq!(fs::read_dir(work.join("input")).unwrap().count(), 3);
    for entry in fs::read_dir(work.join("input")).unwrap() {
        assert_eq!(
            entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
    let run = bundle::read_json(&work.join("run.json")).unwrap();
    assert_eq!(run["host_mounts"], json!([]));
    assert_eq!(run["network"], "none");
    assert_eq!(run["privileged"], false);
    assert!(
        fs::read_to_string(work.join("Dockerfile"))
            .unwrap()
            .contains(IMAGE)
    );
    assert_eq!(
        fs::read_to_string(work.join(".dockerignore")).unwrap(),
        "*\n!Dockerfile\n!input/\n!input/**\n"
    );
    for status in ["1", "", "0\n1", "running"] {
        assert!(finish(&work, status).is_err());
        assert!(!work.join("install-smoke-report.json").exists());
    }
    finish(&work, "0").unwrap();
    let report = bundle::read_json(&work.join("install-smoke-report.json")).unwrap();
    assert_eq!(report["development_override"], true);
    assert_eq!(report["runtime_tested"], false);
    assert_eq!(report["github_download_tested"], false);
    assert!(
        finish(&work, "0").is_err(),
        "never replace an existing report"
    );
}

#[test]
fn rejects_untrusted_inputs_and_unsafe_destinations_before_staging() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("checkout");
    fs::create_dir(&source).unwrap();
    let (archive, installer) = inputs(&root);
    let work = root.join("work");
    assert!(prepare(&source, &archive, &installer, &source.join("work"), false).is_err());
    assert!(prepare(&source, &archive, &installer, &root, false).is_err());
    symlink(&source, &work).unwrap();
    assert!(prepare(&source, &archive, &installer, &work, false).is_err());
    fs::remove_file(&work).unwrap();
    fs::write(&archive, b"tampered").unwrap();
    assert!(prepare(&source, &archive, &installer, &work, false).is_err());
    assert!(!work.exists());
    let (archive, installer) = inputs(&root);
    fs::remove_file(&installer).unwrap();
    symlink(&archive, &installer).unwrap();
    assert!(prepare(&source, &archive, &installer, &work, false).is_err());
    assert!(!work.exists());
}

#[test]
fn input_tampering_after_staging_cannot_produce_success_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("checkout");
    fs::create_dir(&source).unwrap();
    let (archive, installer) = inputs(&root);
    let work = root.join("work");
    prepare(&source, &archive, &installer, &work, false).unwrap();
    fs::write(work.join("input/install.sh"), "modified").unwrap();
    assert!(finish(&work, "0").is_err());
    assert!(!work.join("install-smoke-report.json").exists());
}

#[test]
fn subprocess_deadlines_and_failures_are_not_successes() {
    assert!(bounded(&["1".into(), "sh".into(), "-c".into(), "exit 0".into()]).is_ok());
    assert!(bounded(&["1".into(), "sh".into(), "-c".into(), "exit 23".into()]).is_err());
    let before = Instant::now();
    let error = bounded(&["1".into(), "sleep".into(), "30".into()]).unwrap_err();
    assert!(error.to_string().contains("deadline"));
    assert!(before.elapsed() < Duration::from_secs(5));
    assert!(bounded(&["0".into(), "true".into()]).is_err());
}
