use super::*;

#[test]
fn linux_build_plan_exports_only_verified_sources_and_a_dockerfile_context() {
    let source = source();
    let recipe = source
        .path()
        .join("docker/release/Dockerfile.linux-builder");
    fs::create_dir_all(recipe.parent().unwrap()).unwrap();
    fs::write(&recipe, "FROM fixture\n").unwrap();
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let work = temp.path().join("work with 'quotes'");
    let plan = linux_prepare(source.path(), &work, false, true).unwrap();
    assert_eq!(
        fs::metadata(&work).unwrap().permissions().mode() & 0o777,
        0o700,
        "transport stays private behind the host work directory"
    );
    for path in [
        "input",
        "input/source",
        "input/source/docker",
        "input/source/docker/release",
    ] {
        assert_eq!(
            fs::metadata(work.join(path)).unwrap().permissions().mode() & 0o777,
            0o755,
            "container must be able to traverse {path} without owner privileges"
        );
    }
    for path in [
        "input/source.json",
        "input/options.json",
        "input/source/Cargo.toml",
    ] {
        assert_eq!(
            fs::metadata(work.join(path)).unwrap().permissions().mode() & 0o777,
            0o644,
            "container must be able to read {path} without owner privileges"
        );
    }
    assert_eq!(plan.len(), 3);
    assert!(plan[1].starts_with("proofstorm-linux-build-"));
    assert!(plan[2].starts_with("proofstorm-linux-builder:"));
    assert!(
        plan[2]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-:".contains(&b))
    );
    assert_eq!(fs::read_dir(work.join("toolchain")).unwrap().count(), 1);
    assert_eq!(
        fs::read(work.join("toolchain/Dockerfile")).unwrap(),
        fs::read(recipe).unwrap()
    );
    assert!(!work.join("input/source/.env").exists());
    assert!(!work.join("input/source/.tools").exists());
    assert!(!work.join("input/source/.git").exists());
    let run = bundle::read_json(&work.join("run.json")).unwrap();
    assert_eq!(run["host_mounts"], json!([]));
    assert_eq!(run["privileged"], false);
    assert_eq!(run["platform"], "linux/amd64");
    assert_eq!(run["source"]["dirty"], true);
    assert_eq!(
        bundle::read_json(&work.join("input/options.json")).unwrap(),
        json!({"development":false,"debug":true})
    );
    // Host transport and container worker use exactly the same fingerprint.
    worker_prepare(
        &work.join("input"),
        &temp.path().join("worker"),
        &temp.path().join("artifacts"),
    )
    .unwrap();
    assert!(linux_prepare(source.path(), &work, true, true).is_err());
    assert!(linux_prepare(source.path(), &source.path().join("forbidden"), true, true).is_err());
}

#[test]
fn linux_build_plan_refuses_uncommitted_stable_sources_and_linked_files() {
    let source = source();
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    fs::write(
        source.path().join("Cargo.toml"),
        "[workspace.package]\nversion = '0.1.0'\n",
    )
    .unwrap();
    let work = temp.path().join("work");
    assert!(linux_prepare(source.path(), &work, false, true).is_err());
    assert!(linux_prepare(source.path(), &work, false, false).is_err());
    assert!(!work.exists());
    std::os::unix::fs::symlink(
        source.path().join("Cargo.toml"),
        source.path().join("linked"),
    )
    .unwrap();
    assert!(linux_prepare(source.path(), &work, true, true).is_err());
    assert!(!work.exists());
}

#[test]
fn transport_permissions_preserve_fingerprints_and_do_not_follow_links() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let transport = temp.path().join("input");
    directory(&transport.join("source/scripts")).unwrap();
    let script = transport.join("source/scripts/worker.sh");
    let receipt = transport.join("source.json");
    fs::write(&script, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&receipt, "{}").unwrap();
    fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
    let names = source_names(&transport, true).unwrap();
    let before = fingerprint(&transport, &names).unwrap();
    transport_permissions(&transport).unwrap();
    assert_eq!(fingerprint(&transport, &names).unwrap(), before);
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
        0o644
    );
    let outside = temp.path().join("private");
    fs::write(&outside, "private").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&outside, transport.join("linked")).unwrap();
    assert!(transport_permissions(&transport).is_err());
    assert_eq!(
        fs::metadata(outside).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn worker_owns_a_verified_copy_and_keeps_transport_pristine() {
    let checkout = source();
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let input = temp.path().join("input");
    fs::create_dir(&input).unwrap();
    let receipt = snapshot(checkout.path(), &input.join("source"), true, None).unwrap();
    fs::write(input.join("source.json"), receipt.to_string()).unwrap();
    fs::write(
        input.join("options.json"),
        r#"{"development":false,"debug":true}"#,
    )
    .unwrap();
    let work = temp.path().join("work");
    let output = temp.path().join("artifacts");
    let plan = worker_prepare(&input, &work, &output).unwrap();
    assert_eq!(&plan[2..], ["false", "true"]);
    assert!(!output.exists());
    fs::create_dir(work.join("source/.tools")).unwrap();
    fs::write(work.join("source/.tools/download"), "fixture").unwrap();
    assert!(!input.join("source/.tools").exists());
    assert_eq!(
        fingerprint(
            &input.join("source"),
            &source_names(&input.join("source"), true).unwrap()
        )
        .unwrap(),
        receipt["sha256"]
    );
    assert!(worker_prepare(&input, &work, &output).is_err());
    assert!(worker_prepare(&input, &input.join("bad"), &output).is_err());
    assert!(worker_prepare(&input, &temp.path().join("other"), &input.join("bad")).is_err());
    fs::write(input.join("source/Cargo.toml"), "tampered").unwrap();
    assert!(worker_prepare(&input, &temp.path().join("tampered"), &output).is_err());
    assert!(!temp.path().join("tampered").exists());
}

#[test]
fn worker_options_are_typed_and_new_output_is_required() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let input = temp.path().join("input");
    fs::create_dir(&input).unwrap();
    let work = temp.path().join("work");
    let output = temp.path().join("output");
    for options in [
        r#"{"development":"false","debug":true}"#,
        r#"{"development":true}"#,
        "{}",
    ] {
        fs::write(input.join("options.json"), options).unwrap();
        assert!(worker_prepare(&input, &work, &output).is_err());
        assert!(!work.exists());
    }
    fs::create_dir(&output).unwrap();
    assert!(worker_prepare(&input, &work, &output).is_err());
}

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
