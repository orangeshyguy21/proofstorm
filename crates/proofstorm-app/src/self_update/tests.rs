use super::*;
use crate::installer::{
    install_with_metadata,
    tests::{alpha_info, fixture_with_info},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{cell::RefCell, fs, os::unix::fs::symlink};

fn asset(version: &str, name: &str, bytes: &[u8]) -> schema::Asset {
    schema::Asset {
        name: name.into(),
        url: format!(
            "https://github.com/{}/releases/download/v{version}/{name}",
            schema::REPOSITORY
        ),
        bytes: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}
fn release(version: &str, script: &[u8]) -> schema::Release {
    let archive = format!(
        "proofstorm-{version}-{}.tar.gz",
        schema::platform().unwrap()
    );
    schema::Release {
        schema_version: 1,
        repository: schema::REPOSITORY.into(),
        release_id: 42,
        version: version.into(),
        tag: format!("v{version}"),
        channel: "alpha".into(),
        published_at: "2026-09-14T00:00:00Z".into(),
        installer: asset(version, "install.sh", script),
        platforms: vec![schema::Platform {
            id: schema::platform().unwrap().into(),
            archive: asset(version, &archive, b"archive fixture"),
            checksum: asset(version, &format!("{archive}.sha256"), b"checksum fixture"),
        }],
    }
}
struct Feed {
    release: schema::Release,
    script: Vec<u8>,
    downloads: RefCell<Vec<String>>,
    corrupt: bool,
}
impl Feed {
    fn new(version: &str, script: &str) -> Self {
        Self {
            release: release(version, script.as_bytes()),
            script: script.as_bytes().into(),
            downloads: RefCell::default(),
            corrupt: false,
        }
    }
}
impl Download for Feed {
    async fn metadata(&self, _: &mut Cancellation) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.release)?)
    }
    async fn asset(&self, asset: &schema::Asset, path: &Path, _: &mut Cancellation) -> Result<()> {
        self.downloads.borrow_mut().push(asset.name.clone());
        let bytes: &[u8] = if self.corrupt {
            b"corrupt"
        } else if asset.name == "install.sh" {
            &self.script
        } else if asset.name.ends_with(".sha256") {
            b"checksum fixture"
        } else {
            b"archive fixture"
        };
        fs::write(path, bytes)?;
        Ok(())
    }
}
fn install(root: &Path, prefix: &Path, version: &str) -> (Value, String, PathBuf) {
    fs::create_dir_all(root).unwrap();
    let mut info = alpha_info();
    info["version"] = json!(version);
    info["controller"]["metadata"]["version"] = json!(version);
    let bundle = fixture_with_info(root, &info);
    for name in ["proofstorm", "proofstorm-mcp"] {
        // Metadata-only executable fixture. Actual installer activation is tested separately.
        let script = format!(
            "#!/bin/sh\ncat <<'RELEASE_INFO'\n{}\nRELEASE_INFO\n",
            serde_json::to_string(&info).unwrap()
        );
        fs::write(bundle.join("bin").join(name), script).unwrap();
    }
    let path = bundle.join("manifest.json");
    let mut manifest = managed::json(&path, 2 * 1024 * 1024).unwrap();
    manifest["channel"] = json!("alpha");
    manifest["controller"] = info["controller"].clone();
    for name in ["bin/proofstorm", "bin/proofstorm-mcp"] {
        manifest["files"][name]["sha256"] =
            json!(crate::artifacts::hash(&bundle.join(name)).unwrap());
        manifest["files"][name]["size"] = json!(fs::metadata(bundle.join(name)).unwrap().len());
    }
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let result = install_with_metadata(&bundle, prefix, false, &info).unwrap();
    let id = result["bundle_id"].as_str().unwrap().to_owned();
    let active = prefix.join("lib/proofstorm/versions").join(&id);
    (info, id, active)
}
fn managed_fixture(root: &Path) -> Managed {
    let prefix = root.join("prefix with ' $ spaces");
    let (info, _, active) = install(&root.join("old"), &prefix, "0.1.0-alpha.9");
    Managed::resolve(&active.join("bin/proofstorm"), &info).unwrap()
}
async fn update(installation: &Managed, feed: &Feed, check: bool) -> UpdateResult {
    let mut result = UpdateResult::default();
    execute(
        installation,
        feed,
        check,
        None,
        &|_| {},
        &mut Cancellation::new().unwrap(),
        &mut result,
    )
    .await
    .unwrap();
    result
}
fn point(installation: &Managed, id: &str) {
    fs::remove_file(installation.root.join("current")).unwrap();
    symlink(
        PathBuf::from("versions").join(id),
        installation.root.join("current"),
    )
    .unwrap();
}

