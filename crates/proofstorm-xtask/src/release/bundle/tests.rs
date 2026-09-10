use super::*;
use std::os::unix::fs::symlink;

pub(crate) struct Bundle {
    directory: tempfile::TempDir,
    manifest: Value,
    pub(crate) info: Value,
}

impl Bundle {
    pub(crate) fn new(target: &str, channel: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut info: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/release-info.json"))
                .unwrap();
        info["target"] = json!(target);
        info["bootstrap_tools"]["target"] = json!(target);
        info["controller"]["platform"] = json!(platform(target).unwrap());
        info["source_revision"] = json!("a".repeat(40));
        info["source_sha256"] = json!("b".repeat(64));
        info["catalog"] = json!({"entries": []});
        info["tools"] = json!("TRUNK_VERSION=fixture\n");
        for name in REQUIRED {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Deliberately not executable code: the verifier must never run it.
            fs::write(&path, "fixture payload\n").unwrap();
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
        fs::write(root.join("release-info.json"), info.to_string()).unwrap();
        fs::write(root.join("catalog.json"), info["catalog"].to_string()).unwrap();
        fs::write(
            root.join("tools/versions.env"),
            info["tools"].as_str().unwrap(),
        )
        .unwrap();
        let files: Map<_, _> = REQUIRED.iter().map(|name| {
            let path = root.join(name);
            let metadata = fs::metadata(&path).unwrap();
            ((*name).to_owned(), json!({"size": metadata.len(), "sha256": checksum(&path, metadata.len()).unwrap(),
                "mode": metadata.permissions().mode() & 0o7777}))
        }).collect();
        let manifest = json!({"format_version": 1, "version": info["version"], "target": target,
            "build_profile": "debug", "channel": channel, "release_ready": false,
            "release_blockers": ["Fixture has no runtime verification."],
            "source": {"revision": info["source_revision"], "sha256": info["source_sha256"], "dirty": true},
            "controller": info["controller"], "workload_images": image_inventory(&info).unwrap(), "files": files});
        let fixture = Self {
            directory,
            manifest,
            info,
        };
        fixture.save();
        fixture
    }

    pub(crate) fn root(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn optimized_clean(&mut self) {
        self.info["build_profile"] = json!("release");
        self.manifest["build_profile"] = json!("release");
        self.manifest["source"]["dirty"] = json!(false);
        fs::write(self.root().join("release-info.json"), self.info.to_string()).unwrap();
        self.refresh("release-info.json");
    }

    fn save(&self) {
        fs::write(self.root().join("manifest.json"), self.manifest.to_string()).unwrap();
    }

    pub(crate) fn refresh(&mut self, name: &str) {
        let path = self.root().join(name);
        let size = fs::metadata(&path).unwrap().len();
        self.manifest["files"][name]["size"] = json!(size);
        self.manifest["files"][name]["sha256"] = json!(checksum(&path, size).unwrap());
        self.save();
    }

    fn rejects(&self, message: &str) {
        self.save();
        let error = verify(self.root()).unwrap_err().to_string();
        assert!(error.contains(message), "expected {message}: {error}");
    }
}

fn fixture() -> Bundle {
    Bundle::new("x86_64-unknown-linux-gnu", "development")
}

#[test]
fn both_targets_and_alpha_development_channels_verify_without_executing_payloads() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
        for channel in ["alpha", "development"] {
            let bundle = Bundle::new(target, channel);
            let before = fs::read(bundle.root().join("manifest.json")).unwrap();
            let result = verify(bundle.root()).unwrap();
            assert_eq!(result["integrity_verified"], true);
            assert_eq!(result["release_ready"], false);
            assert_eq!(result["target"], target);
            assert_eq!(
                fs::read(bundle.root().join("manifest.json")).unwrap(),
                before
            );
        }
    }
}

#[test]
fn missing_tampered_and_unlisted_payloads_fail() {
    let bundle = fixture();
    let path = bundle.root().join("LICENSE");
    fs::write(&path, "tampered\n").unwrap();
    bundle.rejects("checksum mismatch");
    fs::remove_file(&path).unwrap();
    bundle.rejects("missing or unlisted");
    fs::write(&path, "fixture payload\n").unwrap();
    fs::write(bundle.root().join("unexpected"), "extra").unwrap();
    bundle.rejects("missing or unlisted");
}

#[test]
fn root_manifest_file_and_directory_symlinks_are_refused() {
    let bundle = fixture();
    let links = tempfile::tempdir().unwrap();
    let root_link = links.path().join("linked-bundle");
    symlink(bundle.root(), &root_link).unwrap();
    assert!(
        verify(&root_link)
            .unwrap_err()
            .to_string()
            .contains("real directory")
    );
    for name in ["manifest.json", "LICENSE", "chart"] {
        let source = bundle.root().join(name);
        let saved = links.path().join(name);
        fs::rename(&source, &saved).unwrap();
        symlink(&saved, &source).unwrap();
        assert!(
            verify(bundle.root())
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );
        fs::remove_file(&source).unwrap();
        fs::rename(&saved, &source).unwrap();
    }
}

