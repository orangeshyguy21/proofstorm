//! Offline candidate handoff for a payment stack's managed gate. Never publishes
//! images. Everything stack-specific is the data in [`STACKS`].
use super::{
    Input, Publication, Receipt, bound_inputs, bundle, inspect, load, sha256, text,
    verify_build_inputs, versioned_recipe,
};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};

/// One candidate image: its recipe selection, its key in the stack's pins file
/// and the catalog entry the gate's lock records for it.
pub(super) struct Image {
    pub(super) repository: &'static str,
    pub(super) version: &'static str,
    pub(super) key: &'static str,
    pub(super) catalog_id: &'static str,
}

/// A payment stack qualified by one managed gate. Its name prefixes the image
/// artifacts (`{name}-image-…`) and the gate's evidence (`{name}-result.json`,
/// `{name}-plan.json`, `{name}-storage-cleanup.json`).
pub(super) struct Stack {
    pub(super) name: &'static str,
    label: &'static str,
    pub(super) gate: &'static str,
    /// Embedded catalog pins, relative to the checkout. Staged only in a
    /// disposable checkout; never written by this module.
    pub(super) pins: &'static str,
    pub(super) images: &'static [Image],
}

pub(super) const STACKS: [Stack; 2] = [
    Stack {
        name: "bark",
        label: "Bark",
        gate: "bark-processor",
        pins: "crates/proofstorm-core/src/bark_images.json",
        images: &[
            Image {
                repository: "cdk-bark-processor",
                version: "0.1.0-fe468ca",
                key: "processor",
                catalog_id: "cdk-bark-processor",
            },
            Image {
                repository: "bark-server",
                version: "0.7.0-6188e2d",
                key: "server",
                catalog_id: "bark-server",
            },
            Image {
                repository: "cln-hold",
                version: "26.06.7-hold.0.3.3",
                key: "cln",
                catalog_id: "cln-hold",
            },
        ],
    },
    Stack {
        name: "spark",
        label: "Spark",
        gate: "spark-processor",
        pins: "crates/proofstorm-core/src/spark_images.json",
        images: &[
            // One image runs every operator of the `spark-operators` component.
            Image {
                repository: "spark-operator",
                version: "0.0.0-5e0c879",
                key: "operators",
                catalog_id: "spark-operators",
            },
            Image {
                repository: "open-ssp",
                version: "0.1.0-9c25f69",
                key: "ssp",
                catalog_id: "open-ssp",
            },
            Image {
                repository: "cdk-spark-processor",
                version: "0.1.0-d7244e2",
                key: "processor",
                catalog_id: "cdk-spark-processor",
            },
            // The macaroon-era revision open-ssp needs, not the processor's 50fe752.
            Image {
                repository: "ldk-server",
                version: "0.1.0-762bc47",
                key: "ldk_server",
                catalog_id: "ldk-server",
            },
        ],
    },
];

pub(super) fn stack(name: &str) -> Result<&'static Stack> {
    STACKS
        .iter()
        .find(|stack| stack.name == name)
        .with_context(|| format!("unknown payment stack {name}; select bark or spark"))
}

