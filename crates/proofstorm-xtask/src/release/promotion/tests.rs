use super::*;
use crate::release::{archive, bundle::tests::Bundle};
use std::{os::unix::fs::symlink, path::PathBuf};

const REPO: &str = "owner/proofstorm";
const TAG: &str = "v0.1.0-alpha.1";
const ID: &str = "42";

fn save(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
}

struct Candidate {
    _directory: tempfile::TempDir,
    metadata: PathBuf,
    files: PathBuf,
}

impl Candidate {
    fn new(clean: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let metadata = root.join("metadata");
        let files = root.join("candidate");
        fs::create_dir(&metadata).unwrap();
        fs::create_dir(&files).unwrap();
        let sha = "a".repeat(40);
        save(
            &metadata,
            "run.json",
            &json!({"id":42, "repository":{"full_name":REPO}, "head_repository":{"full_name":REPO}, "path":".github/workflows/check.yml", "workflow_id":7, "head_branch":"main", "event":"push", "status":"completed", "conclusion":"success", "head_sha":sha, "run_attempt":2}),
        );
        save(
            &metadata,
            "workflow.json",
            &json!({"id":7,"path":".github/workflows/check.yml"}),
        );
        save(
            &metadata,
            "ancestry.json",
            &json!({"base_commit":{"sha":sha},"merge_base_commit":{"sha":sha},"status":"ahead"}),
        );
        let jobs: Vec<_> = ["Formatting and shell", "Rust lints and tests", "Linux bundle and installer", "ARM64 controller", "Mac bundle and installer", "Mac installer isolation"].into_iter().map(|name| json!({"name":name,"status":"completed","conclusion":"success","head_sha":sha,"run_id":42})).collect();
        // Real attempt-specific responses do not require run_attempt on jobs.
        save(&metadata, "jobs.json", &json!([{"jobs":jobs}]));
        save(
            &metadata,
            "artifacts.json",
            &json!([{"artifacts":PLATFORMS.iter().enumerate().map(|(i,(slug,_))| json!({"id":123+i,"name":format!("proofstorm-{slug}-{sha}-2"),"expired":false,"workflow_run":{"id":42,"head_sha":sha}})).collect::<Vec<_>>()}]),
        );
        save(&metadata, "refs.json", &json!([]));
        save(&metadata, "releases.json", &json!([[]]));
        for (slug, target) in PLATFORMS {
            Self::platform(&files.join(slug), target, clean, None);
        }
        fs::write(
            metadata.join("source-install.sh"),
            "#!/bin/sh\ninstall_version=\"0.1.0-alpha.1\"\n",
        )
        .unwrap();
        Self {
            _directory: directory,
            metadata,
            files,
        }
    }

    fn platform(files: &Path, target: &str, clean: bool, source_hash: Option<&str>) {
        fs::create_dir(files).unwrap();
        let mac = target == "aarch64-apple-darwin";
        let mut bundle = Bundle::new(target, "alpha");
        if clean {
            bundle.optimized_clean();
        }
        bundle.matching_controller();
        if let Some(hash) = source_hash {
            bundle.source_hash(hash);
        }
        let report = archive::pack(bundle.root(), files).unwrap();
        save(files, "build-report.json", &report);
        save(
            files,
            "smoke-report.json",
            &json!({"integrity_verified":true,"relocated_binaries_verified":true,"source_read_access_denied":mac,"release_ready":false}),
        );
        let installer = "#!/bin/sh\ninstall_version=\"0.1.0-alpha.1\"\n";
        fs::write(files.join("install.sh"), installer).unwrap();
        save(
            files,
            "install-smoke-report.json",
            &json!({"target":target,"isolation":"macos-sandbox","source_read_access_denied":mac,"compiler_execution_denied":mac,"outside_writes_denied":mac,"local_install":true,"reinstall":true,"cli_mcp_metadata_match":true,"source_checkout_present":mac,"build_tools_present":mac,"network_enabled":false,"runtime_tested":false,"github_download_tested":false,"development_override":false,"archive_sha256":report["sha256"],"installer_sha256":file_digest(&files.join("install.sh")).unwrap()}),
        );
    }

    fn linux(&self) -> PathBuf {
        self.files.join("linux-amd64")
    }

