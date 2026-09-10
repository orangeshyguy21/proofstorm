//! Anonymous GHCR reads: never use Docker/GitHub credentials or user curl config.
use super::{REPOSITORY, sha256, text};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::process::Command;

const ACCEPT: &str = "Accept: application/vnd.oci.image.index.v1+json,application/vnd.oci.image.manifest.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.docker.distribution.manifest.v2+json";

fn request(url: &str, token: Option<&str>, head: bool) -> Result<Vec<u8>> {
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-redirs",
        "5",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-time",
        "60",
        "-H",
        ACCEPT,
    ]);
    if let Some(token) = token {
        command.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    if head {
        command.arg("--head");
    } else {
        // Large layer Content-Length values are normal for HEAD availability checks.
        command.args(["--max-filesize", "4194304"]);
    }
    let output = command.arg(url).output()?;
    ensure!(
        output.status.success(),
        "anonymous GHCR request failed; check package Actions access, public visibility, and image availability"
    );
    ensure!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "registry response too large"
    );
    Ok(output.stdout)
}

fn url(kind: &str, digest: &str) -> Result<String> {
    ensure!(
        digest.strip_prefix("sha256:").is_some_and(sha256),
        "invalid registry digest"
    );
    Ok(format!(
        "https://ghcr.io/v2/{}/{kind}/{digest}",
        REPOSITORY.trim_start_matches("ghcr.io/")
    ))
}

fn metadata(kind: &str, digest: &str, token: &str) -> Result<Value> {
    let bytes = request(&url(kind, digest)?, Some(token), false)?;
    ensure!(
        format!("sha256:{:x}", Sha256::digest(&bytes)) == digest,
        "registry content digest mismatch"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn manifest(digest: &str, identity: &str, token: &str, depth: usize) -> Result<bool> {
    ensure!(depth <= 3, "registry index nesting exceeds limit");
    let value = metadata("manifests", digest, token)?;
    if let Some(children) = value.get("manifests") {
        let children = children.as_array().context("invalid registry index")?;
        ensure!(
            !children.is_empty() && children.len() <= 100,
            "invalid registry child count"
        );
        let mut runnable = 0;
        let mut matches = digest == identity;
        for child in children {
            if child["platform"]["os"] == "unknown" {
                continue;
            }
            matches |= manifest(text(child, "digest")?, identity, token, depth + 1)?;
            runnable += 1;
        }
        ensure!(runnable == 1, "expected exactly one runnable AMD64 image");
        return Ok(matches);
    }
    let config_digest = text(&value["config"], "digest")?;
    let config = metadata("blobs", config_digest, token)?;
    ensure!(
        config["os"] == "linux" && config["architecture"] == "amd64",
        "published controller platform mismatch"
    );
    let layers = value["layers"].as_array().context("missing image layers")?;
    ensure!(
        !layers.is_empty() && layers.len() <= 100,
        "invalid image layer count"
    );
    for layer in layers {
        request(&url("blobs", text(layer, "digest")?)?, Some(token), true)?;
    }
    Ok(digest == identity || config_digest == identity)
}

pub(super) fn verify(digest: &str, identity: &str) -> Result<()> {
    let repository = REPOSITORY.trim_start_matches("ghcr.io/");
    let response = request(
        &format!("https://ghcr.io/token?service=ghcr.io&scope=repository:{repository}:pull"),
        None,
        false,
    )?;
    let response: Value = serde_json::from_slice(&response)?;
    let token = text(&response, "token")?;
    ensure!(
        !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic()),
        "invalid anonymous pull token"
    );
    ensure!(
        manifest(digest, identity, token, 0)?,
        "published image differs from the verified local image"
    );
    Ok(())
}

#[cfg(test)]
pub(super) fn test_url(digest: &str) -> Result<String> {
    url("manifests", digest)
}