impl Stack {
    /// The staged copy of the pins file uses the embedded file's name.
    fn pins_file(&self) -> &'static str {
        self.pins.rsplit('/').next().unwrap_or(self.pins)
    }

    /// The table the qualification script reads; this module stays authoritative.
    pub(super) fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "gate": self.gate,
            "pins": self.pins,
            "images": self.images.iter().map(|image| json!({
                "repository": image.repository,
                "version": image.version,
                "key": image.key,
                "catalog_id": image.catalog_id,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Where candidates may have been built. GitHub's native Linux runners are
/// always accepted. A maintainer's local run may also use Docker Desktop's
/// native ARM64 Linux engine on Apple silicon; `native.json` records that host.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Hosts {
    Runner,
    Local,
}

/// The `catalog-image` handoff commands, or `None` for any other command:
/// `stack STACK`, `stack-restore [--local] STACK ROOT PLATFORM REVISION ATTEMPT
/// ARTIFACTS OUTPUT`, `stack-stage [--local] STACK ROOT PLATFORM CANDIDATES OUTPUT`
/// and `stack-evidence STACK STAGED RUN OUTPUT`. Bark's original `bark-restore`,
/// `bark-stage` and `bark-evidence` remain names for the `bark` stack.
pub(super) fn cli(args: &[&str]) -> Option<Result<()>> {
    let mut args = args.to_vec();
    let alias = match *args.first()? {
        "bark-restore" => Some("stack-restore"),
        "bark-stage" => Some("stack-stage"),
        "bark-evidence" => Some("stack-evidence"),
        _ => None,
    };
    if let Some(command) = alias {
        args.splice(0..1, [command, "bark"]);
    }
    // CI qualification never passes `--local`.
    let hosts =
        if args.get(1) == Some(&"--local") && matches!(args[0], "stack-restore" | "stack-stage") {
            args.remove(1);
            Hosts::Local
        } else {
            Hosts::Runner
        };
    Some(match args.as_slice() {
        ["stack", name] => stack(name).and_then(|stack| {
            println!("{}", serde_json::to_string_pretty(&stack.describe())?);
            Ok(())
        }),
        [
            "stack-restore",
            name,
            root,
            platform,
            revision,
            attempt,
            artifacts,
            output,
        ] => stack(name).and_then(|stack| {
            restore(
                stack,
                hosts,
                Path::new(root),
                platform,
                revision,
                attempt,
                Path::new(artifacts),
                Path::new(output),
            )
        }),
        ["stack-stage", name, root, platform, candidates, output] => {
            stack(name).and_then(|stack| {
                stage(
                    stack,
                    hosts,
                    Path::new(root),
                    platform,
                    Path::new(candidates),
                    Path::new(output),
                )
            })
        }
        ["stack-evidence", name, staged, run, output] => stack(name).and_then(|stack| {
            evidence(stack, Path::new(staged), Path::new(run), Path::new(output))
        }),
        _ => return None,
    })
}

const HANDOFF: [&str; 7] = [
    "image.tar",
    "work.tar.gz",
    "image.json",
    "inspect.json",
    "probe.stdout",
    "native.json",
    "build.log",
];

fn digest(path: &Path) -> Result<String> {
    crate::development::regular(path)?;
    bundle::checksum(path, fs::metadata(path)?.len())
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn handoff_checksums(artifact: &Path, label: &str) -> Result<Value> {
    let mut found = BTreeSet::new();
    let mut hashes = json!({});
    let sums = fs::read_to_string(artifact.join("SHA256SUMS"))?;
    for line in sums.lines() {
        let (hash, name) = line.split_once("  ").context("invalid handoff checksum")?;
        ensure!(
            sha256(hash) && HANDOFF.contains(&name) && found.insert(name),
            "unexpected or duplicate handoff file"
        );
        ensure!(
            digest(&artifact.join(name))? == hash,
            "{label} handoff checksum mismatch: {name}"
        );
        hashes[name] = json!(hash);
    }
    ensure!(found.len() == HANDOFF.len(), "incomplete {label} handoff");
    Ok(hashes)
}

fn extract(archive: &Path, destination: &Path) -> Result<()> {
    let mut archive =
        tar::Archive::new(GzDecoder::new(File::open(archive)?).take(128 * 1024 * 1024));
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = std::str::from_utf8(&entry.path_bytes())?
            .trim_end_matches('/')
            .to_owned();
        let kind = entry.header().entry_type();
        ensure!(
            bundle::safe_name(&name)
                && (name == "source"
                    || name.starts_with("source/")
                    || ["image.json", "inspect.json", "probe.stdout"].contains(&name.as_str()))
                && (kind.is_file() || kind.is_dir())
                && names.insert(name.clone())
                && names.len() <= 10_000,
            "unsafe or duplicate candidate handoff member"
        );
        total = total
            .checked_add(entry.size())
            .context("archive size overflow")?;
        ensure!(
            total <= 64 * 1024 * 1024,
            "oversized candidate source handoff"
        );
        let path = destination.join(&name);
        if kind.is_dir() {
            ensure!(
                name == "source" || name.starts_with("source/"),
                "unexpected handoff directory"
            );
            fs::create_dir_all(path)?;
        } else {
            fs::create_dir_all(path.parent().context("missing handoff parent")?)?;
            let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
            let size = entry.size();
            ensure!(
                std::io::copy(&mut entry, &mut file)? == size,
                "truncated candidate source handoff"
            );
            let mode = if entry.header().mode()? & 0o111 == 0 {
                0o644
            } else {
                0o755
            };
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
    }
    // Force the gzip trailer to be checked, including after tar's end marker.
    let mut reader = archive.into_inner();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        ensure!(
            buffer[..count].iter().all(|byte| *byte == 0),
            "unexpected trailing handoff content"
        );
    }
    Ok(())
}

fn native_host(native: &Value, platform: &str, hosts: Hosts) -> Result<bool> {
    let arch = super::registry::architecture(platform)?;
    let machine = if arch == "amd64" { "x86_64" } else { "aarch64" };
    let linux = native["host"]["os"] == "Linux" && native["host"]["machine"] == machine;
    let apple = hosts == Hosts::Local
        && arch == "arm64"
        && native["host"]["os"] == "Darwin"
        && native["host"]["machine"] == "arm64";
    Ok((linux || apple)
        && [format!("linux/{arch}"), format!("linux/{machine}")]
            .iter()
            .any(|engine| native["docker"]["engine"] == *engine))
}

fn candidate(
    stack: &Stack,
    root: &Path,
    work: &Path,
    image: &Image,
    platform: &str,
    hosts: Hosts,
) -> Result<Receipt> {
    let label = stack.label;
    let (name, version) = (image.repository, image.version);
    let receipt = load(work)?;
    ensure!(
        receipt.repository == name
            && receipt.version()? == version
            && receipt.platform == platform
            && receipt.local_verified
            && receipt.publication == Publication::Prepared
            && receipt.image.is_none(),
        "{label} candidate identity/state mismatch"
    );
    let Input::Build {
        source,
        recipe_sha256,
        ..
    } = &receipt.input
    else {
        anyhow::bail!("{label} gate requires native built candidates");
    };
    let native = bundle::read_json(&work.join("native.json"))?;
    ensure!(
        native["format_version"] == 1
            && native["platform"] == platform
            && native["revision"] == source["revision"]
            && native_host(&native, platform, hosts)?
            && native["managed_qualification"] == false
            && native["published"] == false,
        "candidate was not built on the matching native host"
    );
    verify_build_inputs(&work.join("source"), name, version, recipe_sha256)?;
    verify_build_inputs(root, name, version, recipe_sha256)?;
    // Reuse is allowed across application-only changes, never changed build inputs.
    let (provenance, patches) =
        bound_inputs(name, version).context("stack image has no bound build inputs")?;
    let mut inputs = vec![versioned_recipe(name, version)?, provenance];
    inputs.extend(patches);
    for input in inputs {
        ensure!(
            digest(&root.join(input))? == digest(&work.join("source").join(input))?,
            "candidate build input changed: {input}"
        );
    }
    inspect(work)?;
    ensure!(
        super::valid_probe_version(
            name,
            version,
            &fs::read_to_string(work.join("probe.stdout"))?
        ),
        "invalid native {label} probe"
    );
    Ok(receipt)
}

#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the restore command's explicit positional arguments"
)]
pub(super) fn restore(
    stack: &Stack,
    hosts: Hosts,
    root: &Path,
    platform: &str,
    revision: &str,
    attempt: &str,
    artifacts: &Path,
    output: &Path,
) -> Result<()> {
    let arch = super::registry::architecture(platform)?;
    ensure!(
        revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid candidate revision"
    );
    ensure!(
        attempt.parse::<u32>().is_ok_and(|n| n > 0),
        "invalid candidate attempt"
    );
    ensure!(
        !output.exists() && fs::read_dir(artifacts)?.count() == stack.images.len(),
        "expected exactly {} {} artifacts and a new output",
        stack.images.len(),
        stack.label
    );
    let staging = tempfile::tempdir_in(output.parent().context("output parent")?)?;
    for image in stack.images {
        let (name, version) = (image.repository, image.version);
        let artifact = artifacts.join(format!(
            "{}-image-{name}@{version}-{arch}-{revision}-{attempt}",
            stack.name
        ));
        let hashes = handoff_checksums(&artifact, stack.label)?;
        let work = staging.path().join(name);
        fs::create_dir(&work)?;
        extract(&artifact.join("work.tar.gz"), &work)?;
        for file in ["image.json", "inspect.json", "probe.stdout"] {
            ensure!(
                digest(&work.join(file))? == text(&hashes, file)?,
                "frozen receipt differs from exported receipt"
            );
        }
        fs::copy(artifact.join("native.json"), work.join("native.json"))?;
        let receipt = candidate(stack, root, &work, image, platform, hosts)?;
        let Input::Build { source, .. } = receipt.input else {
            unreachable!()
        };
        ensure!(
            source["revision"] == revision,
            "candidate revision differs from the selected build run"
        );
        write_json(
            &work.join("handoff.json"),
            &json!({"artifact":artifact.canonicalize()?,"checksums":hashes,"revision":revision,"attempt":attempt}),
        )?;
    }
    fs::rename(staging.keep(), output)?;
    Ok(())
}

