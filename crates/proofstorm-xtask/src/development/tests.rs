use super::*;
use std::os::unix::fs::symlink;

fn fixture() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn controller_hash_uses_the_existing_nul_delimited_contract() {
    assert_eq!(
        tree_sha(&BTreeMap::from([("a".into(), "abc".into())])),
        "afa31db071dbac213a1f8e8551bccdfc1ef0b4e66e5744a856566754b050317a"
    );
}

#[test]
fn linked_owner_settings_and_chart_trees_are_refused() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap();
    prepare(&source, None).unwrap();
    let work = source.join(".proofstorm-dev");
    for file in ["owner.json", "build.json"] {
        let original = work.join(file);
        let saved = source.join(file);
        fs::rename(&original, &saved).unwrap();
        symlink(&saved, &original).unwrap();
        assert!(prepare(&source, None).is_err());
        fs::remove_file(&original).unwrap();
        fs::rename(&saved, &original).unwrap();
    }
    let chart = source.join("chart");
    directory(&chart).unwrap();
    symlink(work.join("owner.json"), chart.join("secret.yaml")).unwrap();
    assert!(copy_tree(&chart, &source.join("copied-chart")).is_err());
    assert!(!source.join("copied-chart").exists());
}

#[test]
fn ownership_and_persisted_cache_are_compatible_and_fail_closed() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap();
    let cache = source.join("separate cache");
    assert_eq!(prepare(&source, Some(&cache)).unwrap(), cache);
    assert_eq!(prepare(&source, None).unwrap(), cache);
    let work = source.join(".proofstorm-dev");
    assert_eq!(
        read_json(&work.join("owner.json")).unwrap(),
        json!({"source":source})
    );
    fs::write(
        work.join("build.json"),
        serde_json::to_vec(&json!({"target":source.join("target/debug")})).unwrap(),
    )
    .unwrap();
    assert!(
        prepare(&source, None)
            .unwrap_err()
            .to_string()
            .contains("legacy checkout target")
    );
    fs::write(work.join("owner.json"), r#"{"source":"/foreign"}"#).unwrap();
    assert!(prepare(&source, None).is_err());
}

#[test]
fn unowned_and_linked_outputs_are_refused_without_overwrite() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap();
    fs::create_dir(source.join(".proofstorm-dev")).unwrap();
    assert!(
        prepare(&source, None)
            .unwrap_err()
            .to_string()
            .contains("unowned")
    );
    let output = source.join("original");
    write_owned(&output, b"original", 0o600).unwrap();
    let link = source.join("link");
    symlink(&output, &link).unwrap();
    assert!(write_owned(&link, b"replacement", 0o600).is_err());
    assert!(inventory(&source).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"original");
    assert_eq!(
        fs::metadata(output).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn existing_python_owner_and_settings_keep_the_same_target() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap();
    let work = source.join(".proofstorm-dev");
    directory(&work).unwrap();
    let cache = source.join("python-selected-target");
    fs::write(
        work.join("owner.json"),
        format!("{{\"source\": {}}}", json!(source)),
    )
    .unwrap();
    fs::write(
        work.join("build.json"),
        format!("{{\"target\": {}}}", json!(cache)),
    )
    .unwrap();
    assert_eq!(prepare(&source, None).unwrap(), cache);
    symlink(&cache, source.join("alias")).unwrap();
    fs::write(
        work.join("build.json"),
        serde_json::to_vec(&json!({"target":source.join("alias")})).unwrap(),
    )
    .unwrap();
    assert!(prepare(&source, None).is_err());
}