#[test]
fn unsafe_manifest_paths_are_refused_before_payload_access() {
    for name in [
        "../outside",
        "/absolute",
        "chart/../outside",
        "chart//file",
        "./LICENSE",
        "bin/",
        "manifest.json",
        "..\\outside",
    ] {
        let mut bundle = fixture();
        bundle.manifest["files"][name] = bundle.manifest["files"]["LICENSE"].clone();
        bundle.rejects("unsafe manifest path");
    }
}

#[test]
fn modes_cannot_be_changed_or_relabelled_by_the_manifest() {
    let mut bundle = fixture();
    let binary = bundle.root().join("bin/proofstorm");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
    bundle.rejects("mode mismatch");
    bundle.manifest["files"]["bin/proofstorm"]["mode"] = json!(0o644);
    bundle.rejects("unsafe payload mode");
    bundle.manifest["files"]["bin/proofstorm"]["mode"] = json!(0o4755);
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o4755)).unwrap();
    bundle.rejects("unsafe payload mode");
}

#[test]
fn manifest_identity_and_provenance_must_match_payload_metadata() {
    for (pointer, value, message) in [
        ("/target", json!("aarch64-apple-darwin"), "target mismatch"),
        ("/version", json!("0.2.0"), "version mismatch"),
        ("/build_profile", json!("release"), "build_profile mismatch"),
        (
            "/source/revision",
            json!("f".repeat(40)),
            "provenance mismatch",
        ),
        (
            "/source/sha256",
            json!("f".repeat(64)),
            "provenance mismatch",
        ),
        ("/source/dirty", json!("false"), "dirty flag"),
        (
            "/controller/release_ready",
            json!(true),
            "controller metadata mismatch",
        ),
        (
            "/workload_images/0/availability_verified",
            json!(true),
            "image inventory mismatch",
        ),
    ] {
        let mut bundle = fixture();
        *bundle.manifest.pointer_mut(pointer).unwrap() = value;
        bundle.rejects(message);
    }
}

#[test]
fn catalog_and_tool_pins_are_cross_checked_even_with_updated_checksums() {
    for (name, bytes, message) in [
        ("catalog.json", "{}", "catalog mismatch"),
        (
            "tools/versions.env",
            "OTHER_VERSION=1\n",
            "tool pins mismatch",
        ),
    ] {
        let mut bundle = fixture();
        fs::write(bundle.root().join(name), bytes).unwrap();
        bundle.refresh(name);
        bundle.rejects(message);
    }
}

#[test]
fn metadata_rules_apply_to_checksumming_valid_bundles() {
    let mut bundle = fixture();
    bundle.info["workload_images"][0] = json!("example/app:latest");
    fs::write(
        bundle.root().join("release-info.json"),
        bundle.info.to_string(),
    )
    .unwrap();
    bundle.refresh("release-info.json");
    bundle.rejects("not pinned");
    let mut bundle = Bundle::new("x86_64-unknown-linux-gnu", "alpha");
    bundle.info["controller"] = Value::Null;
    bundle.manifest["controller"] = Value::Null;
    fs::write(
        bundle.root().join("release-info.json"),
        bundle.info.to_string(),
    )
    .unwrap();
    bundle.refresh("release-info.json");
    bundle.rejects("alpha requires");
}

#[test]
fn hand_edited_readiness_is_never_release_evidence() {
    let mut bundle = fixture();
    bundle.manifest["release_ready"] = json!(true);
    bundle.rejects("inconsistent release readiness");
    bundle.manifest["release_blockers"] = json!([]);
    bundle.rejects("lacks required evidence");
    bundle.manifest["channel"] = json!("release");
    bundle.manifest["build_profile"] = json!("release");
    bundle.manifest["source"]["dirty"] = json!(false);
    bundle.manifest["workload_images"][0]["availability_verified"] = json!(true);
    bundle.rejects("lacks required evidence");
}

#[test]
fn malformed_receipts_incomplete_manifests_and_size_limits_fail_closed() {
    for (key, value, message) in [
        ("size", json!(-1), "invalid payload size"),
        ("size", json!(0), "empty payload"),
        ("size", json!(MAX_PAYLOAD_BYTES + 1), "size limit"),
        ("sha256", json!("a".repeat(63)), "invalid payload digest"),
        ("mode", json!(true), "unsafe payload mode"),
    ] {
        let mut bundle = fixture();
        bundle.manifest["files"]["LICENSE"][key] = value;
        bundle.rejects(message);
    }
    let mut bundle = fixture();
    bundle.manifest["files"]
        .as_object_mut()
        .unwrap()
        .remove("chart/Chart.yaml");
    bundle.rejects("incomplete bundle manifest");
    for (key, value) in [
        ("format_version", json!(2)),
        ("channel", json!("unknown")),
        ("release_ready", json!("false")),
        ("release_blockers", json!([false])),
        ("files", json!([])),
    ] {
        let mut bundle = fixture();
        bundle.manifest[key] = value;
        bundle.save();
        assert!(verify(bundle.root()).is_err(), "{key}");
    }
}
