use super::*;

#[test]
fn explicit_versions_bind_recipes_receipts_and_exact_probe_output() {
    let output = "Nutshell, version 0.21.0\nUsage: cashu [OPTIONS] COMMAND [ARGS]...\nUsage: mint-cli [OPTIONS] COMMAND [ARGS]...\n";
    assert_eq!(
        selector("nutshell-mint@0.21.0").unwrap(),
        ("nutshell-mint", "0.21.0")
    );
    assert_eq!(
        selector("nutshell-mint").unwrap(),
        ("nutshell-mint", "0.20.3")
    );
    assert!(valid_probe_version("nutshell-mint", "0.21.0", output));
    assert!(!valid_probe_version("nutshell-mint", "0.20.3", output));
    for name in ["cdk-mint", "cdk-cli-wallet"] {
        assert_eq!(
            selector(&format!("{name}@0.18.2")).unwrap(),
            (name, "0.18.2")
        );
        assert_eq!(
            selector(&format!("{name}@0.18.1")).unwrap(),
            (name, "0.18.1")
        );
        assert_eq!(
            selector(&format!("{name}@0.18.0")).unwrap(),
            (name, "0.18.0")
        );
        assert!(selector(&format!("{name}@0.17.7")).is_err());
        assert!(selector(&format!("{name}@0.17.6")).is_err());
    }
    for selector_value in [
        "nutshell-mint@latest",
        "nutshell-mint@0.21.0;false",
        "nutshell-mint@../Dockerfile",
        "nutshell-mint@0.22.0",
    ] {
        assert!(selector(selector_value).is_err());
    }
    let mut record = receipt();
    record.repository = "nutshell-mint".into();
    record.tag = format!("{NAMESPACE}/nutshell-mint:upload-{}", record.publication_id);
    if let Input::Build { version, .. } = &mut record.input {
        *version = Some("0.21.0".into());
    }
    record.validate().unwrap();
    assert_eq!(
        record.recipe().unwrap(),
        "docker/mint/Dockerfile.nutshell-0.21.0"
    );
    let saved = serde_json::to_value(&record).unwrap();
    assert_eq!(saved["input"]["version"], "0.21.0");
    let mut legacy = saved;
    legacy["input"].as_object_mut().unwrap().remove("version");
    let legacy: Receipt = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.version().unwrap(), "0.20.3");
    assert_eq!(
        legacy.recipe().unwrap(),
        "docker/mint/Dockerfile.kube-nutshell"
    );
}

