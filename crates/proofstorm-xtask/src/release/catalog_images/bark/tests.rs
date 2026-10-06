use super::*;
use flate2::{Compression, write::GzEncoder};
use std::fmt::Write as _;
use std::process::Command;

fn put(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn checksums(artifact: &Path) {
    let mut sums = String::new();
    for name in HANDOFF {
        writeln!(sums, "{}  {name}", digest(&artifact.join(name)).unwrap()).unwrap();
    }
    fs::write(artifact.join("SHA256SUMS"), sums).unwrap();
}

#[allow(
    clippy::too_many_lines,
    reason = "fixture builds three complete source-bound handoffs"
)]
fn fixture(base: &Path, platform: &str) -> String {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = base.join("root");
    fs::create_dir_all(root.join("docker/payment/patches")).unwrap();
    fs::create_dir(root.join("release")).unwrap();
    for input in [
        "release/ghcr.json",
        "docker/payment/Dockerfile.bark-server",
        "docker/payment/Dockerfile.cdk-bark",
        "docker/payment/Dockerfile.cln-hold",
        "docker/payment/bark-server-provenance.json",
        "docker/payment/cdk-bark-provenance.json",
        "docker/payment/cln-hold-provenance.json",
        "docker/payment/patches/bark-regtest-rpc.patch",
    ] {
        fs::copy(checkout.join(input), root.join(input)).unwrap();
    }
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
    let revision = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let artifacts = base.join("artifacts");
    fs::create_dir(&artifacts).unwrap();
    for (name, version, _) in IMAGES {
        let work = base.join(name);
        super::super::prepare(
            &root.canonicalize().unwrap(),
            &work,
            &format!("{name}@{version}"),
            platform,
            None,
        )
        .unwrap();
        let receipt = bundle::read_json(&work.join("image.json")).unwrap();
        let arch = platform.strip_prefix("linux/").unwrap();
        let config = json!({"os":"linux","architecture":arch,"config":{"User":"1000:1000","Labels":{"dev.proofstorm.source-sha256":receipt["input"]["source"]["sha256"]}},"rootfs":{"diff_ids":[]}});
        put(&work.join("config.json"), &config);
        let id = format!("sha256:{}", digest(&work.join("config.json")).unwrap());
        put(
            &work.join("manifest.json"),
            &json!({"schemaVersion":2,"config":{"digest":id},"layers":[]}),
        );
        put(
            &work.join("inspect.json"),
            &json!([{"Id":id,"Os":"linux","Architecture":arch,"Config":config["config"],"RootFS":{"Layers":[]}}]),
        );
        let probe = match name {
            "cdk-bark-processor" => {
                "fe468cad486157683eddbc0df4ff87ba71b6c0a3\nabc3f967d754cdf5e484bf434ef52fa216dfb37cdcbb8fd896477e5c7b40321c"
            }
            "bark-server" => {
                "captaind 0.7.0-dev+6188e2d809f193716b2e571274179f069d9c19ca\n6188e2d809f193716b2e571274179f069d9c19ca"
            }
            _ => "v26.06.7\naf0055b132f3b9f24d0b1d478a15005fcf8f014f",
        };
        fs::write(work.join("probe.stdout"), probe).unwrap();
        super::super::local(&work).unwrap();
        let artifact = artifacts.join(format!("bark-image-{name}@{version}-{arch}-{revision}-1"));
        fs::create_dir(&artifact).unwrap();
        let gz = GzEncoder::new(
            File::create(artifact.join("work.tar.gz")).unwrap(),
            Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        tar.append_dir_all("source", work.join("source")).unwrap();
        for name in ["image.json", "inspect.json", "probe.stdout"] {
            tar.append_path_with_name(work.join(name), name).unwrap();
            fs::copy(work.join(name), artifact.join(name)).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        fs::write(artifact.join("image.tar"), b"fixture image archive").unwrap();
        fs::write(artifact.join("build.log"), b"fixture build").unwrap();
        put(
            &artifact.join("native.json"),
            &json!({"format_version":1,"platform":platform,"revision":revision,
            "host":{"os":"Linux","machine":if arch=="amd64" {"x86_64"} else {"aarch64"}},"docker":{"engine":platform},"managed_qualification":false,"published":false}),
        );
        checksums(&artifact);
    }
    revision
}

#[test]
fn native_handoff_reuses_exact_images_but_refuses_tampering_and_changed_inputs() {
    for platform in ["linux/amd64", "linux/arm64"] {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let revision = fixture(&base, platform);
        let root = base.join("root");
        let artifacts = base.join("artifacts");
        let restored = base.join("restored");
        restore(&root, platform, &revision, "1", &artifacts, &restored).unwrap();
        for (name, _, _) in IMAGES {
            for file in ["manifest.json", "config.json"] {
                fs::copy(base.join(name).join(file), restored.join(name).join(file)).unwrap();
            }
        }
        stage(&root, platform, &restored, &base.join("staged")).unwrap();
        let pins = bundle::read_json(&base.join("staged/bark_images.json")).unwrap();
        assert_eq!(pins.as_object().unwrap().len(), 1);
        assert_eq!(pins[platform].as_object().unwrap().len(), 3);
        assert!(
            restore(
                &root,
                platform,
                &"f".repeat(40),
                "1",
                &artifacts,
                &base.join("foreign")
            )
            .is_err()
        );
        let work = restored.join("bark-server");
        let original_config = fs::read(work.join("config.json")).unwrap();
        fs::write(work.join("config.json"), b"{}").unwrap();
        assert!(stage(&root, platform, &restored, &base.join("bad-config")).is_err());
        fs::write(work.join("config.json"), original_config).unwrap();
        let native_path = work.join("native.json");
        let mut native = bundle::read_json(&native_path).unwrap();
        let original_native = native.clone();
        native["host"]["machine"] = json!("emulated");
        put(&native_path, &native);
        assert!(stage(&root, platform, &restored, &base.join("emulated")).is_err());
        put(&native_path, &original_native);
        fs::write(
            root.join("docker/payment/Dockerfile.cdk-bark"),
            b"changed recipe",
        )
        .unwrap();
        assert!(stage(&root, platform, &restored, &base.join("changed-inputs")).is_err());
        let artifact = fs::read_dir(&artifacts)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(artifact.join("image.tar"), b"substituted image").unwrap();
        assert!(handoff_checksums(&artifact).is_err());
        checksums(&artifact);
        let sums = fs::read_to_string(artifact.join("SHA256SUMS")).unwrap();
        fs::write(
            artifact.join("SHA256SUMS"),
            format!("{sums}{}", sums.lines().next().unwrap()),
        )
        .unwrap();
        assert!(handoff_checksums(&artifact).is_err());
    }
}

#[test]
fn source_archive_refuses_links_duplicates_and_unexpected_files() {
    for (name, link, repeat) in [
        ("source/link", true, false),
        ("source/file", false, true),
        ("credentials.json", false, false),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("source.tar.gz");
        let mut archive = tar::Builder::new(GzEncoder::new(
            File::create(&path).unwrap(),
            Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(0);
        if link {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/tmp/foreign").unwrap();
        }
        header.set_cksum();
        archive.append_data(&mut header, name, &b""[..]).unwrap();
        if repeat {
            archive.append_data(&mut header, name, &b""[..]).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        assert!(extract(&path, &tmp.path().join("out")).is_err());
    }
}

#[test]
fn managed_receipt_requires_exact_images_complete_gate_and_cleanup_and_omits_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let staged = base.join("staged");
    let run = base.join("run");
    fs::create_dir(&staged).unwrap();
    fs::create_dir(&run).unwrap();
    let images: Vec<_> = IMAGES.iter().map(|(name, version, _)| json!({"repository":name,"version":version,"image":format!("{name}@sha256:{}", "a".repeat(64))})).collect();
    put(
        &staged.join("candidates.json"),
        &json!({"images":images,"managed_qualification":false,"published":false}),
    );
    let acceptance = json!({"setup":"passed","cleanup":"passed","preservation":"passed","cleanup_errors":[],"gates":[{"name":"bark-processor","status":"passed"}],"private":"DO_NOT_EXPORT"});
    put(&run.join("acceptance.json"), &acceptance);
    put(
        &run.join("bark-result.json"),
        &json!({"passed":true,"exercise_error":null,"cleanup_error":null}),
    );
    put(
        &run.join("bark-storage-cleanup.json"),
        &json!({"remaining_volumes":0}),
    );
    let entries: Vec<_> = images.iter().map(|image| json!({"catalog_id":image["repository"],"version":image["version"],"image":image["image"]})).collect();
    let plan = json!({"lock":{"entries":entries},"private":"DO_NOT_EXPORT"});
    put(&run.join("bark-plan.json"), &plan);
    let output = tmp.path().join("public.json");
    evidence(&staged, &run, &output).unwrap();
    assert!(
        !fs::read_to_string(&output)
            .unwrap()
            .contains("DO_NOT_EXPORT")
    );
    for field in ["setup", "cleanup", "preservation", "gates"] {
        let mut bad = acceptance.clone();
        bad[field] = json!("failed");
        put(&run.join("acceptance.json"), &bad);
        assert!(evidence(&staged, &run, &tmp.path().join(field)).is_err());
    }
    put(&run.join("acceptance.json"), &acceptance);
    let mut bad = plan.clone();
    bad["lock"]["entries"][0]["image"] = json!("substituted");
    put(&run.join("bark-plan.json"), &bad);
    assert!(evidence(&staged, &run, &tmp.path().join("wrong-image")).is_err());
    put(&run.join("bark-plan.json"), &plan);
    put(
        &run.join("bark-storage-cleanup.json"),
        &json!({"remaining_volumes":1}),
    );
    assert!(evidence(&staged, &run, &tmp.path().join("leftover-volume")).is_err());
}
