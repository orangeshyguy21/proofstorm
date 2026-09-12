use super::*;

#[test]
fn probe_outputs_match_reviewed_versions_and_the_cdk_rpc_binary_name() {
    for (name, output) in [
        ("bitcoin-core", "Bitcoin Core version v31.1.0\nCopyright\n"),
        ("cdk-cli-wallet", "cdk-cli 0.18.0\n"),
        ("cocod-wallet", "0.0.17\n"),
        (
            "cdk-mint-management",
            "cdk-mint-rpc 0.18.0\ncdk-mintd 0.18.0\n",
        ),
        (
            "cdk-ldk-mint-management",
            "cdk-mint-rpc 0.18.0\ncdk-mintd 0.18.0\n",
        ),
    ] {
        assert!(valid_probe(name, output));
        assert!(!valid_probe(name, "wrong version"));
    }
    assert!(!valid_probe(
        "bitcoin-core",
        "Bitcoin Core version v31.10.0\n"
    ));
    assert!(!valid_probe(
        "cdk-mint-management",
        "cdk-mint-rpc 0.18.0\ncdk-mintd 0.17.0\n"
    ));
}

#[test]
fn cocod_external_context_cannot_drift_or_become_a_link() {
    let work = tempfile::tempdir().unwrap();
    let work = work.path().canonicalize().unwrap();
    fs::create_dir_all(work.join("source/docker/wallet")).unwrap();
    fs::create_dir(work.join("context")).unwrap();
    fs::write(
        work.join("context/source.tar.gz"),
        b"reviewed source archive",
    )
    .unwrap();
    fs::write(
        work.join("source/docker/wallet/cocod-44e5101c-provenance.json"),
        json!({"artifact_sha256":format!("{:x}", Sha256::digest(b"reviewed source archive"))})
            .to_string(),
    )
    .unwrap();
    verify_cocod_archive(&work).unwrap();
    fs::write(work.join("context/source.tar.gz"), b"changed").unwrap();
    assert!(verify_cocod_archive(&work).is_err());
    fs::rename(work.join("context/source.tar.gz"), work.join("outside")).unwrap();
    std::os::unix::fs::symlink(work.join("outside"), work.join("context/source.tar.gz")).unwrap();
    assert!(verify_cocod_archive(&work).is_err());
}

fn receipt() -> Receipt {
    let id = "a".repeat(32);
    Receipt {
        format_version: 1,
        repository: "cdk-cli-wallet".into(),
        platform: "linux/amd64".into(),
        publication_id: id.clone(),
        tag: format!("{NAMESPACE}/cdk-cli-wallet:upload-{id}"),
        input: Input::Build {
            source: json!({"revision":"a".repeat(40),"sha256":"b".repeat(64),"dirty":false}),
            recipe_sha256: "c".repeat(64),
        },
        local_image_id: None,
        local_verified: false,
        publication: Publication::Prepared,
        image: None,
        release_ready: false,
    }
}

#[test]
fn receipts_refuse_wrong_namespace_platform_identity_and_readiness_claims() {
    for platform in ["linux/amd64", "linux/arm64"] {
        let mut good = receipt();
        good.platform = platform.into();
        good.validate().unwrap();
    }
    let encoded = serde_json::to_value(receipt()).unwrap();
    for (key, value) in [
        ("tag", json!(format!("{NAMESPACE}/cdk-cli-wallet:latest"))),
        ("tag", json!("ghcr.io/other/wallet:upload-test")),
        ("platform", json!("linux/386")),
        ("repository", json!("proofstormd")),
        ("local_image_id", json!("latest")),
        ("release_ready", json!(true)),
        ("publication", json!("verified")),
        ("publication", json!("uploaded")),
    ] {
        let mut bad = encoded.clone();
        bad[key] = value;
        assert!(
            serde_json::from_value::<Receipt>(bad)
                .unwrap()
                .validate()
                .is_err(),
            "{key}"
        );
    }
    let mut bad = encoded;
    bad["input"]["source"]["dirty"] = json!(true);
    assert!(
        serde_json::from_value::<Receipt>(bad)
            .unwrap()
            .validate()
            .is_err()
    );
    assert!(authorize(Path::new("does-not-exist"), "wrong namespace").is_err());
}

#[test]
fn local_probes_bind_both_platforms_to_clean_source_and_immutable_ids() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("docker/wallet")).unwrap();
    fs::create_dir(root.join("release")).unwrap();
    fs::write(
        root.join("docker/wallet/Dockerfile.kube-cdk"),
        "FROM fixture\n",
    )
    .unwrap();
    fs::write(
        root.join("release/ghcr.json"),
        json!({"namespace":NAMESPACE,"visibility":"public"}).to_string(),
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
                .current_dir(&root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let temp = tempfile::tempdir().unwrap();
    for arch in ["amd64", "arm64"] {
        let work = temp.path().canonicalize().unwrap().join(arch);
        prepare(
            &root,
            &work,
            "cdk-cli-wallet",
            &format!("linux/{arch}"),
            None,
        )
        .unwrap();
        let source_sha =
            bundle::read_json(&work.join("image.json")).unwrap()["input"]["source"]["sha256"]
                .clone();
        let image = json!([{"Id":format!("sha256:{}","d".repeat(64)),"Os":"linux","Architecture":arch,"Config":{"User":"1000:1000","Labels":{"dev.proofstorm.source-sha256":source_sha}}}]);
        fs::write(work.join("inspect.json"), image.to_string()).unwrap();
        fs::write(work.join("probe.stdout"), "cdk-cli 0.18.0\n").unwrap();
        local(&work).unwrap();
        assert!(load(&work).unwrap().local_verified);
        for (path, value) in [
            ("/0/Id", json!(format!("sha256:{}", "e".repeat(64)))),
            ("/0/Architecture", json!("386")),
            ("/0/Config/User", json!("0")),
            (
                "/0/Config/Labels/dev.proofstorm.source-sha256",
                json!("wrong"),
            ),
        ] {
            let mut bad = image.clone();
            *bad.pointer_mut(path).unwrap() = value;
            fs::write(work.join("inspect.json"), bad.to_string()).unwrap();
            assert!(local(&work).is_err());
        }
        fs::write(work.join("inspect.json"), image.to_string()).unwrap();
        fs::write(work.join("probe.stdout"), "wrong version").unwrap();
        assert!(local(&work).is_err());
        fs::write(
            work.join("source/docker/wallet/Dockerfile.kube-cdk"),
            "changed",
        )
        .unwrap();
        assert!(load(&work).is_err());
    }
}
