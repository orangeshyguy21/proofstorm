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

/// The reviewed native probe output of one candidate recipe.
fn probe(name: &str, version: &str) -> &'static str {
    super::super::tests::ADDED_VERSIONS
        .iter()
        .find(|(recipe, reviewed, _, _)| *recipe == name && *reviewed == version)
        .map(|(_, _, _, output)| *output)
        .expect("reviewed probe output")
}

/// Recipe, provenance record and patches a candidate is bound to.
fn bound(image: &Image) -> Vec<&'static str> {
    let (provenance, patches) = bound_inputs(image.repository, image.version).unwrap();
    let mut inputs = vec![
        versioned_recipe(image.repository, image.version).unwrap(),
        provenance,
    ];
    inputs.extend(patches);
    inputs
}

/// Native host as the image build script records it.
const LINUX: (&str, &str) = ("Linux", "");
const APPLE: (&str, &str) = ("Darwin", "arm64");

#[allow(
    clippy::too_many_lines,
    reason = "fixture builds every complete source-bound handoff of one stack"
)]
fn fixture(base: &Path, stack: &Stack, platform: &str, host: (&str, &str)) -> String {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = base.join("root");
    let mut inputs = vec!["release/ghcr.json"];
    for image in stack.images {
        inputs.extend(bound(image));
    }
    for input in inputs {
        let path = root.join(input);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(checkout.join(input), path).unwrap();
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
    let arch = platform.strip_prefix("linux/").unwrap();
    let machine = if arch == "amd64" { "x86_64" } else { "aarch64" };
    let (os, host_machine) = if host.0 == "Linux" {
        ("Linux", machine)
    } else {
        host
    };
    for image in stack.images {
        let (name, version) = (image.repository, image.version);
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
        fs::write(work.join("probe.stdout"), probe(name, version)).unwrap();
        super::super::local(&work).unwrap();
        let artifact = artifacts.join(format!(
            "{}-image-{name}@{version}-{arch}-{revision}-1",
            stack.name
        ));
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
            "host":{"os":os,"machine":host_machine},"docker":{"engine":format!("linux/{machine}")},"managed_qualification":false,"published":false}),
        );
        checksums(&artifact);
    }
    revision
}

/// Copy the loopback registry's manifest/config into restored candidates, as
/// the qualification script does after pushing each image.
fn pushed(base: &Path, stack: &Stack, restored: &Path) {
    for image in stack.images {
        for file in ["manifest.json", "config.json"] {
            fs::copy(
                base.join(image.repository).join(file),
                restored.join(image.repository).join(file),
            )
            .unwrap();
        }
    }
}

#[test]
fn spark_table_matches_the_phase_five_contract() {
    let spark = stack("spark").unwrap();
    assert_eq!(spark.gate, "spark-processor");
    assert_eq!(spark.pins, "crates/proofstorm-core/src/spark_images.json");
    let table: Vec<_> = spark
        .images
        .iter()
        .map(|image| (image.repository, image.version, image.key))
        .collect();
    assert_eq!(
        table,
        [
            ("spark-operator", "0.0.0-5e0c879", "operators"),
            ("open-ssp", "0.1.0-9c25f69", "ssp"),
            ("cdk-spark-processor", "0.1.0-d7244e2", "processor"),
            ("ldk-server", "0.1.0-762bc47", "ldk_server"),
        ]
    );
    let bark = stack("bark").unwrap();
    assert_eq!(bark.gate, "bark-processor");
    assert_eq!(bark.pins, "crates/proofstorm-core/src/bark_images.json");
    assert!(stack("lnd").is_err());
    let described = spark.describe();
    assert_eq!(described["gate"], "spark-processor");
    assert_eq!(described["images"][0]["catalog_id"], "spark-operators");
    assert_eq!(described["images"][3]["repository"], "ldk-server");
}

#[test]
fn every_stack_image_is_a_bound_reviewed_recipe_run_as_non_root() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for stack in &STACKS {
        assert!(checkout.join(stack.pins).is_file(), "{}", stack.pins);
        for image in stack.images {
            assert!(super::super::RECIPES.contains(&image.repository));
            for input in bound(image) {
                assert!(checkout.join(input).is_file(), "{input}");
            }
            let recipe =
                fs::read_to_string(checkout.join(bound(image)[0])).expect("reviewed recipe");
            assert!(recipe.contains("USER 1000:1000"), "{}", image.repository);
        }
    }
}

