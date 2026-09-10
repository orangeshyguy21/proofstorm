use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture(root: &Path) -> PathBuf {
    let bundle = root.join("bundle");
    fs::create_dir(&bundle).unwrap();
    for name in REQUIRED {
        let path = bundle.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"fixture").unwrap();
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if name.starts_with("bin/") {
                0o755
            } else {
                0o644
            }),
        )
        .unwrap();
    }
    let info = crate::release::describe();
    fs::write(
        bundle.join("release-info.json"),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    let mut files = serde_json::Map::new();
    for name in REQUIRED {
        let path = bundle.join(name);
        files.insert((*name).into(), json!({"sha256":hash(&path).unwrap(),"size":fs::metadata(&path).unwrap().len(),"mode":fs::metadata(path).unwrap().permissions().mode() & 0o777}));
    }
    fs::write(bundle.join("manifest.json"), serde_json::to_vec(&json!({
        "format_version":1,"target":crate::platform::target(),"channel":"development","release_ready":false,
        "version":info["version"],"build_profile":info["build_profile"],"source":{"revision":info["source_revision"],"sha256":info["source_sha256"],"dirty":true},"files":files
    })).unwrap()).unwrap();
    bundle
}

#[test]
fn alpha_installs_reinstalls_and_permits_its_controller_without_override() {
    let root = tempfile::tempdir().unwrap();
    let bundle = fixture(root.path());
    let path = bundle.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["channel"] = json!("alpha");
    manifest["controller"] = crate::release::describe()["controller"].clone();
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let prefix = root.path().join("prefix");
    let first = install(&bundle, &prefix, false).unwrap();
    assert_eq!(first, install(&bundle, &prefix, false).unwrap());
    assert!(!prefix.join("lib/proofstorm/state").exists());
    assert!(crate::artifacts::verify(&root.path().join("home"), &bundle, false).unwrap());
    fs::write(bundle.join("LICENSE"), b"tampered").unwrap();
    assert!(install(&bundle, &prefix, false).is_err());
}

#[test]
fn alpha_never_waives_controller_compatibility_or_stable_release_gates() {
    let info = crate::release::describe();
    let manifest = json!({"release_ready":false,"controller":info["controller"]});
    validate_alpha(&manifest, &info).unwrap();
    for version in ["0.1.0", "0.1.0-alpha.", "0.1-alpha.1", "0.1.0-alpha.1-dev"] {
        let mut changed = info.clone();
        changed["version"] = json!(version);
        assert!(validate_alpha(&manifest, &changed).is_err());
    }
    for key in ["image", "platform", "metadata"] {
        let mut changed = info.clone();
        changed["controller"][key] = json!("wrong");
        let altered_manifest = json!({"release_ready":false,"controller":changed["controller"]});
        assert!(validate_alpha(&altered_manifest, &changed).is_err());
    }
    let mut changed = manifest.clone();
    changed["controller"] = Value::Null;
    assert!(validate_alpha(&changed, &info).is_err());
    changed = manifest;
    changed["release_ready"] = json!(true);
    assert!(validate_alpha(&changed, &info).is_err());
}

#[test]
fn foreign_target_is_refused_even_in_development_mode() {
    let root = tempfile::tempdir().unwrap();
    let bundle = fixture(root.path());
    let path = bundle.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["target"] = json!(if crate::platform::target() == crate::platform::MAC_ARM64 {
        crate::platform::LINUX_AMD64
    } else {
        crate::platform::MAC_ARM64
    });
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let prefix = root.path().join("must-not-exist");
    assert!(
        install(&bundle, &prefix, true)
            .unwrap_err()
            .to_string()
            .contains("platform")
    );
    assert!(!prefix.exists());
}

