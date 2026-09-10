use super::*;
use crate::release::build;
use std::{collections::BTreeMap, process::Command};

fn save_file(root: &Path, name: &str, value: &Value) {
    save(&root.join(name), value).unwrap();
}

fn source() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("Cargo.toml"),
        "[workspace.package]\nversion = '0.1.0-alpha.2'\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(directory.path())
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    directory
}

pub(crate) fn receipt(provenance: &Value) -> Value {
    json!({"format_version":1,"release_ready":false,"platform":"linux/amd64","source":provenance,"metadata":{"version":"0.1.0-alpha.2","source_sha256":provenance["sha256"],"runtime_contract_sha256":"c".repeat(64)},"image":format!("{REPOSITORY}@sha256:{}","d".repeat(64)),"anonymous_verified":true,"verification":{"registry_identity":true,"offline_metadata":true,"non_root":true,"helper_startup":true}})
}

#[test]
fn clean_controller_snapshot_and_external_receipt_leave_checkout_unchanged() {
    let source = source();
    let output = tempfile::tempdir().unwrap();
    let work = output.path().canonicalize().unwrap().join("controller");
    let before = fs::read(source.path().join("Cargo.toml")).unwrap();
    let plan = prepare(source.path(), &work).unwrap();
    assert_eq!(plan.len(), 3);
    let record = bundle::read_json(&work.join("build.json")).unwrap();
    assert_eq!(record["source"]["dirty"], false);
    assert!(!work.join("source/.git").exists());
    assert!(plan[1].starts_with(&format!("{REPOSITORY}:ci-")));
    save_file(&work, "controller.json", &receipt(&record["source"]));
    stage(
        &work.join("controller.json"),
        &work.join("source"),
        &record["source"],
        &work.join("copy.json"),
    )
    .unwrap();
    assert_eq!(fs::read(source.path().join("Cargo.toml")).unwrap(), before);
    fs::write(source.path().join("uncommitted"), "user work").unwrap();
    assert!(prepare(source.path(), &output.path().join("dirty")).is_err());
}

#[test]
fn stale_dirty_wrong_platform_and_unverified_controller_records_fail_closed() {
    let provenance = json!({"revision":"a".repeat(40),"sha256":"b".repeat(64),"dirty":false});
    let valid = receipt(&provenance);
    validate(&valid, &provenance, "0.1.0-alpha.2").unwrap();
    for (path, value) in [
        ("/source/revision", json!("b".repeat(40))),
        ("/source/sha256", json!("a".repeat(64))),
        ("/source/dirty", json!(true)),
        ("/platform", json!("linux/arm64")),
        ("/metadata/version", json!("0.1.0-alpha.1")),
        ("/image", json!("ghcr.io/wrong/repo@sha256:bad")),
        ("/anonymous_verified", json!(false)),
        ("/verification/registry_identity", json!(false)),
        ("/verification/helper_startup", json!(false)),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(path).unwrap() = value;
        assert!(
            validate(&invalid, &provenance, "0.1.0-alpha.2").is_err(),
            "accepted {path}"
        );
    }
}

#[test]
fn local_image_and_exact_helper_probe_are_bound_to_source_and_previous_build() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().canonicalize().unwrap();
    let checkout = source();
    let source = build::snapshot(
        &checkout.path().canonicalize().unwrap(),
        &root.join("source"),
        false,
        None,
    )
    .unwrap();
    save_file(
        &root,
        "build.json",
        &json!({"version":"0.1.0-alpha.2","source":source}),
    );
    save_file(
        &root,
        "inspect.json",
        &json!([{"Id":format!("sha256:{}","d".repeat(64)),"Os":"linux","Architecture":"amd64","Config":{"User":"65532:65532","Labels":{"dev.proofstorm.source-sha256":source["sha256"]}}}]),
    );
    save_file(
        &root,
        "metadata.json",
        &json!({"format_version":1,"version":"0.1.0-alpha.2","source_sha256":source["sha256"],"runtime_contract_sha256":"c".repeat(64)}),
    );
    fs::write(root.join("helper.stdout"), "").unwrap();
    fs::write(
        root.join("helper.stderr"),
        "{\"runner_error\":\"native_runner_failed\"}\n",
    )
    .unwrap();
    fs::write(root.join("helper.status"), "1").unwrap();
    local(&root).unwrap();
    for (name, body) in [
        ("helper.stdout", "unexpected output"),
        ("helper.stderr", "architecture error"),
        ("helper.status", "0"),
    ] {
        let old = fs::read(root.join(name)).unwrap();
        fs::write(root.join(name), body).unwrap();
        assert!(local(&root).is_err());
        fs::write(root.join(name), old).unwrap();
    }
    let mut inspect = bundle::read_json(&root.join("inspect.json")).unwrap();
    inspect[0]["Id"] = json!(format!("sha256:{}", "e".repeat(64)));
    save_file(&root, "inspect.json", &inspect);
    assert!(local(&root).is_err());
    fs::write(root.join("source/unexpected"), "changed source").unwrap();
    assert!(build::verify_snapshot(&root.join("source"), &source).is_err());
}

// Generate a fixture registry graph for the real curl-free registry verifier tests.
#[test]
fn registry_digest_paths_reject_unsafe_identifiers() {
    assert!(registry::test_url("sha256:../../etc/passwd").is_err());
    assert!(
        registry::test_url(&format!("sha256:{}", "d".repeat(64)))
            .unwrap()
            .starts_with("https://ghcr.io/v2/")
    );
}

#[test]
fn staged_receipt_requires_the_exact_unmodified_source_fingerprint() {
    let source = source();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let provenance = build::snapshot(
        &source.path().canonicalize().unwrap(),
        &root.join("source"),
        false,
        None,
    )
    .unwrap();
    save_file(&root, "receipt.json", &receipt(&provenance));
    let mut other = provenance.clone();
    other["sha256"] = json!("f".repeat(64));
    assert!(
        stage(
            &root.join("receipt.json"),
            &root.join("source"),
            &other,
            &root.join("staged.json")
        )
        .is_err()
    );
    let files: BTreeMap<_, _> = crate::development::inventory(&root.join("source")).unwrap();
    assert_eq!(files.len(), 1);
    fs::write(
        root.join("source/Cargo.toml"),
        "[workspace.package]\nversion = '0.1.0-alpha.2'\n# changed\n",
    )
    .unwrap();
    assert!(
        stage(
            &root.join("receipt.json"),
            &root.join("source"),
            &provenance,
            &root.join("staged.json")
        )
        .is_err()
    );
}