    fn assets(&self) -> PathBuf {
        let output = self.metadata.parent().unwrap().join("assets");
        fs::create_dir(&output).unwrap();
        for (slug, _) in PLATFORMS {
            for entry in fs::read_dir(self.files.join(slug)).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                if REPORTS
                    .iter()
                    .any(|report| name == format!("{report}.json"))
                {
                    continue;
                }
                fs::copy(entry.path(), output.join(name)).unwrap();
            }
        }
        reports::pack(&self.files, &output.join(reports::NAME)).unwrap();
        fs::copy(
            self.metadata.join(manifest::NAME),
            output.join(manifest::NAME),
        )
        .unwrap();
        output
    }

    fn verify(&self) -> Result<()> {
        verify(&self.metadata, &self.files, REPO, ID, TAG)
    }

    fn mutate(&self, metadata: bool, name: &str, pointer: &str, value: Value) {
        let root = if metadata {
            self.metadata.clone()
        } else {
            self.linux()
        };
        let mut document = bundle::read_json(&root.join(name)).unwrap();
        *document.pointer_mut(pointer).unwrap() = value;
        save(&root, name, &document);
    }
}

#[test]
fn verified_promotion_is_draft_only_and_never_executes_payloads() {
    let fixture = Candidate::new(true);
    fixture.verify().unwrap();
    let request = bundle::read_json(&fixture.metadata.join("create-release.json")).unwrap();
    assert_eq!(request["draft"], true);
    assert_eq!(request["prerelease"], true);
    assert_eq!(request["make_latest"], "false");
    assert_eq!(request["target_commitish"], "a".repeat(40));
    assert_eq!(request["tag_name"], TAG);
    assert!(
        request["body"]
            .as_str()
            .unwrap()
            .contains("not a stable or release-ready build")
    );
    assert_eq!(
        &evidence(&fixture.metadata, REPO, ID, TAG).unwrap()[4..],
        &["123", "124"]
    );
}

#[test]
fn public_manifest_describes_exact_download_bytes_and_is_deterministic() {
    let fixture = Candidate::new(true);
    fixture.verify().unwrap();
    let bytes = fs::read(fixture.metadata.join(manifest::NAME)).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["version"], "0.1.0-alpha.1");
    assert_eq!(document["tag"], TAG);
    assert_eq!(document["source_commit"], "a".repeat(40));
    assert_eq!(document["channel"], "alpha");
    assert_eq!(document["repository"], REPO);
    assert_eq!(document["installer"], "install.sh");
    assert_eq!(document["verification_reports"], reports::NAME);
    assert_eq!(
        document["platforms"],
        json!({
            "linux-amd64": {
                "os":"linux", "arch":"amd64", "target":"x86_64-unknown-linux-gnu",
                "archive":"proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz",
                "checksum":"proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz.sha256"
            },
            "macos-arm64": {
                "os":"macos", "arch":"arm64", "target":"aarch64-apple-darwin",
                "archive":"proofstorm-0.1.0-alpha.1-macos-arm64.tar.gz",
                "checksum":"proofstorm-0.1.0-alpha.1-macos-arm64.tar.gz.sha256"
            }
        })
    );
    let directory = fixture.assets();
    let mut files = inventory(&directory).unwrap();
    files.remove(manifest::NAME).unwrap();
    let assets = document["assets"].as_object().unwrap();
    assert_eq!(assets.len(), 6);
    assert_eq!(
        assets.keys().collect::<BTreeSet<_>>(),
        files.keys().collect::<BTreeSet<_>>()
    );
    for (name, sha256) in files {
        assert_eq!(
            assets[&name],
            json!({"size_bytes":fs::metadata(directory.join(&name)).unwrap().len(),"sha256":sha256})
        );
    }
    assert_eq!(
        assets["install.sh"]["sha256"],
        file_digest(&fixture.metadata.join("source-install.sh")).unwrap()
    );
    assert!(!String::from_utf8_lossy(&bytes).contains(fixture.metadata.to_str().unwrap()));
    fixture.verify().unwrap();
    assert_eq!(
        bytes,
        fs::read(fixture.metadata.join(manifest::NAME)).unwrap()
    );
    verify_assets(&fixture.metadata, &directory).unwrap();
}

#[test]
fn manifest_asset_measurements_reject_changed_or_linked_inputs() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let path = root.join("install.sh");
    let bytes = b"fixture installer";
    fs::write(&path, bytes).unwrap();
    let expected = manifest::Asset::from_bytes(bytes);
    let measured = manifest::Asset::read_verified(&path, &expected.sha256).unwrap();
    assert_eq!(measured.size_bytes, bytes.len() as u64);
    assert_eq!(measured.sha256, expected.sha256);
    fs::write(&path, b"changed installer").unwrap();
    assert!(manifest::Asset::read_verified(&path, &expected.sha256).is_err());
    let link = root.join("linked.sh");
    symlink(&path, &link).unwrap();
    assert!(manifest::Asset::read_verified(&link, &file_digest(&path).unwrap()).is_err());
}