#[test]
fn schema_refuses_ambiguous_or_untrusted_selection() {
    let original = serde_json::to_value(release("0.1.0-alpha.10", b"installer")).unwrap();
    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/repository", json!("foreign/repo")),
        ("/tag", json!("v0.1.0-alpha.9")),
        ("/version", json!("0.1.0-alpha.010")),
        ("/channel", json!("release")),
        ("/installer/url", json!("https://example.com/install.sh")),
        ("/installer/sha256", json!("bad")),
        ("/installer/bytes", json!(0)),
        ("/platforms/0/archive/bytes", json!(schema::MAX_ARCHIVE + 1)),
        ("/platforms/0/archive/name", json!("../bundle.tar.gz")),
        (
            "/platforms/0/checksum/url",
            json!("http://github.com/checksum"),
        ),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        let parsed: schema::Release = serde_json::from_value(changed).unwrap();
        assert!(
            parsed
                .selected(schema::platform().unwrap(), "alpha")
                .is_err(),
            "{pointer}"
        );
    }
    let mut duplicate: schema::Release = serde_json::from_value(original.clone()).unwrap();
    duplicate.platforms.push(duplicate.platforms[0].clone());
    assert!(
        duplicate
            .selected(schema::platform().unwrap(), "alpha")
            .is_err()
    );
    let mut extra = original;
    extra["future_field"] = json!("ignored");
    let parsed: schema::Release = serde_json::from_value(extra).unwrap();
    assert!(
        parsed
            .selected(schema::platform().unwrap(), "alpha")
            .is_ok()
    );
    assert!(parsed.selected("unsupported", "alpha").is_err());
}

#[tokio::test]
async fn discovery_is_numeric_read_only_and_never_downgrades() {
    let root = tempfile::tempdir().unwrap();
    let installation = managed_fixture(root.path());
    let before = fs::read(installation.root.join("install.json")).unwrap();
    for (version, status) in [
        ("0.1.0-alpha.10", "update_available"),
        ("0.1.0-alpha.9", "up_to_date"),
        ("0.1.0-alpha.8", "installed_newer"),
    ] {
        let feed = Feed::new(version, "exit 99\n");
        let result = update(&installation, &feed, true).await;
        assert_eq!(result.status, status, "{:?}", result.error);
        assert!(result.success());
        assert!(!result.verification_passed);
        assert!(feed.downloads.borrow().is_empty());
    }
    let feed = Feed::new("0.1.0-alpha.8", "exit 99\n");
    let result = update(&installation, &feed, false).await;
    assert_eq!(result.status, "installed_newer");
    assert!(result.verification_passed);
    assert!(feed.downloads.borrow().is_empty());
    assert_eq!(
        before,
        fs::read(installation.root.join("install.json")).unwrap()
    );
    assert!(!installation.root.join("state").exists());
}

#[tokio::test]
async fn failed_download_or_install_preserves_activation() {
    let root = tempfile::tempdir().unwrap();
    let installation = managed_fixture(root.path());
    let mut feed = Feed::new("0.1.0-alpha.10", "exit 12\n");
    feed.corrupt = true;
    let result = update(&installation, &feed, false).await;
    assert_eq!(result.error.unwrap().code, "release_download_failed");
    assert!(!result.activation_changed);
    feed.corrupt = false;
    let result = update(&installation, &feed, false).await;
    assert_eq!(result.status, "failed");
    assert!(result.activation_observed);
    assert_eq!(result.installed_version.as_deref(), Some("0.1.0-alpha.9"));
    assert_eq!(installation.active().unwrap().0, installation.id);
}

// A controlled installer stands in for the external, verified release script.
// It receives real literal arguments and simulates early/late failures around activation.
fn activation_script(installation: &Managed, id: &str, fail: bool) -> String {
    let receipt = json!({"installed":true,"version":"0.1.0-alpha.10","prefix":installation.prefix,"bundle_id":id});
    format!(
        "#!/bin/sh\nset -eu\n[ \"$1\" = --prefix ]\nprefix=$2\nrm \"$prefix/lib/proofstorm/current\"\nln -s 'versions/{id}' \"$prefix/lib/proofstorm/current\"\n{}\ncat <<'RECEIPT'\n{receipt}\nRECEIPT\n",
        if fail { "exit 12" } else { ":" }
    )
}
#[tokio::test]
async fn activation_receipt_and_late_failure_are_observed_and_retry_verified() {
    let root = tempfile::tempdir().unwrap();
    let installation = managed_fixture(root.path());
    let (new_info, new_id, new_bundle) = install(
        &root.path().join("new"),
        &installation.prefix,
        "0.1.0-alpha.10",
    );
    for fail in [false, true] {
        point(&installation, &installation.id);
        let feed = Feed::new(
            "0.1.0-alpha.10",
            &activation_script(&installation, &new_id, fail),
        );
        let result = update(&installation, &feed, false).await;
        assert_eq!(
            result.status,
            if fail {
                "activated_with_error"
            } else {
                "updated"
            },
            "{:?}",
            result.error
        );
        assert!(result.activation_changed);
        assert!(result.verification_passed);
        assert!(!result.runtime_refreshed);
        assert!(installation.bundle.exists());
        let newer = Managed::resolve(&new_bundle.join("bin/proofstorm"), &new_info).unwrap();
        let retry = Feed::new("0.1.0-alpha.10", "exit 99\n");
        let result = update(&newer, &retry, false).await;
        assert_eq!(result.status, "up_to_date");
        assert!(result.verification_passed);
        assert!(retry.downloads.borrow().is_empty());
    }
}