fn manifest(work: &Path, receipt: &Receipt, hosts: Hosts) -> Result<String> {
    let manifest = bundle::read_json(&work.join("manifest.json"))?;
    let config_digest = format!("sha256:{}", digest(&work.join("config.json"))?);
    let manifest_digest = format!("sha256:{}", digest(&work.join("manifest.json"))?);
    // Docker's classic store names an image by its config digest. Docker Desktop's
    // containerd store names it by its manifest digest, which commits to that config.
    let local_id = receipt.local_image_id.as_deref();
    ensure!(
        manifest["schemaVersion"] == 2
            && manifest["config"]["digest"] == config_digest
            && (local_id == Some(&config_digest)
                || (hosts == Hosts::Local && local_id == Some(&manifest_digest))),
        "registry config differs from the native candidate image"
    );
    let config = bundle::read_json(&work.join("config.json"))?;
    let inspected = bundle::read_json(&work.join("inspect.json"))?;
    ensure!(
        config["os"] == "linux"
            && config["architecture"] == super::registry::architecture(&receipt.platform)?
            && config["config"]["User"] == "1000:1000"
            && config["rootfs"]["diff_ids"] == inspected[0]["RootFS"]["Layers"],
        "registry platform/root filesystem mismatch"
    );
    Ok(manifest_digest)
}