/// Recipe, version, provenance record and exact native probe output.
pub(super) const ADDED_VERSIONS: &[(&str, &str, &str, &str)] = &[
    (
        "cdk-bark-processor",
        "0.1.0-fe468ca",
        "docker/payment/cdk-bark-provenance.json",
        "fe468cad486157683eddbc0df4ff87ba71b6c0a3\nabc3f967d754cdf5e484bf434ef52fa216dfb37cdcbb8fd896477e5c7b40321c\n",
    ),
    (
        "bark-server",
        "0.7.0-6188e2d",
        "docker/payment/bark-server-provenance.json",
        "captaind 0.7.0-dev+6188e2d809f193716b2e571274179f069d9c19ca\n6188e2d809f193716b2e571274179f069d9c19ca\n",
    ),
    (
        "cln-hold",
        "26.06.7-hold.0.3.3",
        "docker/payment/cln-hold-provenance.json",
        "v26.06.7\naf0055b132f3b9f24d0b1d478a15005fcf8f014f\n",
    ),
    (
        "ldk-server",
        "0.1.0-50fe752",
        "docker/payment/ldk-server-provenance.json",
        "ldk-server 0.1.0\nldk-server-cli 0.1.0\n",
    ),
    (
        "cdk-ldk-server-processor",
        "0.1.0-fe468ca",
        "docker/payment/cdk-ldk-server-provenance.json",
        "fe468cad486157683eddbc0df4ff87ba71b6c0a3\n",
    ),
    (
        "cdk-mint",
        "0.18.1",
        "docker/mint/cdk-0.18.1-provenance.json",
        "cdk-mint-rpc 0.18.1\ncdk-mintd 0.18.1\n",
    ),
    (
        "cdk-cli-wallet",
        "0.18.1",
        "docker/wallet/cdk-cli-0.18.1-provenance.json",
        "cdk-cli 0.18.1\n",
    ),
    (
        "cdk-mint",
        "0.18.2",
        "docker/mint/cdk-0.18.2-provenance.json",
        "cdk-mint-rpc 0.18.2\ncdk-mintd 0.18.2\n",
    ),
    (
        "cdk-cli-wallet",
        "0.18.2",
        "docker/wallet/cdk-cli-0.18.2-provenance.json",
        "cdk-cli 0.18.2\n",
    ),
    (
        "bitcoin-core",
        "29.4",
        "docker/bitcoin/bitcoin-29.4-provenance.json",
        "Bitcoin Core daemon version v29.4.0\n",
    ),
    (
        "bitcoin-core",
        "30.3",
        "docker/bitcoin/bitcoin-30.3-provenance.json",
        "Bitcoin Core daemon version v30.3.0 bitcoind\n",
    ),
    (
        "nutshell-mint",
        "0.21.0",
        "docker/mint/nutshell-0.21.0-provenance.json",
        "Nutshell, version 0.21.0\nUsage: cashu [OPTIONS] COMMAND [ARGS]...\nUsage: mint-cli [OPTIONS] COMMAND [ARGS]...\n",
    ),
    (
        "ldk-server",
        "0.1.0-762bc47",
        "docker/payment/ldk-server-762bc47-provenance.json",
        "ldk-server 0.1.0 (unknown)\nldk-server-cli 0.1.0 (unknown)\n762bc473342c8f945475785ec6743a9b26f56aff\n",
    ),
    (
        "spark-operator",
        "0.0.0-5e0c879",
        "docker/payment/spark-operator-provenance.json",
        "spark-frost-signer 0.1.0\n5e0c879de2775f6a34aea6d3b0c455d065a3a70f\n",
    ),
    (
        "open-ssp",
        "0.1.0-9c25f69",
        "docker/payment/open-ssp-provenance.json",
        "9c25f69924490c22a66f03a98e5840015383dc79\n",
    ),
    (
        "cdk-spark-processor",
        "0.1.0-d7244e2",
        "docker/payment/cdk-spark-provenance.json",
        "d7244e261506b51d5b309a558bf19cd4ffc32f13\n5c2bdd606e691c48adb31fd621081aeecf53d5550ee5fa1c3e1262c1afb053a6\n499a3baa6e432020bf71ecf0656fdc4c3cb91acae89adaa6c5ae7032fe7c280d\n",
    ),
];

#[test]
fn added_version_recipes_match_their_provenance_and_native_versions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for &(name, version, provenance, output) in ADDED_VERSIONS {
        let recipe = versioned_recipe(name, version).unwrap();
        let record: proofstorm_core::BuildProvenance =
            serde_json::from_slice(&fs::read(root.join(provenance)).unwrap()).unwrap();
        assert_eq!(
            record.recipe_digest,
            format!(
                "sha256:{:x}",
                Sha256::digest(fs::read(root.join(recipe)).unwrap())
            ),
            "{name}@{version}"
        );
        assert!(valid_probe_version(name, version, output));
        assert_eq!(
            valid_probe(name, output),
            legacy_version(name).unwrap() == version
        );
    }
}

#[test]
fn current_catalog_has_one_cdk_mint_recipe_and_old_receipts_remain_readable() {
    assert_eq!(
        RECIPES
            .iter()
            .filter(|name| name.ends_with("-mint") && name.starts_with("cdk"))
            .copied()
            .collect::<Vec<_>>(),
        ["cdk-mint"]
    );
    assert!(recipe("cdk-mint-management").is_ok());
    assert!(
        prepare(
            Path::new("missing"),
            Path::new("missing-output"),
            "cdk-mint-management",
            "linux/arm64",
            None
        )
        .is_err()
    );
}