#[test]
fn untrusted_failed_stale_or_incomplete_ci_evidence_is_rejected() {
    let cases = [
        ("run.json", "/id", json!(43)),
        ("run.json", "/repository/full_name", json!("other/repo")),
        ("run.json", "/head_repository/full_name", json!("fork/repo")),
        ("run.json", "/head_branch", json!("feature")),
        ("run.json", "/event", json!("pull_request")),
        ("run.json", "/path", json!("other.yml")),
        ("workflow.json", "/id", json!(8)),
        ("run.json", "/conclusion", json!("failure")),
        ("run.json", "/status", json!("in_progress")),
        ("run.json", "/head_sha", json!("bad")),
        ("run.json", "/run_attempt", json!(0)),
        ("run.json", "/run_attempt", json!(3)),
        ("ancestry.json", "/status", json!("diverged")),
        (
            "ancestry.json",
            "/merge_base_commit/sha",
            json!("b".repeat(40)),
        ),
        ("jobs.json", "/0/jobs/0/conclusion", json!("skipped")),
        ("jobs.json", "/0/jobs/1/run_id", json!(43)),
        ("jobs.json", "/0/jobs/2/head_sha", json!("b".repeat(40))),
        ("jobs.json", "/0/jobs/3/conclusion", json!("failure")),
        ("jobs.json", "/0/jobs/4/conclusion", json!("skipped")),
        ("jobs.json", "/0/jobs/5/conclusion", json!("failure")),
        ("jobs.json", "/0/jobs", json!([])),
        ("artifacts.json", "/0/artifacts/0/expired", json!(true)),
        (
            "artifacts.json",
            "/0/artifacts/0/workflow_run/id",
            json!(43),
        ),
        ("artifacts.json", "/0/artifacts/0/id", json!(0)),
        ("artifacts.json", "/0/artifacts/0/name", json!("different")),
        ("artifacts.json", "/0/artifacts/1/expired", json!(true)),
        ("artifacts.json", "/0/artifacts/1/id", json!(123)),
    ];
    let fixture = Candidate::new(true);
    for (name, pointer, value) in cases {
        let original = fs::read(fixture.metadata.join(name)).unwrap();
        fixture.mutate(true, name, pointer, value);
        assert!(fixture.verify().is_err(), "accepted {name} {pointer}");
        fs::write(fixture.metadata.join(name), original).unwrap();
    }
    for (name, key) in [("jobs.json", "jobs"), ("artifacts.json", "artifacts")] {
        let original = bundle::read_json(&fixture.metadata.join(name)).unwrap();
        let duplicate = json!([original[0].clone(), original[0].clone()]);
        save(&fixture.metadata, name, &duplicate);
        assert!(fixture.verify().is_err(), "duplicate {key}");
        save(&fixture.metadata, name, &original);
    }
}