/// Write the stack's pins file for `platform` from verified loopback manifests.
pub(super) fn stage(
    stack: &Stack,
    hosts: Hosts,
    root: &Path,
    platform: &str,
    candidates: &Path,
    output: &Path,
) -> Result<()> {
    super::registry::architecture(platform)?;
    ensure!(!output.exists(), "staged output already exists");
    let staging = tempfile::tempdir_in(output.parent().context("output parent")?)?;
    let mut pins = json!({});
    let mut records = Vec::new();
    let mut revisions = BTreeSet::new();
    for image in stack.images {
        let (name, version) = (image.repository, image.version);
        let work = candidates.join(name);
        let receipt = candidate(stack, root, &work, image, platform, hosts)?;
        let manifest = manifest(&work, &receipt, hosts)?;
        let Input::Build { source, .. } = &receipt.input else {
            unreachable!()
        };
        revisions.insert(text(source, "revision")?.to_owned());
        let reference = format!("proofstorm-registry.localhost:5000/{name}@{manifest}");
        pins[platform][image.key] = json!(reference);
        records.push(
            json!({"repository":name,"version":version,"platform":platform,"image":reference,
            "config_digest":receipt.local_image_id,"source":source,"native_probe_verified":true,
            "handoff":bundle::read_json(&work.join("handoff.json"))?}),
        );
        let dest = staging.path().join(name);
        fs::create_dir(&dest)?;
        for file in [
            "image.json",
            "native.json",
            "probe.stdout",
            "manifest.json",
            "config.json",
        ] {
            fs::copy(work.join(file), dest.join(file))?;
        }
    }
    ensure!(revisions.len() == 1, "mixed candidate source revisions");
    write_json(&staging.path().join(stack.pins_file()), &pins)?;
    write_json(
        &staging.path().join("candidates.json"),
        &json!({"format_version":1,"platform":platform,"images":records,"published":false,"managed_qualification":false}),
    )?;
    fs::rename(staging.keep(), output)?;
    Ok(())
}