#[test]
fn payment_stack_build_inputs_and_probes_bind_every_source_identity() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, version) in [
        ("cdk-bark-processor", "0.1.0-fe468ca"),
        ("bark-server", "0.7.0-6188e2d"),
        ("cln-hold", "26.06.7-hold.0.3.3"),
        ("spark-operator", "0.0.0-5e0c879"),
        ("open-ssp", "0.1.0-9c25f69"),
        ("cdk-spark-processor", "0.1.0-d7244e2"),
        ("ldk-server", "0.1.0-762bc47"),
    ] {
        assert!(bound_inputs(name, version).is_some(), "{name}@{version}");
        let recipe_path = versioned_recipe(name, version).unwrap();
        let hash = format!(
            "{:x}",
            Sha256::digest(fs::read(root.join(recipe_path)).unwrap())
        );
        verify_build_inputs(&root, name, version, &hash).unwrap();
        assert!(verify_build_inputs(&root, name, version, &"0".repeat(64)).is_err());
        assert!(selector(&format!("{name}@latest")).is_err());
        assert!(!valid_probe_version(name, version, "wrong revision"));
    }
    // The legacy LDK Server recipe predates bound provenance and stays unbound.
    assert!(bound_inputs("ldk-server", "0.1.0-50fe752").is_none());
    assert!(!valid_probe_version(
        "ldk-server",
        "0.1.0-762bc47",
        "ldk-server 0.1.0\nldk-server-cli 0.1.0\n"
    ));
    assert!(!valid_probe(
        "cdk-spark-processor",
        "d7244e261506b51d5b309a558bf19cd4ffc32f13\n5c2bdd606e691c48adb31fd621081aeecf53d5550ee5fa1c3e1262c1afb053a6"
    ));
    assert!(!valid_probe(
        "cdk-bark-processor",
        "fe468cad486157683eddbc0df4ff87ba71b6c0a3"
    ));
    assert!(!valid_probe("cln-hold", "v26.06.7\nwrong hold revision"));
    assert!(!valid_probe(
        "bark-server",
        "captaind 0.7.0-dev+wrong\n6188e2d809f193716b2e571274179f069d9c19ca"
    ));
    for (name, version) in [
        ("cdk-bark-processor", "0.1.0-fe468ca"),
        ("cdk-spark-processor", "0.1.0-d7244e2"),
    ] {
        let (provenance, patches) = bound_inputs(name, version).unwrap();
        let recipe_path = recipe(name).unwrap();
        let hash = format!(
            "{:x}",
            Sha256::digest(fs::read(root.join(recipe_path)).unwrap())
        );
        for drifted in patches {
            let work = tempfile::tempdir().unwrap();
            fs::create_dir_all(work.path().join("docker/payment/patches")).unwrap();
            for path in patches.iter().chain([&recipe_path, &provenance]) {
                fs::copy(root.join(path), work.path().join(path)).unwrap();
            }
            verify_build_inputs(work.path(), name, version, &hash).unwrap();
            fs::write(work.path().join(drifted), "drifted patch").unwrap();
            assert!(verify_build_inputs(work.path(), name, version, &hash).is_err());
        }
    }
    // A secondary input named in the provenance must be checked by the recipe.
    let work = tempfile::tempdir().unwrap();
    fs::create_dir_all(work.path().join("docker/payment")).unwrap();
    let (provenance, _) = bound_inputs("open-ssp", "0.1.0-9c25f69").unwrap();
    fs::copy(
        root.join("docker/payment/Dockerfile.open-ssp"),
        work.path().join("docker/payment/Dockerfile.open-ssp"),
    )
    .unwrap();
    let mut record: Value =
        serde_json::from_slice(&fs::read(root.join(provenance)).unwrap()).unwrap();
    record["transformations"]
        .as_array_mut()
        .unwrap()
        .push(format!("Unchecked vendor archive sha256:{}", "1".repeat(64)).into());
    fs::write(work.path().join(provenance), record.to_string()).unwrap();
    let hash = format!(
        "{:x}",
        Sha256::digest(fs::read(root.join("docker/payment/Dockerfile.open-ssp")).unwrap())
    );
    assert!(verify_build_inputs(work.path(), "open-ssp", "0.1.0-9c25f69", &hash).is_err());
}

