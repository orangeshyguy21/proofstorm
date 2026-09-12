//! Offline release metadata checks. Passing is not release acceptance.
mod archive;
mod build;
mod bundle;
mod catalog_images;
mod controller;
mod linux_install;
mod macos_install;
mod package;
mod promotion;
mod registry;
mod shortcuts;
mod smoke;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::PathBuf,
};

const LOCAL_REGISTRY: &str = "proofstorm-registry.localhost:5000/";
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

pub(super) fn verify_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    bundle::cli(args)
}

pub(super) fn smoke_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    smoke::cli(args)
}

pub(super) fn promotion_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    promotion::cli(args)
}

pub(super) fn shortcuts_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    shortcuts::cli(args)
}

pub(super) fn controller_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    controller::cli(args)
}
pub(super) fn catalog_image_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    catalog_images::cli(args)
}

pub(super) fn macos_install_cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    macos_install::cli(args)
}

pub(super) fn artifact_cli(command: &str, args: impl Iterator<Item = OsString>) -> Result<()> {
    if command == "release-package" {
        package::cli(args)
    } else {
        archive::cli(command, args)
    }
}

pub(super) fn build_cli(command: &str, args: impl Iterator<Item = OsString>) -> Result<()> {
    build::cli(command, args)
}

pub(super) fn linux_install_cli(command: &str, args: impl Iterator<Item = OsString>) -> Result<()> {
    linux_install::cli(command, args)
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing or invalid {key}"))
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn pinned(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(name, digest)| {
        !name.is_empty()
            && !name.contains('@')
            && !name.chars().any(char::is_whitespace)
            && sha256(digest)
    })
}

fn platform(target: &str) -> Result<&'static str> {
    match target {
        "aarch64-apple-darwin" => Ok("linux/arm64"),
        "x86_64-unknown-linux-gnu" => Ok("linux/amd64"),
        _ => bail!("unsupported bundle target: {target}"),
    }
}

fn artifact_platform(target: &str) -> Result<&'static str> {
    match target {
        "aarch64-apple-darwin" => Ok("macos-arm64"),
        "x86_64-unknown-linux-gnu" => Ok("linux-amd64"),
        _ => bail!("unsupported bundle target: {target}"),
    }
}

fn alpha_version(version: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    version.split_once("-alpha.").is_some_and(|(base, alpha)| {
        let parts: Vec<_> = base.split('.').collect();
        parts.len() == 3 && parts.iter().all(|part| digits(part)) && digits(alpha)
    })
}

fn publication_namespace(value: &str) -> bool {
    let valid = |part: &str, slash: bool| {
        part.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && part.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b"._-".contains(&b)
                    || (slash && b == b'/')
            })
    };
    value
        .strip_prefix("ghcr.io/")
        .and_then(|v| v.split_once('/'))
        .is_some_and(|(owner, repo)| valid(owner, false) && valid(repo, true))
}

fn image_inventory(info: &Value) -> Result<Vec<Value>> {
    let publication: Value = match info.get("image_publication") {
        None => json!({}),
        Some(value) => serde_json::from_str(
            value
                .as_str()
                .context("image_publication must be a JSON string")?,
        )
        .context("invalid image_publication JSON")?,
    };
    ensure!(
        publication.is_object(),
        "image_publication must describe an object"
    );
    let namespace = match publication.get("namespace") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let namespace = value.as_str().context("invalid publication namespace")?;
            ensure!(
                publication_namespace(namespace),
                "invalid publication namespace"
            );
            Some(namespace)
        }
    };
    info["workload_images"]
        .as_array()
        .context("missing or invalid workload_images")?
        .iter()
        .map(|value| {
            let image = value.as_str().context("invalid workload image")?;
            ensure!(pinned(image), "image is not pinned: {image}");
            let source = if let Some(local) = image.strip_prefix(LOCAL_REGISTRY) {
                if let Some(upstream) = local.strip_prefix("upstream/") {
                    ensure!(pinned(upstream), "invalid upstream image source");
                    Some(upstream.to_owned())
                } else {
                    namespace.map(|namespace| format!("{namespace}/{local}"))
                }
            } else {
                Some(image.to_owned())
            };
            Ok(json!({"image": image, "published_source": source,
                "verified_platforms": [], "availability_verified": false}))
        })
        .collect()
}

