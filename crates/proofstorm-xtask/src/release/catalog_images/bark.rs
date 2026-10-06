//! Offline candidate handoff for the managed Bark gate. Never publishes images.
use super::{
    Input, Publication, Receipt, bundle, inspect, load, recipe, sha256, text, verify_bark_inputs,
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

const IMAGES: [(&str, &str, &str); 3] = [
    ("cdk-bark-processor", "0.1.0-fe468ca", "processor"),
    ("bark-server", "0.7.0-6188e2d", "server"),
    ("cln-hold", "26.06.7-hold.0.3.3", "cln"),
];
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

fn handoff_checksums(artifact: &Path) -> Result<Value> {
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
            "Bark handoff checksum mismatch: {name}"
        );
        hashes[name] = json!(hash);
    }
    ensure!(found.len() == HANDOFF.len(), "incomplete Bark handoff");
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
            "unsafe or duplicate Bark handoff member"
        );
        total = total
            .checked_add(entry.size())
            .context("archive size overflow")?;
        ensure!(total <= 64 * 1024 * 1024, "oversized Bark source handoff");
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
                "truncated Bark source handoff"
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

fn candidate(
    root: &Path,
    work: &Path,
    name: &str,
    version: &str,
    platform: &str,
) -> Result<Receipt> {
    let receipt = load(work)?;
    ensure!(
        receipt.repository == name
            && receipt.version()? == version
            && receipt.platform == platform
            && receipt.local_verified
            && receipt.publication == Publication::Prepared
            && receipt.image.is_none(),
        "Bark candidate identity/state mismatch"
    );
    let Input::Build {
        source,
        recipe_sha256,
        ..
    } = &receipt.input
    else {
        anyhow::bail!("Bark gate requires native built candidates");
    };
    let native = bundle::read_json(&work.join("native.json"))?;
    let arch = super::registry::architecture(platform)?;
    let machine = if arch == "amd64" { "x86_64" } else { "aarch64" };
    ensure!(
        native["format_version"] == 1
            && native["platform"] == platform
            && native["revision"] == source["revision"]
            && native["host"]["os"] == "Linux"
            && native["host"]["machine"] == machine
            && [format!("linux/{arch}"), format!("linux/{machine}")]
                .iter()
                .any(|engine| native["docker"]["engine"] == *engine)
            && native["managed_qualification"] == false
            && native["published"] == false,
        "candidate was not built on the matching native Linux runner"
    );
    verify_bark_inputs(&work.join("source"), name, recipe_sha256)?;
    verify_bark_inputs(root, name, recipe_sha256)?;
    // Reuse is allowed across application-only changes, never changed build inputs.
    let stem = if name == "cdk-bark-processor" {
        "cdk-bark"
    } else {
        name
    };
    let mut inputs = vec![
        recipe(name)?.to_owned(),
        format!("docker/payment/{stem}-provenance.json"),
    ];
    if name == "cdk-bark-processor" {
        inputs.push("docker/payment/patches/bark-regtest-rpc.patch".into());
    }
    for input in inputs {
        ensure!(
            digest(&root.join(&input))? == digest(&work.join("source").join(&input))?,
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
        "invalid native Bark probe"
    );
    Ok(receipt)
}

pub(super) fn restore(
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
        !output.exists() && fs::read_dir(artifacts)?.count() == IMAGES.len(),
        "expected exactly three Bark artifacts and a new output"
    );
    let staging = tempfile::tempdir_in(output.parent().context("output parent")?)?;
    for (name, version, _) in IMAGES {
        let artifact = artifacts.join(format!(
            "bark-image-{name}@{version}-{arch}-{revision}-{attempt}"
        ));
        let hashes = handoff_checksums(&artifact)?;
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
        let receipt = candidate(root, &work, name, version, platform)?;
        let Input::Build { source, .. } = receipt.input else {
            unreachable!()
        };
        ensure!(
            source["revision"] == revision,
            "candidate revision differs from the selected Actions run"
        );
        write_json(
            &work.join("handoff.json"),
            &json!({"artifact":artifact.canonicalize()?,"checksums":hashes,"revision":revision,"attempt":attempt}),
        )?;
    }
    fs::rename(staging.keep(), output)?;
    Ok(())
}