#[test]
fn probe_outputs_match_reviewed_versions_and_the_cdk_rpc_binary_name() {
    for (name, output) in [
        (
            "bitcoin-core",
            "Bitcoin Core daemon version v31.1.0 bitcoind\nCopyright\n",
        ),
        ("cdk-cli-wallet", "cdk-cli 0.18.0\n"),
        ("cocod-wallet", "0.0.17\n"),
        (
            "nutshell-mint",
            "Nutshell, version 0.20.3\nUsage: cashu [OPTIONS] COMMAND [ARGS]...\nUsage: mint-cli [OPTIONS] COMMAND [ARGS]...\n",
        ),
        (
            "nutshell-mint-management",
            "Nutshell, version 0.20.3\nUsage: cashu [OPTIONS] COMMAND [ARGS]...\nUsage: mint-cli [OPTIONS] COMMAND [ARGS]...\n",
        ),
        ("cdk-mint", "cdk-mint-rpc 0.18.0\ncdk-mintd 0.18.0\n"),
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
        "nutshell-mint-management",
        "Usage: mint-cli [OPTIONS] COMMAND [ARGS]...\n--help\n"
    ));
    assert!(!valid_probe(
        "bitcoin-core",
        "Bitcoin Core daemon version v31.10.0 bitcoind\n"
    ));
    assert!(!valid_probe(
        "cdk-mint-management",
        "cdk-mint-rpc 0.18.0\ncdk-mintd 0.17.0\n"
    ));
}

#[test]
fn bitcoin_probes_require_the_selected_daemon_and_exact_release_banner() {
    for (version, banner) in [
        ("29.4", "Bitcoin Core daemon version v29.4.0"),
        ("30.3", "Bitcoin Core daemon version v30.3.0 bitcoind"),
        ("31.1", "Bitcoin Core daemon version v31.1.0 bitcoind"),
    ] {
        assert!(valid_probe_version("bitcoin-core", version, banner));
        assert!(!valid_probe_version(
            "bitcoin-core",
            version,
            &format!("{banner}rc1")
        ));
        assert!(!valid_probe_version(
            "bitcoin-core",
            version,
            &format!("Bitcoin Core RPC client version v{version}.0")
        ));
        for other in ["29.4", "30.3", "31.1"] {
            if other != version {
                assert!(!valid_probe_version("bitcoin-core", other, banner));
            }
        }
    }
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
            version: None,
        },
        local_image_id: None,
        local_verified: false,
        publication: Publication::Prepared,
        image: None,
        release_ready: false,
    }
}

#[test]
fn mint_image_copies_allow_reviewed_renames_and_keep_historical_receipts_valid() {
    for (old, current) in [
        ("cdk-ldk-mint-management", "cdk-mint"),
        ("nutshell-mint-management", "nutshell-mint"),
    ] {
        for destination in [old, current] {
            let mut copy = receipt();
            copy.repository = destination.into();
            copy.tag = format!("{NAMESPACE}/{destination}:upload-{}", copy.publication_id);
            copy.input = Input::Copy {
                image: format!("{NAMESPACE}/{old}@sha256:{}", "b".repeat(64)),
            };
            copy.validate().unwrap();
            let saved = serde_json::to_string(&copy).unwrap();
            let recovered: Receipt = serde_json::from_str(&saved).unwrap();
            recovered.validate().unwrap();
            assert_eq!(serde_json::to_string(&recovered).unwrap(), saved);
        }
    }
    // The old non-LDK image must not become the consolidated CDK image.
    for source in [
        "cdk-mint-management",
        "nutshell-mint-management",
        "cdk-cli-wallet",
    ] {
        let mut copy = receipt();
        copy.repository = "cdk-mint".into();
        copy.tag = format!("{NAMESPACE}/cdk-mint:upload-{}", copy.publication_id);
        copy.input = Input::Copy {
            image: format!("{NAMESPACE}/{source}@sha256:{}", "b".repeat(64)),
        };
        assert!(copy.validate().is_err());
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
