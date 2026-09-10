use super::*;

fn source() -> tempfile::TempDir {
    let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace.package]\nversion = '0.1.0-alpha.1'\n",
    )
    .unwrap();
    fs::write(root.path().join(".gitignore"), ".env\n.tools/\n").unwrap();
    fs::write(root.path().join(".env"), "private").unwrap();
    fs::create_dir(root.path().join("tools")).unwrap();
    fs::write(
        root.path().join("tools/versions.env"),
        "TRUNK_VERSION=fixture\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join(".tools/bin")).unwrap();
    let trunk = root.path().join(".tools/bin/trunk");
    fs::write(&trunk, "#!/bin/sh\nprintf 'trunk fixture\\n'\n").unwrap();
    fs::set_permissions(trunk, fs::Permissions::from_mode(0o755)).unwrap();
    git(root.path(), &["init", "-q"]).unwrap();
    git(root.path(), &["add", "."]).unwrap();
    git(
        root.path(),
        &[
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "user.name=Fixture",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        ],
    )
    .unwrap();
    root
}

fn plan_args(source: &Path, work: &Path, output: &Path) -> Vec<OsString> {
    vec![
        source.into(),
        work.into(),
        output.into(),
        "".into(),
        "".into(),
        "".into(),
        "false".into(),
        "true".into(),
    ]
}

#[test]
fn snapshots_exclude_ignored_files_preserve_provenance_and_detect_transport_changes() {
    let source = source();
    let work = tempfile::tempdir().unwrap();
    let destination = work.path().canonicalize().unwrap().join("source");
    let receipt = snapshot(source.path(), &destination, false, None).unwrap();
    assert_eq!(receipt["dirty"], false);
    assert!(!destination.join(".env").exists());
    assert!(!destination.join(".tools").exists());
    let copied = snapshot(
        &destination,
        &work.path().canonicalize().unwrap().join("second"),
        false,
        Some(&receipt),
    )
    .unwrap();
    assert_eq!(copied, receipt);
    fs::write(destination.join("unexpected"), "extra").unwrap();
    assert!(
        snapshot(
            &destination,
            &work.path().join("third"),
            true,
            Some(&receipt)
        )
        .unwrap_err()
        .to_string()
        .contains("checksum mismatch")
    );
    assert!(!work.path().join("third").exists());
}

#[test]
fn dirty_and_symlinked_sources_fail_closed() {
    let source = source();
    fs::write(source.path().join("untracked"), "new").unwrap();
    let work = tempfile::tempdir().unwrap();
    assert!(snapshot(source.path(), &work.path().join("clean"), false, None).is_err());
    let allowed = snapshot(
        source.path(),
        &work.path().canonicalize().unwrap().join("dirty"),
        true,
        None,
    )
    .unwrap();
    assert_eq!(allowed["dirty"], true);
    std::os::unix::fs::symlink(source.path().join(".env"), source.path().join("link")).unwrap();
    assert!(snapshot(source.path(), &work.path().join("linked"), true, None).is_err());
    assert!(!work.path().join("linked").exists());
}

#[test]
fn build_plan_records_safe_paths_and_accepts_an_explicit_external_cache() {
    let source = source();
    let external = tempfile::tempdir().unwrap();
    let work = external.path().join("work with 'quotes'");
    let cache = external.path().join("cache");
    let mut args = plan_args(source.path(), &work, &external.path().join("artifacts"));
    args[3] = cache.as_os_str().to_owned();
    let plan = prepare(&args).unwrap();
    assert_eq!(plan.len(), 8);
    assert_eq!(plan[3], output_path(&cache).unwrap().to_str().unwrap());
    assert_eq!(plan[7], host_target().unwrap());
    assert!(work.join("source.json").is_file());
    assert!(work.join("build-plan.json").is_file());
    assert!(!source.path().join("target").exists());
    assert!(
        prepare(&args)
            .unwrap_err()
            .to_string()
            .contains("already exist")
    );
}

#[test]
fn invalid_outputs_tool_pins_and_stable_debug_builds_do_not_create_work() {
    let source = source();
    let external = tempfile::tempdir().unwrap();
    let work = external.path().join("work");
    let output = external.path().join("out");
    for index in [1, 2, 3] {
        let mut args = plan_args(source.path(), &work, &output);
        args[index] = source.path().join("forbidden").into_os_string();
        assert!(prepare(&args).is_err());
        assert!(!source.path().join("forbidden").exists());
        assert!(!work.exists());
    }
    fs::write(
        source.path().join("tools/versions.env"),
        "TRUNK_VERSION=wrong\n",
    )
    .unwrap();
    let args = plan_args(source.path(), &work, &output);
    assert!(
        prepare(&args)
            .unwrap_err()
            .to_string()
            .contains("Trunk version")
    );
    assert!(!work.exists());
    fs::write(
        source.path().join("Cargo.toml"),
        "[workspace.package]\nversion = '0.1.0'\n",
    )
    .unwrap();
    assert!(
        prepare(&args)
            .unwrap_err()
            .to_string()
            .contains("debug binaries")
    );
    assert!(!work.exists());
}