#[test]
fn build_script_and_image_workflows_select_exactly_each_stack_table() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = fs::read_to_string(checkout.join("scripts/stack-image-build.sh")).unwrap();
    for stack in &STACKS {
        let expected: Vec<_> = stack
            .images
            .iter()
            .map(|image| format!("{}@{}", image.repository, image.version))
            .collect();
        let prefix = format!("  {}) ", stack.name);
        let line = script
            .lines()
            .find(|line| line.starts_with(&prefix))
            .expect("stack row in the build script");
        let listed: Vec<_> = line
            .split(['(', ')', ' '])
            .filter(|word| word.contains('@'))
            .collect();
        assert_eq!(listed, expected, "{}", stack.name);
        let workflow = fs::read_to_string(
            checkout.join(format!(".github/workflows/{}-images.yml", stack.name)),
        )
        .unwrap();
        let listed: Vec<_> = workflow
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- "))
            .filter(|item| item.contains('@') && !item.contains(':'))
            .collect();
        assert_eq!(listed, expected, "{}", stack.name);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pass per stack and platform keeps every refusal beside its fixture"
)]
fn native_handoff_reuses_exact_images_but_refuses_tampering_and_changed_inputs() {
    for stack in &STACKS {
        for platform in ["linux/amd64", "linux/arm64"] {
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().canonicalize().unwrap();
            let revision = fixture(&base, stack, platform, LINUX);
            let root = base.join("root");
            let artifacts = base.join("artifacts");
            let restored = base.join("restored");
            let restore = |output: &Path, revision: &str| {
                super::restore(
                    stack,
                    Hosts::Runner,
                    &root,
                    platform,
                    revision,
                    "1",
                    &artifacts,
                    output,
                )
            };
            restore(&restored, &revision).unwrap();
            pushed(&base, stack, &restored);
            let stage = |output: &str| {
                super::stage(
                    stack,
                    Hosts::Runner,
                    &root,
                    platform,
                    &restored,
                    &base.join(output),
                )
            };
            stage("staged").unwrap();
            let pins = bundle::read_json(&base.join("staged").join(stack.pins_file())).unwrap();
            assert_eq!(pins.as_object().unwrap().len(), 1);
            let keys: BTreeSet<_> = pins[platform]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                stack.images.iter().map(|image| image.key).collect(),
                "{}",
                stack.name
            );
            for image in stack.images {
                let pin = pins[platform][image.key].as_str().unwrap();
                let digest = pin
                    .strip_prefix(&format!(
                        "proofstorm-registry.localhost:5000/{}@sha256:",
                        image.repository
                    ))
                    .unwrap();
                assert!(sha256(digest), "{pin}");
            }
            // The other stack's pins file is never written.
            for other in STACKS.iter().filter(|other| other.name != stack.name) {
                assert!(!base.join("staged").join(other.pins_file()).exists());
            }
            assert!(restore(&base.join("foreign"), &"f".repeat(40)).is_err());
            let work = restored.join(stack.images[1].repository);
            let original_config = fs::read(work.join("config.json")).unwrap();
            fs::write(work.join("config.json"), b"{}").unwrap();
            assert!(stage("bad-config").is_err());
            fs::write(work.join("config.json"), original_config).unwrap();
            let native_path = work.join("native.json");
            let mut native = bundle::read_json(&native_path).unwrap();
            let original_native = native.clone();
            native["host"]["machine"] = json!("emulated");
            put(&native_path, &native);
            assert!(stage("emulated").is_err());
            put(&native_path, &original_native);
            // Every bound input of the processor: recipe, provenance and patches.
            let processor = stack
                .images
                .iter()
                .find(|image| image.key == "processor")
                .unwrap();
            for (index, input) in bound(processor).into_iter().enumerate() {
                let original = fs::read(root.join(input)).unwrap();
                fs::write(root.join(input), b"changed input").unwrap();
                assert!(stage(&format!("changed-{index}")).is_err(), "{input}");
                fs::write(root.join(input), original).unwrap();
            }
            stage("unchanged").unwrap();
            let artifact = fs::read_dir(&artifacts)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            fs::write(artifact.join("image.tar"), b"substituted image").unwrap();
            assert!(handoff_checksums(&artifact, stack.label).is_err());
            checksums(&artifact);
            let sums = fs::read_to_string(artifact.join("SHA256SUMS")).unwrap();
            fs::write(
                artifact.join("SHA256SUMS"),
                format!("{sums}{}", sums.lines().next().unwrap()),
            )
            .unwrap();
            assert!(handoff_checksums(&artifact, stack.label).is_err());
        }
    }
}

