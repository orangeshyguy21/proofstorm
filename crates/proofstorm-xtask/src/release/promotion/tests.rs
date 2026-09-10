use super::*;
use crate::release::bundle::tests::Bundle;
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
        let jobs: Vec<_> = ["Formatting and shell", "Rust lints and tests", "Linux bundle and installer"].into_iter().map(|name| json!({"name":name,"status":"completed","conclusion":"success","head_sha":sha,"run_id":42})).collect();
        // Real attempt-specific responses do not require run_attempt on jobs.
        save(&metadata, "jobs.json", &json!([{"jobs":jobs}]));
        save(
            &metadata,
            "artifacts.json",
            &json!([{"artifacts":[{"id":123,"name":format!("proofstorm-linux-amd64-{sha}-2"),"expired":false,"workflow_run":{"id":42,"head_sha":sha}}]}]),
        );
        save(&metadata, "refs.json", &json!([]));
        save(&metadata, "releases.json", &json!([[]]));
        let mut bundle = Bundle::new("x86_64-unknown-linux-gnu", "alpha");
        if clean {
            bundle.optimized_clean();
        }
        let report = archive::pack(bundle.root(), &files).unwrap();
        save(&files, "build-report.json", &report);
        save(
            &files,
            "smoke-report.json",
            &json!({"integrity_verified":true,"relocated_binaries_verified":true,"source_read_access_denied":false,"release_ready":false}),
        );
        let installer = "#!/bin/sh\ninstall_version=\"0.1.0-alpha.1\"\n";
        fs::write(files.join("install.sh"), installer).unwrap();
        fs::write(metadata.join("source-install.sh"), installer).unwrap();
        save(
            &files,
            "install-smoke-report.json",
            &json!({"local_install":true,"reinstall":true,"cli_mcp_metadata_match":true,"source_checkout_present":false,"build_tools_present":false,"network_enabled":false,"runtime_tested":false,"github_download_tested":false,"development_override":false,"archive_sha256":report["sha256"],"installer_sha256":file_digest(&files.join("install.sh")).unwrap()}),
        );
        Self {
            _directory: directory,
            metadata,
            files,
        }
    }

    fn verify(&self) -> Result<()> {
        verify(&self.metadata, &self.files, REPO, ID, TAG)
    }

    fn mutate(&self, metadata: bool, name: &str, pointer: &str, value: Value) {
        let root = if metadata {
            &self.metadata
        } else {
            &self.files
        };
        let mut document = bundle::read_json(&root.join(name)).unwrap();
        *document.pointer_mut(pointer).unwrap() = value;
        save(root, name, &document);
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
        evidence(&fixture.metadata, REPO, ID, TAG).unwrap()[3],
        "123"
    );
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
        ("jobs.json", "/0/jobs", json!([])),
        ("artifacts.json", "/0/artifacts/0/expired", json!(true)),
        (
            "artifacts.json",
            "/0/artifacts/0/workflow_run/id",
            json!(43),
        ),
        ("artifacts.json", "/0/artifacts/0/id", json!(0)),
        ("artifacts.json", "/0/artifacts/0/name", json!("different")),
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
        let original = fs::read(fixture.files.join(name)).unwrap();
        fixture.mutate(false, name, pointer, value);
        assert!(fixture.verify().is_err(), "accepted {name} {pointer}");
        fs::write(fixture.files.join(name), original).unwrap();
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
    fs::remove_file(fixture.files.join("install.sh")).unwrap();
    symlink(
        fixture.metadata.join("source-install.sh"),
        fixture.files.join("install.sh"),
    )
    .unwrap();
    assert!(fixture.verify().is_err());
}

#[test]
fn installer_default_cannot_point_at_a_different_release() {
    let fixture = Candidate::new(true);
    let installer = "install_version=\"0.1.0-alpha.0\"\n";
    fs::write(fixture.files.join("install.sh"), installer).unwrap();
    fs::write(fixture.metadata.join("source-install.sh"), installer).unwrap();
    fixture.mutate(
        false,
        "install-smoke-report.json",
        "/installer_sha256",
        json!(file_digest(&fixture.files.join("install.sh")).unwrap()),
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
    let files = inventory(&fixture.files).unwrap();
    let response = json!({"id":9,"tag_name":TAG,"target_commitish":"a".repeat(40),"draft":true,"prerelease":true,"assets":files.keys().map(|name| json!({"name":name,"state":"uploaded"})).collect::<Vec<_>>()});
    save(&fixture.metadata, "created.json", &response);
    save(&fixture.metadata, "uploaded.json", &response);
    uploaded(&fixture.metadata, &fixture.files).unwrap();
    for (pointer, value) in [
        ("/id", json!(10)),
        ("/draft", json!(false)),
        ("/prerelease", json!(false)),
        ("/assets/0/state", json!("starter")),
        ("/assets/0/name", json!("unknown")),
        ("/assets", json!([])),
    ] {
        fixture.mutate(true, "uploaded.json", pointer, value);
        assert!(uploaded(&fixture.metadata, &fixture.files).is_err());
        save(&fixture.metadata, "uploaded.json", &response);
    }
    fs::write(fixture.files.join("install.sh"), "tampered after upload").unwrap();
    assert!(uploaded(&fixture.metadata, &fixture.files).is_err());
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