/// Bind the gate's own evidence to the staged candidates in a public receipt.
pub(super) fn evidence(stack: &Stack, staged: &Path, run: &Path, output: &Path) -> Result<()> {
    let label = stack.label;
    let files = [
        "acceptance.json".to_owned(),
        format!("{}-result.json", stack.name),
        format!("{}-plan.json", stack.name),
        format!("{}-storage-cleanup.json", stack.name),
    ];
    let mut report = bundle::read_json(&staged.join("candidates.json"))?;
    let acceptance = bundle::read_json(&run.join(&files[0]))?;
    let result = bundle::read_json(&run.join(&files[1]))?;
    let plan = bundle::read_json(&run.join(&files[2]))?;
    let storage = bundle::read_json(&run.join(&files[3]))?;
    ensure!(
        acceptance["setup"] == "passed"
            && acceptance["cleanup"] == "passed"
            && acceptance["preservation"] == "passed"
            && acceptance["cleanup_errors"] == json!([])
            && acceptance["gates"] == json!([{"name":stack.gate,"status":"passed"}])
            && result["passed"] == true
            && result["exercise_error"].is_null()
            && result["cleanup_error"].is_null()
            && storage["remaining_volumes"] == 0,
        "managed {label} qualification or cleanup did not pass"
    );
    let entries = plan["lock"]["entries"]
        .as_array()
        .with_context(|| format!("{label} lock entries"))?;
    let images = report["images"].as_array().context("candidate images")?;
    ensure!(
        images.len() == stack.images.len(),
        "incomplete candidate set"
    );
    for expected in stack.images {
        let selected: Vec<_> = images
            .iter()
            .filter(|image| {
                image["repository"] == expected.repository && image["version"] == expected.version
            })
            .collect();
        ensure!(selected.len() == 1, "missing or duplicate candidate");
        let image = selected[0];
        ensure!(
            entries
                .iter()
                .any(|entry| entry["catalog_id"] == expected.catalog_id
                    && entry["version"] == image["version"]
                    && entry["image"] == image["image"]),
            "gate did not use the staged candidate images"
        );
    }
    // Public receipt is an allowlist. Never upload the runtime home, raw command
    // output, mint proofs, seed material, TLS keys, or error strings.
    let mut hashes = json!({});
    for file in &files {
        hashes[file] = json!(digest(&run.join(file))?);
    }
    report["managed_qualification"] = json!(true);
    report["cleanup_verified"] = json!(true);
    report["preservation_verified"] = json!(true);
    report["evidence_sha256"] = hashes;
    write_json(output, &report)
}

#[cfg(test)]
mod tests;
