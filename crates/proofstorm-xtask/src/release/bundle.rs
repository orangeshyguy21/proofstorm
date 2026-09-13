//! Verify an unpacked bundle without executing it or trusting readiness claims.
use super::{MAX_METADATA_BYTES, image_inventory, platform, sha256, text, validate};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub(super) const REQUIRED: &[&str] = &[
    "bin/proofstorm",
    "bin/proofstorm-mcp",
    "LICENSE",
    "catalog.json",
    "tools/versions.env",
    "chart/Chart.yaml",
    "chart/values.yaml",
    "chart/templates/deployment.yaml",
    "release-info.json",
    "chart/templates/_helpers.tpl",
    "chart/templates/serviceaccount.yaml",
    "chart/templates/rbac.yaml",
    "chart/templates/private-pvc.yaml",
    "chart/crds/proofstorm.dev_proofstormcells.yaml",
    "chart/crds/proofstorm.dev_proofstormcellactions.yaml",
    "chart/crds/proofstorm.dev_proofstormcandidatebuilds.yaml",
];
pub(super) const MAX_FILES: usize = 10_000;
pub(super) const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;

fn regular(path: &Path) -> Result<fs::Metadata> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "payload symlink/non-file refused: {}",
        path.display()
    );
    Ok(metadata)
}

fn read_small(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        regular(path)?.len() <= MAX_METADATA_BYTES,
        "metadata exceeds 4 MiB: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "metadata exceeds 4 MiB"
    );
    Ok(bytes)
}

pub(super) fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&read_small(path)?)
        .with_context(|| format!("invalid JSON: {}", path.display()))
}

pub(super) fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', '\0'])
        && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn manifest_files(manifest: &Value) -> Result<&Map<String, Value>> {
    let files = manifest["files"]
        .as_object()
        .context("invalid bundle file inventory")?;
    ensure!(files.len() < MAX_FILES, "bundle exceeds file limit");
    ensure!(
        REQUIRED.iter().all(|name| files.contains_key(*name)),
        "incomplete bundle manifest"
    );
    let mut total = 0_u64;
    for (name, receipt) in files {
        ensure!(
            safe_name(name) && name != "manifest.json",
            "unsafe manifest path: {name}"
        );
        let size = receipt["size"].as_u64().context("invalid payload size")?;
        total = total
            .checked_add(size)
            .context("bundle exceeds size limit")?;
        ensure!(
            size > 0 && total <= MAX_PAYLOAD_BYTES,
            "empty payload or bundle exceeds size limit"
        );
        ensure!(
            sha256(text(receipt, "sha256")?),
            "invalid payload digest: {name}"
        );
        let expected_mode = if name.starts_with("bin/") {
            0o755
        } else {
            0o644
        };
        ensure!(
            receipt["mode"].as_u64() == Some(expected_mode),
            "unsafe payload mode: {name}"
        );
    }
    Ok(files)
}

fn walk(root: &Path, current: &Path, depth: usize, observed: &mut BTreeSet<String>) -> Result<()> {
    ensure!(depth <= 64, "bundle directory nesting exceeds limit");
    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        ensure!(
            !kind.is_symlink(),
            "payload symlink refused: {}",
            path.display()
        );
        if kind.is_dir() {
            walk(root, &path, depth + 1, observed)?;
        } else {
            ensure!(
                kind.is_file(),
                "non-regular payload refused: {}",
                path.display()
            );
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF-8 payload name")?;
            observed.insert(name.to_owned());
            ensure!(observed.len() <= MAX_FILES, "bundle exceeds file limit");
        }
    }
    Ok(())
}

pub(super) fn checksum(path: &Path, expected_size: u64) -> Result<String> {
    let mut file = File::open(path)?.take(expected_size + 1);
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        digest.update(&buffer[..count]);
    }
    ensure!(
        bytes == expected_size,
        "payload size changed: {}",
        path.display()
    );
    Ok(format!("{:x}", digest.finalize()))
}