#[test]
fn restore_requires_exactly_this_stacks_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let bark = stack("bark").unwrap();
    let revision = fixture(&base, bark, "linux/arm64", LINUX);
    // Three Bark handoffs are neither a Spark set nor renamed into one.
    let spark = stack("spark").unwrap();
    assert!(
        restore(
            spark,
            Hosts::Runner,
            &base.join("root"),
            "linux/arm64",
            &revision,
            "1",
            &base.join("artifacts"),
            &base.join("spark")
        )
        .is_err()
    );
    assert!(!base.join("spark").exists());
}

#[test]
fn apple_silicon_candidates_are_accepted_only_for_an_explicit_local_run() {
    let spark = stack("spark").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let revision = fixture(&base, spark, "linux/arm64", APPLE);
    let root = base.join("root");
    let artifacts = base.join("artifacts");
    let restore = |hosts, output: &str| {
        super::restore(
            spark,
            hosts,
            &root,
            "linux/arm64",
            &revision,
            "1",
            &artifacts,
            &base.join(output),
        )
    };
    assert!(restore(Hosts::Runner, "runner").is_err());
    restore(Hosts::Local, "local").unwrap();
    let restored = base.join("local");
    pushed(&base, spark, &restored);
    let stage = |hosts, output: &str| {
        super::stage(
            spark,
            hosts,
            &root,
            "linux/arm64",
            &restored,
            &base.join(output),
        )
    };
    assert!(stage(Hosts::Runner, "staged-runner").is_err());
    stage(Hosts::Local, "staged").unwrap();
    // The receipt keeps the real host; it never claims a Linux runner.
    let candidates = bundle::read_json(&base.join("staged/candidates.json")).unwrap();
    assert_eq!(candidates["images"].as_array().unwrap().len(), 4);
    let native = bundle::read_json(&base.join("staged/open-ssp/native.json")).unwrap();
    assert_eq!(native["host"], json!({"os":"Darwin","machine":"arm64"}));
    // Apple silicon never stands in for AMD64, and only arm64 Macs qualify.
    let mut native =
        json!({"host":{"os":"Darwin","machine":"arm64"},"docker":{"engine":"linux/amd64"}});
    assert!(!native_host(&native, "linux/amd64", Hosts::Local).unwrap());
    native["host"]["machine"] = json!("x86_64");
    native["docker"]["engine"] = json!("linux/arm64");
    assert!(!native_host(&native, "linux/arm64", Hosts::Local).unwrap());
    native["host"] = json!({"os":"Linux","machine":"aarch64"});
    assert!(native_host(&native, "linux/arm64", Hosts::Runner).unwrap());
    native["docker"]["engine"] = json!("linux/amd64");
    assert!(!native_host(&native, "linux/arm64", Hosts::Local).unwrap());
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
#[allow(
    clippy::too_many_lines,
    reason = "every acceptance, gate, lock and evidence-name refusal shares one run fixture"
)]
fn managed_receipt_requires_exact_images_complete_gate_and_cleanup_and_omits_secrets() {
    for stack in &STACKS {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let staged = base.join("staged");
        let run = base.join("run");
        fs::create_dir(&staged).unwrap();
        fs::create_dir(&run).unwrap();
        let images: Vec<_> = stack
            .images
            .iter()
            .map(|image| json!({"repository":image.repository,"version":image.version,"image":format!("{}@sha256:{}", image.repository, "a".repeat(64))}))
            .collect();
        put(
            &staged.join("candidates.json"),
            &json!({"images":images,"managed_qualification":false,"published":false}),
        );
        let evidence_file = |kind: &str| run.join(format!("{}-{kind}.json", stack.name));
        let acceptance = json!({"setup":"passed","cleanup":"passed","preservation":"passed","cleanup_errors":[],"gates":[{"name":stack.gate,"status":"passed"}],"private":"DO_NOT_EXPORT"});
        put(&run.join("acceptance.json"), &acceptance);
        put(
            &evidence_file("result"),
            &json!({"passed":true,"exercise_error":null,"cleanup_error":null}),
        );
        put(
            &evidence_file("storage-cleanup"),
            &json!({"remaining_volumes":0}),
        );
        // The lock names catalog entries, which need not match the image repository.
        let entries: Vec<_> = stack
            .images
            .iter()
            .zip(&images)
            .map(|(expected, image)| json!({"catalog_id":expected.catalog_id,"version":image["version"],"image":image["image"]}))
            .collect();
        let plan = json!({"lock":{"entries":entries},"private":"DO_NOT_EXPORT"});
        put(&evidence_file("plan"), &plan);
        let output = base.join("public.json");
        evidence(stack, &staged, &run, &output).unwrap();
        let public = fs::read_to_string(&output).unwrap();
        assert!(!public.contains("DO_NOT_EXPORT"));
        let report: Value = serde_json::from_str(&public).unwrap();
        let hashed: Vec<_> = report["evidence_sha256"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut expected = vec![
            "acceptance.json".to_owned(),
            format!("{}-plan.json", stack.name),
            format!("{}-result.json", stack.name),
            format!("{}-storage-cleanup.json", stack.name),
        ];
        expected.sort();
        assert_eq!(hashed, expected);
        for field in ["setup", "cleanup", "preservation", "gates"] {
            let mut bad = acceptance.clone();
            bad[field] = json!("failed");
            put(&run.join("acceptance.json"), &bad);
            assert!(evidence(stack, &staged, &run, &base.join(field)).is_err());
        }
        // Only this stack's gate, alone, qualifies its images.
        for gates in [
            json!([{"name":stack.gate,"status":"passed"},{"name":"smoke","status":"passed"}]),
            json!([]),
        ]
        .into_iter()
        .chain(
            STACKS
                .iter()
                .filter(|other| other.name != stack.name)
                .map(|other| json!([{"name":other.gate,"status":"passed"}])),
        ) {
            let mut bad = acceptance.clone();
            bad["gates"] = gates;
            put(&run.join("acceptance.json"), &bad);
            assert!(evidence(stack, &staged, &run, &base.join("wrong-gate")).is_err());
        }
        put(&run.join("acceptance.json"), &acceptance);
        let mut bad = plan.clone();
        bad["lock"]["entries"][0]["image"] = json!("substituted");
        put(&evidence_file("plan"), &bad);
        assert!(evidence(stack, &staged, &run, &base.join("wrong-image")).is_err());
        let mut bad = plan.clone();
        bad["lock"]["entries"][0]["catalog_id"] = json!("foreign");
        put(&evidence_file("plan"), &bad);
        assert!(evidence(stack, &staged, &run, &base.join("wrong-entry")).is_err());
        put(&evidence_file("plan"), &plan);
        put(
            &evidence_file("storage-cleanup"),
            &json!({"remaining_volumes":1}),
        );
        assert!(evidence(stack, &staged, &run, &base.join("leftover-volume")).is_err());
        put(
            &evidence_file("storage-cleanup"),
            &json!({"remaining_volumes":0}),
        );
        evidence(stack, &staged, &run, &base.join("again")).unwrap();
        // Another stack's evidence file names never stand in for this stack's.
        fs::remove_file(evidence_file("result")).unwrap();
        for other in STACKS.iter().filter(|other| other.name != stack.name) {
            put(
                &run.join(format!("{}-result.json", other.name)),
                &json!({"passed":true,"exercise_error":null,"cleanup_error":null}),
            );
        }
        assert!(evidence(stack, &staged, &run, &base.join("foreign-evidence")).is_err());
    }
}

/// A passing gate run for `stack`, as `evidence` reads it.
fn passing_run(base: &Path, stack: &Stack) -> (std::path::PathBuf, std::path::PathBuf) {
    let (staged, run) = (base.join("staged"), base.join("run"));
    fs::create_dir(&staged).unwrap();
    fs::create_dir(&run).unwrap();
    let image = |image: &Image| format!("{}@sha256:{}", image.repository, "b".repeat(64));
    let images: Vec<_> = stack
        .images
        .iter()
        .map(|candidate| json!({"repository":candidate.repository,"version":candidate.version,"image":image(candidate)}))
        .collect();
    put(&staged.join("candidates.json"), &json!({"images":images}));
    put(
        &run.join("acceptance.json"),
        &json!({"setup":"passed","cleanup":"passed","preservation":"passed","cleanup_errors":[],"gates":[{"name":stack.gate,"status":"passed"}]}),
    );
    put(
        &run.join(format!("{}-result.json", stack.name)),
        &json!({"passed":true,"exercise_error":null,"cleanup_error":null}),
    );
    put(
        &run.join(format!("{}-storage-cleanup.json", stack.name)),
        &json!({"remaining_volumes":0}),
    );
    let entries: Vec<_> = stack
        .images
        .iter()
        .map(|candidate| json!({"catalog_id":candidate.catalog_id,"version":candidate.version,"image":image(candidate)}))
        .collect();
    put(
        &run.join(format!("{}-plan.json", stack.name)),
        &json!({"lock":{"entries":entries}}),
    );
    (staged, run)
}

#[test]
fn bark_command_names_remain_and_only_restore_and_stage_accept_local_hosts() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (staged, run) = passing_run(&base, stack("bark").unwrap());
    let path = |path: &Path| path.to_str().unwrap().to_owned();
    let (staged, run) = (path(&staged), path(&run));
    let output = |name: &str| path(&base.join(name));
    // Other catalog-image commands, and handoff commands of the wrong shape, fall through.
    assert!(cli(&["list"]).is_none());
    assert!(cli(&["stack-evidence", "bark", &staged, &run]).is_none());
    assert!(
        cli(&[
            "stack-evidence",
            "--local",
            "bark",
            &staged,
            &run,
            &output("x")
        ])
        .is_none()
    );
    assert!(cli(&["bark-restore", "--local", "r", "p", "v", "a", "s", "o"]).is_none());
    assert!(cli(&["stack", "lnd"]).unwrap().is_err());
    cli(&["stack", "spark"]).unwrap().unwrap();
    cli(&["bark-evidence", &staged, &run, &output("alias.json")])
        .unwrap()
        .unwrap();
    cli(&[
        "stack-evidence",
        "bark",
        &staged,
        &run,
        &output("generic.json"),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(
        fs::read(base.join("alias.json")).unwrap(),
        fs::read(base.join("generic.json")).unwrap()
    );
    // Bark's run never qualifies Spark.
    assert!(
        cli(&[
            "stack-evidence",
            "spark",
            &staged,
            &run,
            &output("spark.json")
        ])
        .unwrap()
        .is_err()
    );
}

/// One PAX record, whose decimal length prefix counts itself.
fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut length = body.len() + 1;
    while length.to_string().len() + body.len() != length {
        length = length.to_string().len() + body.len();
    }
    format!("{length}{body}")
}

#[test]
fn source_archive_from_a_mac_keeps_only_its_members_and_ignores_extended_attributes() {
    // macOS tar (with COPYFILE_DISABLE) records xattrs in PAX headers, which
    // describe the next member and are never extracted themselves.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("source.tar.gz");
    let mut archive = tar::Builder::new(GzEncoder::new(
        File::create(&path).unwrap(),
        Compression::fast(),
    ));
    for (name, directory) in [("source", true), ("source/f", false), ("image.json", false)] {
        let records = pax_record("LIBARCHIVE.xattr.com.apple.provenance", "AQIAMuNtV+oJ3lA");
        let mut pax = tar::Header::new_ustar();
        pax.set_entry_type(tar::EntryType::XHeader);
        pax.set_path(format!("PaxHeader/{name}")).unwrap();
        pax.set_mode(0o644);
        pax.set_size(records.len() as u64);
        pax.set_cksum();
        archive.append(&pax, records.as_bytes()).unwrap();
        let mut header = tar::Header::new_ustar();
        if directory {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            archive.append_data(&mut header, name, &b""[..]).unwrap();
        } else {
            header.set_mode(0o644);
            header.set_size(2);
            archive.append_data(&mut header, name, &b"{}"[..]).unwrap();
        }
    }
    archive.into_inner().unwrap().finish().unwrap();
    let out = tmp.path().join("out");
    extract(&path, &out).unwrap();
    let mut names: Vec<_> = walk(&out);
    names.sort();
    assert_eq!(names, ["image.json", "source", "source/f"]);
}

fn walk(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            names.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    names
}
