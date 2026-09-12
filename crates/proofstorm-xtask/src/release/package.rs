//! Assemble a bundle from trusted local build outputs, then verify and archive it.
use super::{alpha_version, archive, bundle, image_inventory, platform, sha256, text, validate};
use crate::development::{copy_tree, directory, inventory, regular};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn binary_info(binary: &Path, flags: &[&str], work: &Path) -> Result<Value> {
    let environment: Vec<_> = std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            !name.starts_with("PROOFSTORM_")
                && !name.starts_with("K3D_")
                && !name.starts_with("TRUNK_")
                && !matches!(name.as_ref(), "CARGO_BUILD_TARGET" | "CARGO_TARGET_DIR")
        })
        .collect();
    let output = Command::new(binary)
        .args(flags)
        .current_dir(work)
        .env_clear()
        .envs(environment)
        .env("PROOFSTORM_HOME", work.join("must-not-be-created"))
        .output()
        .with_context(|| format!("cannot read metadata from {}", binary.display()))?;
    ensure!(
        output.status.success(),
        "binary metadata command failed: {}\n{}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.stdout.len() as u64 <= super::MAX_METADATA_BYTES,
        "binary metadata exceeds 4 MiB"
    );
    ensure!(
        !work.join("must-not-be-created").exists() && !work.join(".proofstorm").exists(),
        "metadata command created runtime state"
    );
    serde_json::from_slice(&output.stdout).context("invalid binary metadata JSON")
}

fn blockers(info: &Value, provenance: &Value, images: &[Value]) -> Result<Vec<String>> {
    let target = text(info, "target")?;
    let mut result = vec![format!(
        "Remote image availability and {} platforms are not verified.",
        platform(target)?
    )];
    result.push(
        if target == "aarch64-apple-darwin" {
            "Downloaded macOS signing/quarantine behavior has not been validated."
        } else {
            "Fresh Linux VM installation and runtime have not been validated."
        }
        .into(),
    );
    if info["controller"].is_null() {
        result.push("Published digest-pinned controller image is not configured.".into());
    } else if info["controller"]["release_ready"] != true {
        result.push(if info["controller"]["verification"]["registry_identity"] == true {
            "Controller image/startup are verified; live cluster reconciliation remains untested."
        } else { "Configured controller is a development preview, not a coherent release build." }.into());
    }
    if info["bootstrap_tools"].is_null() {
        result.push("Pinned bootstrap-tool downloads/checksums are not yet packaged.".into());
    }
    for image in images
        .iter()
        .filter(|image| image["published_source"].is_null())
    {
        result.push(format!(
            "Missing published image source: {}",
            text(image, "image")?
        ));
    }
    if provenance["dirty"]
        .as_bool()
        .context("invalid source dirty flag")?
    {
        result.push("Source snapshot includes uncommitted development changes.".into());
    }
    if info["build_profile"] != "release" {
        result.push("Host executables use a debug build profile.".into());
    }
    Ok(result)
}