fn validate_assets(info: &Value) -> Result<()> {
    let assets = info["web_assets"]
        .as_array()
        .context("missing or invalid web_assets")?;
    ensure!(
        assets.iter().any(|asset| asset["path"] == "index.html"),
        "missing embedded index.html"
    );
    for suffix in [".js", ".wasm", ".css"] {
        ensure!(
            assets
                .iter()
                .any(|asset| asset["path"].as_str().is_some_and(|p| p.ends_with(suffix))),
            "missing embedded {suffix}"
        );
    }
    for asset in assets {
        text(asset, "path")?;
        ensure!(
            asset["size"].as_u64().is_some_and(|size| size > 0) && sha256(text(asset, "sha256")?),
            "invalid embedded asset receipt"
        );
    }
    Ok(())
}

fn validate_controller(info: &Value, expected_platform: &str, alpha: bool) -> Result<()> {
    let controller = info.get("controller").filter(|value| !value.is_null());
    if let Some(controller) = controller {
        ensure!(
            controller["platform"] == expected_platform,
            "controller platform does not match host bundle"
        );
        let contract = text(info, "runtime_contract_sha256")?;
        ensure!(
            sha256(contract)
                && controller["metadata"]["version"] == info["version"]
                && controller["metadata"]["runtime_contract_sha256"] == contract,
            "controller runtime contract does not match host bundle"
        );
    }
    if alpha {
        let controller =
            controller.context("alpha requires a published digest-pinned controller")?;
        let image = text(controller, "image")?;
        ensure!(
            image.strip_prefix("ghcr.io/").is_some_and(pinned),
            "alpha requires a published digest-pinned controller"
        );
    }
    Ok(())
}

fn validate(info: &Value, alpha: bool) -> Result<Value> {
    ensure!(
        info["format_version"].as_u64() == Some(1),
        "unsupported binary metadata format"
    );
    let target = text(info, "target")?;
    let expected_platform = platform(target)?;
    let version = text(info, "version")?;
    ensure!(
        version
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b)),
        "unsafe version"
    );
    ensure!(
        matches!(text(info, "build_profile")?, "debug" | "release"),
        "unsupported build profile"
    );
    if let Some(tools) = info.get("bootstrap_tools").filter(|value| !value.is_null()) {
        ensure!(tools["target"] == target, "bootstrap tool target mismatch");
    }
    validate_assets(info)?;
    let images = image_inventory(info)?;
    validate_controller(info, expected_platform, alpha)?;
    if alpha {
        ensure!(
            alpha_version(version),
            "alpha channel requires an alpha version"
        );
        ensure!(
            info["bootstrap_tools"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "alpha requires pinned bootstrap tools"
        );
        ensure!(
            images
                .iter()
                .all(|image| image["published_source"].is_string()),
            "alpha requires published workload image sources"
        );
    }
    Ok(
        json!({"format_version": 1, "metadata_valid": true, "alpha_requirements_checked": alpha,
        "version": version, "target": target, "platform": expected_platform,
        "release_ready": false, "workload_images": images,
        "unverified": ["payload integrity and binary provenance", "remote image availability and platforms",
            "fresh installation and running environment"]}),
    )
}

pub(super) fn cli(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let path = PathBuf::from(
        args.next()
            .context("usage: release-check FILE [--alpha] [--json]")?,
    );
    let mut alpha = false;
    let mut json_output = false;
    for arg in args {
        match arg.to_str() {
            Some("--alpha") if !alpha => alpha = true,
            Some("--json") if !json_output => json_output = true,
            _ => bail!("usage: release-check FILE [--alpha] [--json]"),
        }
    }
    let mut bytes = Vec::new();
    // Reject directories/devices/FIFOs before opening, which could otherwise block.
    ensure!(
        fs::metadata(&path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .is_file(),
        "release metadata must be a regular file"
    );
    let file = File::open(&path).with_context(|| format!("cannot read {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "release metadata must be a regular file"
    );
    file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "release metadata exceeds 4 MiB"
    );
    let info: Value = serde_json::from_slice(&bytes).context("invalid release metadata JSON")?;
    let receipt = validate(&info, alpha)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
    } else {
        println!(
            "Release metadata valid: {} ({})",
            text(&info, "version")?,
            text(&info, "target")?
        );
        if alpha {
            println!("Alpha metadata requirements passed.");
        }
        println!(
            "Not release acceptance: payloads, remote images, and installation still need verification."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