#[test]
fn installation_is_repeatable_and_preserves_previous_version_on_bad_update() {
    let root = tempfile::tempdir().unwrap();
    let bundle = fixture(root.path());
    let prefix = root.path().join("prefix with ' $ } spaces");
    let result = install(&bundle, &prefix, true).unwrap();
    assert_eq!(result["runtime_initialized"], false);
    let current = prefix.join("lib/proofstorm/current");
    let first = fs::read_link(&current).unwrap();
    assert_eq!(result, install(&bundle, &prefix, true).unwrap());
    assert_eq!(first, fs::read_link(&current).unwrap());
    assert!(!prefix.join("lib/proofstorm/state").exists());
    fs::write(bundle.join("LICENSE"), b"tampered").unwrap();
    assert!(install(&bundle, &prefix, true).is_err());
    assert_eq!(first, fs::read_link(current).unwrap());
}

#[test]
fn valid_upgrade_retains_old_version_and_refuses_changed_launcher() {
    let root = tempfile::tempdir().unwrap();
    let bundle = fixture(root.path());
    let prefix = root.path().join("prefix");
    install(&bundle, &prefix, true).unwrap();
    let managed = prefix.join("lib/proofstorm");
    let first = fs::read_link(managed.join("current")).unwrap();
    let license = bundle.join("LICENSE");
    fs::write(&license, b"updated fixture").unwrap();
    let manifest_path = bundle.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]["LICENSE"]["sha256"] = json!(hash(&license).unwrap());
    manifest["files"]["LICENSE"]["size"] = json!(fs::metadata(license).unwrap().len());
    fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    install(&bundle, &prefix, true).unwrap();
    let second = fs::read_link(managed.join("current")).unwrap();
    assert_ne!(first, second);
    assert!(managed.join(first).is_dir());
    fs::write(prefix.join("bin/proofstorm"), b"user modification").unwrap();
    assert!(install(&bundle, &prefix, true).is_err());
    assert_eq!(second, fs::read_link(managed.join("current")).unwrap());
}

#[test]
fn refuses_foreign_executables_and_unowned_installations() {
    let root = tempfile::tempdir().unwrap();
    let bundle = fixture(root.path());
    let prefix = root.path().join("prefix");
    fs::create_dir_all(prefix.join("bin")).unwrap();
    fs::write(prefix.join("bin/proofstorm"), b"unrelated user executable").unwrap();
    assert!(install(&bundle, &prefix, true).is_err());
    assert_eq!(
        fs::read(prefix.join("bin/proofstorm")).unwrap(),
        b"unrelated user executable"
    );
    fs::remove_file(prefix.join("bin/proofstorm")).unwrap();
    fs::create_dir_all(prefix.join("lib/proofstorm")).unwrap();
    assert!(install(&bundle, &prefix, true).is_err());
}

#[test]
fn refuses_development_by_default_and_detects_payload_changes() {
    let root = tempfile::tempdir().unwrap();
    let bundle = fixture(root.path());
    let prefix = root.path().join("prefix");
    assert!(install(&bundle, &prefix, false).is_err());
    assert!(!prefix.exists());
    fs::write(bundle.join("unlisted"), b"extra").unwrap();
    assert!(verify(&bundle, true, true).is_err());
    fs::remove_file(bundle.join("unlisted")).unwrap();
    fs::remove_file(bundle.join("LICENSE")).unwrap();
    std::os::unix::fs::symlink(root.path().join("foreign"), bundle.join("LICENSE")).unwrap();
    assert!(verify(&bundle, true, true).is_err());
}

#[test]
fn launcher_quotes_paths_and_respects_explicit_home() {
    let root = tempfile::tempdir().unwrap();
    let managed = root.path().join("quoted ' $ } path");
    fs::create_dir_all(managed.join("current/bin")).unwrap();
    let program = managed.join("current/bin/proofstorm");
    fs::write(
        &program,
        b"#!/bin/sh\nprintf '%s\\n' \"$PROOFSTORM_HOME\" \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = root.path().join("launcher");
    fs::write(&wrapper, launcher(&managed, "proofstorm").unwrap()).unwrap();
    let output = std::process::Command::new("sh")
        .arg(&wrapper)
        .arg("argument with spaces")
        .env_remove("PROOFSTORM_HOME")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{}\nargument with spaces\n",
            managed.join("state").display()
        )
    );
    let output = std::process::Command::new("sh")
        .arg(&wrapper)
        .arg("arg")
        .env("PROOFSTORM_HOME", "/explicit home")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "/explicit home\narg\n"
    );
}