#[test]
fn existing_versions_and_api_errors_fail_closed() {
    let fixture = Candidate::new(true);
    save(
        &fixture.metadata,
        "refs.json",
        &json!([{"ref":format!("refs/tags/{TAG}-suffix")} ]),
    );
    fixture.verify().unwrap();
    save(
        &fixture.metadata,
        "refs.json",
        &json!([{"ref":format!("refs/tags/{TAG}")} ]),
    );
    assert!(
        fixture
            .verify()
            .unwrap_err()
            .to_string()
            .contains("tag already exists")
    );
    save(&fixture.metadata, "refs.json", &json!([]));
    save(
        &fixture.metadata,
        "releases.json",
        &json!([[], [{"tag_name":TAG,"draft":true}]]),
    );
    assert!(
        fixture
            .verify()
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
    save(
        &fixture.metadata,
        "releases.json",
        &json!({"message":"Unauthorized"}),
    );
    assert!(fixture.verify().is_err());
}

#[test]
fn reports_must_match_tested_bundle_bytes_and_successful_checks() {
    let fixture = Candidate::new(true);
    for (name, pointer, value) in [
        ("build-report.json", "/sha256", json!("0".repeat(64))),
        ("build-report.json", "/archive", json!("wrong.tar.gz")),
        ("build-report.json", "/release_ready", json!(true)),
        ("build-report.json", "/release_blockers", json!([])),
        (
            "smoke-report.json",
            "/relocated_binaries_verified",
            json!(false),
        ),
        ("smoke-report.json", "/integrity_verified", json!(false)),
        ("install-smoke-report.json", "/reinstall", json!(false)),
        (
            "install-smoke-report.json",
            "/archive_sha256",
            json!("0".repeat(64)),
        ),
        (
            "install-smoke-report.json",
            "/installer_sha256",
            json!("0".repeat(64)),
        ),
        (
            "install-smoke-report.json",
            "/development_override",
            json!(true),
        ),
        ("install-smoke-report.json", "/runtime_tested", json!(true)),
    ] {
        let original = fs::read(fixture.linux().join(name)).unwrap();
        fixture.mutate(false, name, pointer, value);
        assert!(fixture.verify().is_err(), "accepted {name} {pointer}");
        fs::write(fixture.linux().join(name), original).unwrap();
    }
    fs::write(fixture.metadata.join("source-install.sh"), "different").unwrap();
    assert!(
        fixture
            .verify()
            .unwrap_err()
            .to_string()
            .contains("selected source commit")
    );
}

#[test]
fn dirty_debug_extra_linked_or_wrong_version_candidates_are_rejected() {
    assert!(
        Candidate::new(false)
            .verify()
            .unwrap_err()
            .to_string()
            .contains("optimized, clean")
    );
    let fixture = Candidate::new(true);
    assert!(
        verify(
            &fixture.metadata,
            &fixture.files,
            REPO,
            ID,
            "v0.1.0-alpha.2"
        )
        .is_err()
    );
    fs::write(fixture.files.join("extra"), "unexpected").unwrap();
    assert!(fixture.verify().is_err());
    fs::remove_file(fixture.files.join("extra")).unwrap();
    fs::remove_file(fixture.linux().join("install.sh")).unwrap();
    symlink(
        fixture.metadata.join("source-install.sh"),
        fixture.linux().join("install.sh"),
    )
    .unwrap();
    assert!(fixture.verify().is_err());
}

#[test]
fn installer_default_cannot_point_at_a_different_release() {
    let fixture = Candidate::new(true);
    let installer = "install_version=\"0.1.0-alpha.0\"\n";
    fs::write(fixture.linux().join("install.sh"), installer).unwrap();
    fs::write(fixture.metadata.join("source-install.sh"), installer).unwrap();
    fixture.mutate(
        false,
        "install-smoke-report.json",
        "/installer_sha256",
        json!(file_digest(&fixture.linux().join("install.sh")).unwrap()),
    );
    assert!(
        fixture
            .verify()
            .unwrap_err()
            .to_string()
            .contains("default version")
    );
}

#[test]
fn uploaded_assets_must_remain_identical_and_unpublished() {
    let fixture = Candidate::new(true);
    fixture.verify().unwrap();
    let assets = fixture.assets();
    verify_assets(&fixture.metadata, &assets).unwrap();
    let files = inventory(&assets).unwrap();
    assert_eq!(files.len(), 7);
    let response = json!({"id":9,"tag_name":TAG,"target_commitish":"a".repeat(40),"draft":true,"prerelease":true,"assets":files.keys().map(|name| json!({"name":name,"state":"uploaded"})).collect::<Vec<_>>()});
    save(&fixture.metadata, "created.json", &response);
    save(&fixture.metadata, "uploaded.json", &response);
    uploaded(&fixture.metadata, &assets).unwrap();
    for (pointer, value) in [
        ("/id", json!(10)),
        ("/draft", json!(false)),
        ("/prerelease", json!(false)),
        ("/assets/0/state", json!("starter")),
        ("/assets/0/name", json!("unknown")),
        ("/assets", json!([])),
    ] {
        fixture.mutate(true, "uploaded.json", pointer, value);
        assert!(uploaded(&fixture.metadata, &assets).is_err());
        save(&fixture.metadata, "uploaded.json", &response);
    }
    for name in ["install.sh", manifest::NAME, reports::NAME] {
        let path = assets.join(name);
        let original = fs::read(&path).unwrap();
        fs::write(&path, "tampered after upload").unwrap();
        assert!(
            uploaded(&fixture.metadata, &assets).is_err(),
            "accepted changed {name}"
        );
        assert!(verify_assets(&fixture.metadata, &assets).is_err());
        fs::remove_file(&path).unwrap();
        assert!(
            uploaded(&fixture.metadata, &assets).is_err(),
            "accepted missing {name}"
        );
        assert!(verify_assets(&fixture.metadata, &assets).is_err());
        fs::write(&path, original).unwrap();
    }
    uploaded(&fixture.metadata, &assets).unwrap();
}

#[test]
fn report_archive_retains_exact_evidence_and_is_verified_as_one_asset() {
    use std::{collections::BTreeMap, io::Read};
    let fixture = Candidate::new(true);
    fixture.verify().unwrap();
    let assets = fixture.assets();
    let first = assets.join(reports::NAME);
    let second = assets.join("repeat.tar.gz");
    reports::pack(&fixture.files, &second).unwrap();
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    assert!(
        reports::pack(&fixture.files, &first).is_err(),
        "overwrote report archive"
    );
    fs::remove_file(second).unwrap();
    let mut expected = BTreeMap::new();
    for (platform, _) in PLATFORMS {
        for report in REPORTS {
            expected.insert(
                format!("{report}-{platform}.json"),
                fs::read(fixture.files.join(platform).join(format!("{report}.json"))).unwrap(),
            );
        }
    }
    let reader = flate2::read::GzDecoder::new(fs::File::open(&first).unwrap());
    let mut archive = tar::Archive::new(reader);
    let mut actual = BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        assert!(entry.header().entry_type().is_file());
        assert_eq!(entry.header().mode().unwrap(), 0o644);
        assert_eq!(entry.header().mtime().unwrap(), 0);
        let name = entry.path().unwrap().to_str().unwrap().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(actual.insert(name, bytes).is_none());
    }
    assert_eq!(actual, expected);
    verify_assets(&fixture.metadata, &assets).unwrap();
    fs::write(&first, "tampered reports").unwrap();
    assert!(verify_assets(&fixture.metadata, &assets).is_err());
    let files = inventory(&fixture.files).unwrap();
    fs::write(fixture.linux().join("build-report.json"), "{}").unwrap();
    assert!(
        reports::asset(&fixture.files, &files).is_err(),
        "accepted changed source evidence"
    );
    fs::remove_file(fixture.linux().join("build-report.json")).unwrap();
    symlink(
        fixture.metadata.join("run.json"),
        fixture.linux().join("build-report.json"),
    )
    .unwrap();
    assert!(reports::pack(&fixture.files, &assets.join("linked.tar.gz")).is_err());
}