fn manifest(work: &Path, receipt: &Receipt) -> Result<String> {
    let manifest = bundle::read_json(&work.join("manifest.json"))?;
    let config_digest = format!("sha256:{}", digest(&work.join("config.json"))?);
    ensure!(
        manifest["schemaVersion"] == 2
            && manifest["config"]["digest"] == config_digest
            && receipt.local_image_id.as_deref() == Some(&config_digest),
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
    Ok(format!("sha256:{}", digest(&work.join("manifest.json"))?))
}

pub(super) fn stage(root: &Path, platform: &str, candidates: &Path, output: &Path) -> Result<()> {
    super::registry::architecture(platform)?;
    ensure!(!output.exists(), "staged output already exists");
    let staging = tempfile::tempdir_in(output.parent().context("output parent")?)?;
    let mut pins = json!({});
    let mut records = Vec::new();
    let mut revisions = BTreeSet::new();
    for (name, version, key) in IMAGES {
        let work = candidates.join(name);
        let receipt = candidate(root, &work, name, version, platform)?;
        let manifest = manifest(&work, &receipt)?;
        let Input::Build { source, .. } = &receipt.input else {
            unreachable!()
        };
        revisions.insert(text(source, "revision")?.to_owned());
        let image = format!("proofstorm-registry.localhost:5000/{name}@{manifest}");
        pins[platform][key] = json!(image);
        records.push(
            json!({"repository":name,"version":version,"platform":platform,"image":image,
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
    write_json(&staging.path().join("bark_images.json"), &pins)?;
    write_json(
        &staging.path().join("candidates.json"),
        &json!({"format_version":1,"platform":platform,"images":records,"published":false,"managed_qualification":false}),
    )?;
    fs::rename(staging.keep(), output)?;
    Ok(())
}

pub(super) fn evidence(staged: &Path, run: &Path, output: &Path) -> Result<()> {
    let mut report = bundle::read_json(&staged.join("candidates.json"))?;
    let acceptance = bundle::read_json(&run.join("acceptance.json"))?;
    let result = bundle::read_json(&run.join("bark-result.json"))?;
    let storage = bundle::read_json(&run.join("bark-storage-cleanup.json"))?;
    let plan = bundle::read_json(&run.join("bark-plan.json"))?;
    ensure!(
        acceptance["setup"] == "passed"
            && acceptance["cleanup"] == "passed"
            && acceptance["preservation"] == "passed"
            && acceptance["cleanup_errors"] == json!([])
            && acceptance["gates"] == json!([{"name":"bark-processor","status":"passed"}])
            && result["passed"] == true
            && result["exercise_error"].is_null()
            && result["cleanup_error"].is_null()
            && storage["remaining_volumes"] == 0,
        "managed Bark qualification or cleanup did not pass"
    );
    let entries = plan["lock"]["entries"]
        .as_array()
        .context("Bark lock entries")?;
    let images = report["images"].as_array().context("candidate images")?;
    ensure!(images.len() == IMAGES.len(), "incomplete candidate set");
    for (name, version, _) in IMAGES {
        let selected: Vec<_> = images
            .iter()
            .filter(|image| image["repository"] == name && image["version"] == version)
            .collect();
        ensure!(selected.len() == 1, "missing or duplicate candidate");
        let image = selected[0];
        ensure!(
            entries
                .iter()
                .any(|entry| entry["catalog_id"] == image["repository"]
                    && entry["version"] == image["version"]
                    && entry["image"] == image["image"]),
            "gate did not use the staged candidate images"
        );
    }
    // Public receipt is an allowlist. Never upload the runtime home, raw command
    // output, mint proofs, seed material, TLS keys, or error strings.
    let mut hashes = json!({});
    for file in [
        "acceptance.json",
        "bark-result.json",
        "bark-plan.json",
        "bark-storage-cleanup.json",
    ] {
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
