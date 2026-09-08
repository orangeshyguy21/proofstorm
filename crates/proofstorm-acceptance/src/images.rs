//! Provision exact catalog images in the disposable k3d registry.
use std::{collections::BTreeSet, process::Command};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::Kubectl;

const LOCAL_REGISTRY: &str = "proofstorm-registry.localhost:5000/";

fn catalog_images() -> BTreeSet<String> {
    proofstorm_core::default_catalog()
        .entries
        .iter()
        .map(|entry| entry.image.clone())
        .collect()
}

fn local_reference(image: &str) -> Option<(&str, &str)> {
    image.strip_prefix(LOCAL_REGISTRY)?.split_once('@')
}

fn registry_has(repository: &str, digest: &str) -> Result<bool> {
    let output = Command::new("curl").args([
        "--silent", "--show-error", "--fail", "--head", "--max-time", "15",
        "-H", "Accept: application/vnd.oci.image.index.v1+json,application/vnd.oci.image.manifest.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.docker.distribution.manifest.v2+json",
        &format!("http://127.0.0.1:5111/v2/{repository}/manifests/{digest}"),
    ]).output().context("query local image registry")?;
    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("docker-content-digest") && value.trim() == digest
            })
        }))
}

fn cached_reference(rows: &str, digest: &str) -> Result<Option<String>> {
    for row in rows.lines().filter(|line| !line.is_empty()) {
        let image: Value = serde_json::from_str(row).context("parse Docker image inventory")?;
        if image["Digest"] == digest {
            if let Some(repository) = image["Repository"].as_str().filter(|r| *r != "<none>") {
                return Ok(Some(format!("{repository}@{digest}")));
            }
        }
    }
    Ok(None)
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    if !Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("run {program}"))?
        .success()
    {
        bail!("{program} {} failed", args.join(" "));
    }
    Ok(())
}

fn upstream_reference(repository: &str, digest: &str) -> Option<String> {
    let source = repository.strip_prefix("upstream/")?;
    (source.starts_with("docker.io/") || source.starts_with("quay.io/"))
        .then(|| format!("{source}@{digest}"))
}

/// Mirror publisher images by digest and restore local builds from exact cached
/// digests. A rebuild is never silently substituted for a reviewed artifact.
pub fn provision() -> Result<()> {
    let cache = Command::new("docker")
        .args(["image", "ls", "--digests", "--format", "{{json .}}"])
        .output()
        .context("read Docker image cache")?;
    if !cache.status.success() {
        bail!("cannot read Docker image cache; start Docker first");
    }
    let rows = String::from_utf8(cache.stdout)?;
    for image in catalog_images() {
        let Some((repository, digest)) = local_reference(&image) else {
            continue;
        };
        if registry_has(repository, digest)? {
            println!("Catalog image available: {image}");
            continue;
        }
        let tag = format!(
            "localhost:5111/{repository}:catalog-{}",
            digest
                .strip_prefix("sha256:")
                .context("catalog image must use a sha256 digest")?
        );
        if let Some(source) = upstream_reference(repository, digest) {
            println!("Mirroring publisher image: {source}");
            run(
                "docker",
                &[
                    "buildx",
                    "imagetools",
                    "create",
                    "--prefer-index=false",
                    "--tag",
                    &tag,
                    &source,
                ],
            )?;
        } else {
            let cached = cached_reference(&rows, digest)?.with_context(|| format!(
                "required catalog image is missing from both the local registry and Docker cache: {image}\nRestore the exact pinned image with docker load/pull, then run make images. A source rebuild may produce a different digest and must be reviewed as a catalog update; setup cannot silently substitute it."
            ))?;
            println!("Restoring catalog image: {image}");
            run("docker", &["tag", &cached, &tag])?;
            run("docker", &["push", &tag])?;
        }
        if !registry_has(repository, digest)? {
            bail!("registry did not retain the exact catalog digest for {image}");
        }
    }
    Ok(())
}

/// Prove every catalog image is pullable by each schedulable local cluster node.
pub fn verify(kubectl: &Kubectl) -> Result<()> {
    let inventory = kubectl.get_json(&["get", "nodes"])?;
    let nodes = inventory["items"]
        .as_array()
        .context("cluster node list is missing")?
        .iter()
        .filter(|node| node["spec"]["unschedulable"] != true)
        .map(|node| {
            node["metadata"]["name"]
                .as_str()
                .context("node name missing")
        })
        .collect::<Result<Vec<_>>>()?;
    if nodes.is_empty() {
        bail!("no schedulable cluster nodes found");
    }
    for image in catalog_images() {
        if let Some((repository, digest)) = local_reference(&image) {
            if !registry_has(repository, digest)? {
                bail!(
                    "catalog image missing: {image}\nRun make images to restore required local images, then rerun make doctor."
                );
            }
        }
        for node in &nodes {
            if !node.starts_with("k3d-proofstorm-") {
                bail!("image checks require a local k3d-proofstorm node, got {node}");
            }
            println!("Checking image on {node}: {image}");
            run("docker", &["exec", node, "crictl", "--timeout=120s", "pull", &image])
                .with_context(|| format!("cluster cannot pull catalog image {image}; inspect registry access, platform support, and the pinned image; local images can be restored with make images"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restoration_uses_exact_digests_not_mutable_tags() {
        let rows = "{\"Repository\":\"localhost:5111/wallet\",\"Tag\":\"latest\",\"Digest\":\"sha256:wrong\"}\n{\"Repository\":\"other/wallet\",\"Tag\":\"old\",\"Digest\":\"sha256:required\"}";
        assert_eq!(
            cached_reference(rows, "sha256:required")
                .unwrap()
                .as_deref(),
            Some("other/wallet@sha256:required")
        );
        assert!(cached_reference(rows, "sha256:absent").unwrap().is_none());
        assert_eq!(
            local_reference("proofstorm-registry.localhost:5000/wallet@sha256:required"),
            Some(("wallet", "sha256:required"))
        );
        assert!(local_reference("example.com/wallet@sha256:required").is_none());
    }
    #[test]
    fn local_catalog_images_are_in_the_setup_inventory() {
        let images = catalog_images();
        assert!(
            images
                .iter()
                .any(|image| image.starts_with(&format!("{LOCAL_REGISTRY}cdk-cli-wallet@")))
        );
        assert!(
            images
                .iter()
                .any(|image| image.starts_with(&format!("{LOCAL_REGISTRY}cocod-wallet@")))
        );
    }

    #[test]
    fn publisher_mirrors_retain_the_registry_repository_and_digest() {
        assert_eq!(
            upstream_reference("upstream/docker.io/lightninglabs/lnd", "sha256:exact"),
            Some("docker.io/lightninglabs/lnd@sha256:exact".into())
        );
        assert_eq!(
            upstream_reference("upstream/quay.io/keycloak/keycloak", "sha256:exact"),
            Some("quay.io/keycloak/keycloak@sha256:exact".into())
        );
        assert!(upstream_reference("bitcoin-core", "sha256:exact").is_none());
        assert!(upstream_reference("upstream/unreviewed.example/image", "sha256:exact").is_none());
        assert!(
            catalog_images()
                .iter()
                .all(|image| local_reference(image).is_some())
        );
    }
}