fn copy_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    regular(source)?;
    fs::copy(source, destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

fn assemble(
    source: &Path,
    binaries: &Path,
    root: &Path,
    provenance: &Value,
    development: bool,
) -> Result<()> {
    directory(&root.join("bin"))?;
    for name in ["proofstorm", "proofstorm-mcp"] {
        copy_file(&binaries.join(name), &root.join("bin").join(name), 0o755)?;
    }
    // Execute only the explicitly selected, locally built binaries after copying.
    // Extracting/downloading an archive never invokes this function.
    let info = binary_info(&root.join("bin/proofstorm"), &["version", "--json"], root)?;
    let mcp = binary_info(&root.join("bin/proofstorm-mcp"), &["--release-info"], root)?;
    ensure!(
        info == mcp,
        "CLI and MCP were not built from the same release inputs"
    );
    let alpha = !development && alpha_version(text(&info, "version")?);
    validate(&info, alpha)?;
    ensure!(
        sha256(text(provenance, "sha256")?)
            && !text(provenance, "revision")?.is_empty()
            && info["source_revision"] == provenance["revision"]
            && info["source_sha256"] == provenance["sha256"],
        "binary/source provenance mismatch"
    );
    let images = image_inventory(&info)?;
    let blockers = blockers(&info, provenance, &images)?;
    ensure!(
        development || alpha || blockers.is_empty(),
        "release blocked:\n{}",
        blockers.join("\n")
    );
    copy_tree(&source.join("charts/proofstorm"), &root.join("chart"))?;
    copy_file(&source.join("LICENSE"), &root.join("LICENSE"), 0o644)?;
    directory(&root.join("tools"))?;
    copy_file(
        &source.join("tools/versions.env"),
        &root.join("tools/versions.env"),
        0o644,
    )?;
    ensure!(
        fs::read_to_string(root.join("tools/versions.env"))? == text(&info, "tools")?,
        "binary/tool pins mismatch"
    );
    // Match the current generated chart's unquoted top-level version fields.
    let chart = fs::read_to_string(root.join("chart/Chart.yaml"))?;
    let fields: std::collections::BTreeMap<_, _> = chart
        .lines()
        .filter_map(|line| line.split_once(": "))
        .collect();
    let version = text(&info, "version")?;
    ensure!(
        fields.get("version") == Some(&version) && fields.get("appVersion") == Some(&version),
        "chart version does not match the binaries"
    );
    write_json(
        &root.join("catalog.json"),
        info.get("catalog").context("missing catalog metadata")?,
    )?;
    write_json(&root.join("release-info.json"), &info)?;
    let mut files = Map::new();
    for (name, digest) in inventory(root)? {
        let path = root.join(&name);
        let mode = if name.starts_with("bin/") {
            0o755
        } else {
            0o644
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        files.insert(
            name,
            json!({"sha256": digest, "size": fs::metadata(path)?.len(), "mode": mode}),
        );
    }
    let manifest = json!({"format_version": 1, "version": info["version"], "target": info["target"],
        "build_profile": info["build_profile"], "channel": if development { "development" } else if alpha { "alpha" } else { "release" },
        "release_ready": false, "release_blockers": blockers, "source": provenance, "files": files,
        "controller": info["controller"], "workload_images": images});
    write_json(&root.join("manifest.json"), &manifest)?;
    fs::set_permissions(
        root.join("manifest.json"),
        fs::Permissions::from_mode(0o644),
    )?;
    bundle::verify(root)?;
    Ok(())
}

fn package(
    source: &Path,
    binaries: &Path,
    provenance: &Value,
    output: &Path,
    development: bool,
) -> Result<Value> {
    let source = source.canonicalize()?;
    let binaries = binaries.canonicalize()?;
    let output = archive::output_path(output)?;
    ensure!(
        !output.starts_with(&source) && !output.starts_with(&binaries),
        "package output must be outside source and binary inputs"
    );
    directory(&output)?;
    let output = output.canonicalize()?;
    let temporary = tempfile::Builder::new()
        .prefix(".proofstorm-stage-")
        .tempdir_in(&output)?;
    let root = temporary.path().join("proofstorm");
    assemble(&source, &binaries, &root, provenance, development)?;
    archive::pack(&root, &output)
}

pub(super) fn cli(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let source = PathBuf::from(args.next().context("expected source directory")?);
    let binaries = PathBuf::from(args.next().context("expected local binary directory")?);
    let provenance = PathBuf::from(args.next().context("expected source provenance JSON")?);
    let output = PathBuf::from(args.next().context("expected output directory")?);
    let mut development = false;
    let mut json_output = false;
    for arg in args {
        match arg.to_str() {
            Some("--development") if !development => development = true,
            Some("--json") if !json_output => json_output = true,
            _ => bail!(
                "usage: release-package SOURCE BINARIES PROVENANCE_JSON OUTPUT [--development] [--json]"
            ),
        }
    }
    let result = package(
        &source,
        &binaries,
        &bundle::read_json(&provenance)?,
        &output,
        development,
    )?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Bundle created: {}", text(&result, "archive")?);
        println!("Release readiness remains unverified.");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