#[tokio::test]
async fn same_version_with_missing_launcher_is_not_reported_healthy() {
    let root = tempfile::tempdir().unwrap();
    let installation = managed_fixture(root.path());
    fs::remove_file(installation.prefix.join("bin/proofstorm-mcp")).unwrap();
    let feed = Feed::new("0.1.0-alpha.9", "exit 12\n");
    let result = update(&installation, &feed, false).await;
    assert_eq!(result.status, "failed");
    assert!(!result.verification_passed);
    assert_eq!(feed.downloads.borrow().len(), 3);
}

#[test]
fn unmanaged_inactive_and_mismatched_owner_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let installation = managed_fixture(root.path());
    let info = managed::json(
        &installation.bundle.join("release-info.json"),
        2 * 1024 * 1024,
    )
    .unwrap();
    assert!(Managed::resolve(Path::new("/bin/sh"), &info).is_err());
    let (_, new_id, _) = install(
        &root.path().join("new"),
        &installation.prefix,
        "0.1.0-alpha.10",
    );
    assert_ne!(new_id, installation.id);
    assert!(Managed::resolve(&installation.bundle.join("bin/proofstorm"), &info).is_err());
    point(&installation, &installation.id);
    fs::write(installation.root.join("install.json"), b"{}").unwrap();
    assert!(Managed::resolve(&installation.bundle.join("bin/proofstorm"), &info).is_err());
}

#[tokio::test]
async fn subprocesses_are_bounded_and_descendants_are_stopped() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("late-write");
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "(sleep 1; touch \"$1\") & wait", "fixture"])
        .arg(&marker);
    let output = process::run(command, Duration::from_millis(50), None)
        .await
        .unwrap();
    assert_eq!(output.failure, Some("timeout"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!marker.exists());
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "while :; do printf '1234567890123456789012345678901234567890'; done",
    ]);
    let output = process::run(command, Duration::from_secs(10), None)
        .await
        .unwrap();
    assert_eq!(output.failure, Some("output_limit"));
    assert!(output.stdout.len() <= 262_144);
}

#[test]
fn custom_runtime_advice_preserves_home_and_never_changes_shared_state() {
    let root = tempfile::tempdir().unwrap();
    let installation = managed_fixture(root.path());
    let home = root.path().join("custom ' runtime");
    let runtime = crate::installation::Installation::initialize(&home, None, None).unwrap();
    let info = managed::json(
        &installation.bundle.join("release-info.json"),
        2 * 1024 * 1024,
    )
    .unwrap();
    let state = json!({"installation_id":runtime.id,"image":"old controller"});
    fs::write(
        home.join("deployment-inputs.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let gui = json!({"installation_id":runtime.id,"executable":"/old/binary","token":"private-fixture-token"});
    fs::write(
        home.join("gui-process.json"),
        serde_json::to_vec(&gui).unwrap(),
    )
    .unwrap();
    fs::write(runtime.database(), b"sentinel: never open").unwrap();
    let before: Vec<_> = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| (p.clone(), fs::read(p).unwrap()))
        .collect();
    let actions = follow_ups(&installation, Some(&home), &info, true);
    assert_eq!(actions.len(), 4);
    let commands: Vec<_> = actions.iter().filter_map(|a| a.command.as_ref()).collect();
    assert_eq!(commands[0][3..], ["gui", "stop"]);
    assert_eq!(commands[1][3..], ["setup"]);
    assert_eq!(commands[2][3..], ["gui"]);
    for command in commands {
        assert_eq!(
            command[0],
            installation.prefix.join("bin/proofstorm").to_str().unwrap()
        );
        assert_eq!(command[2], runtime.home.to_str().unwrap());
    }
    assert!(
        !serde_json::to_string(&actions)
            .unwrap()
            .contains("private-fixture-token")
    );
    for (path, bytes) in before {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert!(!installation.root.join("state").exists());
}