fn verify_payload(root: &Path, files: &Map<String, Value>) -> Result<()> {
    let expected: BTreeSet<_> = files
        .keys()
        .cloned()
        .chain(["manifest.json".to_owned()])
        .collect();
    let mut observed = BTreeSet::new();
    walk(root, root, 0, &mut observed)?;
    ensure!(observed == expected, "bundle has missing or unlisted files");
    for (name, receipt) in files {
        let path = root.join(name);
        let metadata = regular(&path)?;
        ensure!(
            metadata.len() == receipt["size"].as_u64().context("invalid payload size")?
                && checksum(&path, metadata.len())? == text(receipt, "sha256")?,
            "payload checksum mismatch: {name}"
        );
        ensure!(
            u64::from(metadata.permissions().mode() & 0o7777)
                == receipt["mode"].as_u64().context("invalid payload mode")?,
            "payload mode mismatch: {name}"
        );
    }
    Ok(())
}

fn verify_metadata(root: &Path, manifest: &Value) -> Result<()> {
    let info = read_json(&root.join("release-info.json"))?;
    validate(&info, manifest["channel"] == "alpha")?;
    for key in ["target", "version", "build_profile"] {
        ensure!(info[key] == manifest[key], "manifest {key} mismatch");
    }
    let revision = text(&manifest["source"], "revision")?;
    let digest = text(&manifest["source"], "sha256")?;
    ensure!(
        !revision.is_empty()
            && sha256(digest)
            && info["source_revision"] == revision
            && info["source_sha256"] == digest,
        "manifest provenance mismatch"
    );
    ensure!(
        manifest["source"]["dirty"].is_boolean(),
        "invalid source dirty flag"
    );
    ensure!(
        manifest.get("controller").unwrap_or(&Value::Null)
            == info.get("controller").unwrap_or(&Value::Null),
        "controller metadata mismatch"
    );
    ensure!(
        info.get("catalog").context("missing catalog metadata")?
            == &read_json(&root.join("catalog.json"))?,
        "catalog mismatch"
    );
    ensure!(
        text(&info, "tools")?.as_bytes() == read_small(&root.join("tools/versions.env"))?,
        "tool pins mismatch"
    );
    ensure!(
        manifest["workload_images"] == json!(image_inventory(&info)?),
        "image inventory mismatch"
    );
    Ok(())
}

pub(super) fn verify(root: &Path) -> Result<Value> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "bundle root must be a real directory"
    );
    let manifest = read_json(&root.join("manifest.json"))?;
    ensure!(
        manifest["format_version"].as_u64() == Some(1),
        "unsupported bundle format"
    );
    platform(text(&manifest, "target")?)?;
    ensure!(
        matches!(
            text(&manifest, "channel")?,
            "development" | "alpha" | "release"
        ),
        "unsupported bundle channel"
    );
    let blockers = manifest["release_blockers"]
        .as_array()
        .context("invalid release blockers")?;
    ensure!(
        blockers.iter().all(Value::is_string),
        "invalid release blockers"
    );
    let ready = manifest["release_ready"]
        .as_bool()
        .context("invalid release readiness")?;
    ensure!(
        ready == blockers.is_empty(),
        "inconsistent release readiness"
    );
    // Current receipts contain no authenticated runtime/remote-image evidence.
    // Never turn a hand-edited manifest into release acceptance.
    ensure!(
        !ready,
        "release readiness lacks required evidence; independent verification is unavailable"
    );
    let files = manifest_files(&manifest)?;
    verify_payload(root, files)?;
    verify_metadata(root, &manifest)?;
    Ok(json!({"format_version": 1, "integrity_verified": true,
        "release_ready": false, "release_blockers": blockers,
        "version": manifest["version"], "target": manifest["target"], "channel": manifest["channel"],
        "unverified": ["embedded binary metadata and source provenance", "remote image availability and platforms",
            "fresh installation and running environment"]}))
}

pub(super) fn cli(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let root = PathBuf::from(
        args.next()
            .context("usage: release-verify DIRECTORY [--json]")?,
    );
    let json_output = match args.next() {
        None => false,
        Some(arg) if arg == "--json" => true,
        _ => bail!("usage: release-verify DIRECTORY [--json]"),
    };
    ensure!(
        args.next().is_none(),
        "usage: release-verify DIRECTORY [--json]"
    );
    let receipt = verify(&root)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
    } else {
        println!(
            "Bundle integrity verified: {} ({})",
            text(&receipt, "version")?,
            text(&receipt, "target")?
        );
        println!(
            "Not release acceptance: bundled programs were not executed; runtime and remote images remain unverified."
        );
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