#[test]
fn invalid_cli_and_version_arguments_fail() {
    for tag in ["v1.0.0", "0.1.0-alpha.1", "v0.1.0-alpha.1;echo x"] {
        assert!(version(tag).is_err());
    }
    assert!(cli([OsString::from("unknown")].into_iter()).is_err());
    let fixture = Candidate::new(true);
    for (repo, id) in [("bad/repo/extra", ID), (REPO, "0"), (REPO, "-1")] {
        assert!(run_plan(&fixture.metadata, repo, id, TAG).is_err());
    }
}

#[test]
fn mac_requires_its_own_isolation_evidence_and_identical_source() {
    let fixture = Candidate::new(true);
    let mac = fixture.files.join("macos-arm64");
    let name = "install-smoke-report.json";
    let original = bundle::read_json(&mac.join(name)).unwrap();
    for (key, value) in [
        ("source_read_access_denied", json!(false)),
        ("compiler_execution_denied", json!(false)),
        ("outside_writes_denied", json!(false)),
        ("network_enabled", json!(true)),
        ("target", json!("x86_64-unknown-linux-gnu")),
        ("isolation", json!("none")),
        ("reinstall", json!(false)),
    ] {
        let mut invalid = original.clone();
        invalid[key] = value;
        save(&mac, name, &invalid);
        assert!(fixture.verify().is_err(), "accepted Mac {key}");
    }
    save(&mac, name, &original);
    fixture.verify().unwrap();
    // Each archive and controller is internally coherent, but they are not the same source.
    fs::remove_dir_all(&mac).unwrap();
    Candidate::platform(&mac, "aarch64-apple-darwin", true, Some(&"c".repeat(64)));
    assert!(
        fixture
            .verify()
            .unwrap_err()
            .to_string()
            .contains("source fingerprints differ")
    );
    fs::remove_dir_all(&mac).unwrap();
    assert!(fixture.verify().is_err(), "accepted Linux-only candidate");
}