#[test]
fn controller_snapshot_excludes_host_source_and_local_state() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap().join("source");
    let names = [
        "Cargo.toml",
        "Cargo.lock",
        "Dockerfile.proofstormd",
        "crates/proofstormd/Cargo.toml",
        "crates/proofstormd/src/main.rs",
        "crates/proofstorm-web/Cargo.toml",
        "crates/proofstorm-web/src/lib.rs",
        "crates/proofstorm-xtask/Cargo.toml",
        "crates/proofstorm-xtask/src/main.rs",
        ".env",
        ".proofstorm-dev/state/private.json",
        ".cargo/config.toml",
    ];
    for name in names {
        write_owned(&source.join(name), b"fixture", 0o600).unwrap();
    }
    let first_dir = source.parent().unwrap().join("first");
    let first = controller_snapshot(&source, &first_dir, &names).unwrap();
    assert!(!first_dir.join(".env").exists());
    assert!(!first_dir.join(".proofstorm-dev").exists());
    assert!(!first_dir.join(".cargo").exists());
    assert_eq!(
        fs::read_to_string(first_dir.join("crates/proofstorm-web/src/lib.rs")).unwrap(),
        "// Unbuilt workspace member.\n"
    );
    for host in ["proofstorm-web", "proofstorm-xtask"] {
        write_owned(
            &source.join(format!("crates/{host}/src/main.rs")),
            b"new host source",
            0o600,
        )
        .unwrap();
    }
    let second =
        controller_snapshot(&source, &source.parent().unwrap().join("second"), &names).unwrap();
    assert_eq!(first, second);
    fs::write(
        source.join("crates/proofstormd/src/main.rs"),
        "new controller",
    )
    .unwrap();
    assert_ne!(
        first,
        controller_snapshot(&source, &source.parent().unwrap().join("third"), &names).unwrap()
    );
}

#[test]
fn controller_source_refuses_links_and_unsafe_paths() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap();
    symlink("missing-secret", source.join("Cargo.toml")).unwrap();
    assert!(controller_snapshot(&source, &source.join("one"), &["Cargo.toml"]).is_err());
    assert!(controller_snapshot(&source, &source.join("two"), &["../Cargo.toml"]).is_err());
    assert!(controller_snapshot(&source, &source.join("three"), &["/Cargo.toml"]).is_err());
}

#[test]
fn content_addresses_reuse_unchanged_snapshots_and_refuse_tampering() {
    let dir = fixture();
    let source = dir.path().canonicalize().unwrap();
    let resources = source.join("resources");
    directory(&resources).unwrap();
    let stage = source.join("stage");
    write_owned(&stage.join("Chart.yaml"), b"first", 0o600).unwrap();
    let destination = publish(&stage, &resources).unwrap();
    write_owned(&stage.join("Chart.yaml"), b"first", 0o600).unwrap();
    assert_eq!(publish(&stage, &resources).unwrap(), destination);
    fs::write(destination.join("Chart.yaml"), "tampered").unwrap();
    assert!(publish(&stage, &resources).is_err());
    fs::write(stage.join("Chart.yaml"), "second").unwrap();
    assert_ne!(publish(&stage, &resources).unwrap(), destination);
}

#[test]
fn launchers_quote_paths_preserve_arguments_and_refuse_foreign_files() {
    let dir = tempfile::Builder::new()
        .prefix("proofstorm's checkout ")
        .tempdir()
        .unwrap();
    let source = dir.path().canonicalize().unwrap();
    let cache = prepare(&source, None).unwrap();
    write_owned(
        &cache.join("debug/proofstorm"),
        b"#!/bin/sh\nprintf '%s\\n' \"$PROOFSTORM_HOME\" \"$@\"\n",
        0o700,
    )
    .unwrap();
    launchers(&source).unwrap();
    let launcher = source.join(".proofstorm-dev/bin/proofstorm");
    let result = Command::new(&launcher)
        .args(["open", "a directory's name"])
        .env("PROOFSTORM_HOME", "/foreign")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        format!(
            "{}\nopen\na directory's name\n",
            source.join(".proofstorm-dev/state").display()
        )
    );
    fs::write(&launcher, "foreign").unwrap();
    assert!(launchers(&source).is_err());
    assert_eq!(fs::read_to_string(launcher).unwrap(), "foreign");
}

#[test]
fn ordinary_shell_exit_is_success_but_signals_remain_visible() {
    for status in [0, 1, 130] {
        assert_eq!(shell_exit(ExitStatus::from_raw(status << 8)), 0);
    }
    assert_eq!(shell_exit(ExitStatus::from_raw(15)), 143);
    for name in [
        "PROOFSTORM_HOME",
        "PROOFSTORM_DB",
        "TRUNK_BUILD_DIST",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
    ] {
        assert!(scrubbed(name));
    }
    assert!(!scrubbed("PATH"));
}
