//! Checkout builds publish only into their verified installation-local registry.
use super::{cluster, digest, docker, process};
use crate::installation::{CATALOG_REGISTRY, Installation};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{fs, path::Path};

const RECEIPT: &str = "checkout-controller.json";

fn metadata(value: &Value, sha: &str) -> Result<()> {
    ensure!(
        value["format_version"] == 1
            && value["source_sha256"] == sha
            && value["version"] == env!("CARGO_PKG_VERSION")
            && value["runtime_contract_sha256"] == crate::release::runtime_contract_sha256(),
        "checkout controller/client compatibility or source mismatch; rebuild before deployment"
    );
    Ok(())
}

fn built_identity(inspect: &Value, build_id: &str, sha: &str) -> Result<String> {
    let image_id = inspect["Id"]
        .as_str()
        .context("built image identity missing")?;
    ensure!(
        image_id.strip_prefix("sha256:").is_some_and(digest)
            && (image_id == build_id
                || inspect["Descriptor"]["digest"] == build_id
                || inspect["Descriptor"]["annotations"]["config.digest"] == build_id)
            && inspect["Os"] == "linux"
            && inspect["Architecture"] == crate::platform::container_arch()?
            && inspect["Config"]["Labels"]["dev.proofstorm.source-sha256"] == sha,
        "built controller identity/platform/provenance mismatch"
    );
    Ok(image_id.into())
}

fn cached(installation: &Installation, sha: &str) -> Result<Option<Value>> {
    let path = installation.home.join(RECEIPT);
    let stat = match fs::symlink_metadata(&path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        stat.is_file()
            && stat.nlink() == 1
            && stat.len() < 65536
            && stat.permissions().mode().trailing_zeros() >= 6,
        "invalid private controller receipt"
    );
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    validate_receipt(installation, &value)?;
    if value["source_sha256"] != sha {
        return Ok(None);
    }
    metadata(&value["metadata"], sha)?;
    Ok(Some(value))
}

fn validate_receipt(installation: &Installation, value: &Value) -> Result<()> {
    ensure!(
        value["format_version"] == 1
            && value["installation_id"] == installation.id
            && value["release_ready"] == false
            && value["image"]
                .as_str()
                .and_then(|s| s.strip_prefix(&format!("{CATALOG_REGISTRY}/proofstormd@sha256:")))
                .is_some_and(digest)
            && value["image_id"]
                .as_str()
                .and_then(|s| s.strip_prefix("sha256:"))
                .is_some_and(digest)
            && value["source_sha256"].as_str().is_some_and(digest)
            && value["metadata"]["source_sha256"] == value["source_sha256"],
        "controller receipt is foreign or malformed; refusing replacement"
    );
    Ok(())
}

pub(super) fn current(installation: &Installation, sha: &str) -> Result<Value> {
    cached(installation, sha)?.with_context(|| {
        format!(
            "checkout controller has not been deployed for this build; run {} setup",
            crate::command_name()
        )
    })
}

fn manifest(installation: &Installation, image: &str) -> Result<String> {
    let value: Value = serde_json::from_str(&docker(
        &installation.home,
        &[
            "buildx",
            "imagetools",
            "inspect",
            image,
            "--format",
            "{{json .Manifest}}",
        ],
        30,
    )?)?;
    let sha = value["digest"]
        .as_str()
        .context("registry manifest digest missing")?;
    ensure!(
        sha.strip_prefix("sha256:").is_some_and(digest),
        "invalid registry manifest digest"
    );
    Ok(sha.into())
}

fn verify_published_image(installation: &Installation, image: &str, image_id: &str) -> Result<()> {
    // Docker's containerd image store reports the manifest/index ID, while the
    // classic store reports the config ID. Both are immutable content identities.
    if image
        .split_once('@')
        .is_some_and(|(_, sha)| sha == image_id)
    {
        return Ok(());
    }
    let raw = |image: &str| -> Result<Value> {
        Ok(serde_json::from_str(&docker(
            &installation.home,
            &["buildx", "imagetools", "inspect", "--raw", image],
            30,
        )?)?)
    };
    let mut value = raw(image)?;
    if let Some(manifests) = value["manifests"].as_array() {
        ensure!(
            manifests.len() == 1
                && manifests[0]["platform"]["os"] == "linux"
                && manifests[0]["platform"]["architecture"] == crate::platform::container_arch()?,
            "unexpected published controller platforms"
        );
        let child = manifests[0]["digest"]
            .as_str()
            .context("controller child manifest missing")?;
        ensure!(
            child.strip_prefix("sha256:").is_some_and(digest),
            "invalid controller child digest"
        );
        value = raw(&format!(
            "{}/proofstormd@{child}",
            installation.host_registry()
        ))?;
    }
    ensure!(
        value["config"]["digest"] == image_id,
        "published controller differs from verified local image"
    );
    Ok(())
}

fn reusable(installation: &Installation, sha: &str) -> Result<Option<Value>> {
    if let Some(value) = cached(installation, sha)? {
        let expected = value["image"]
            .as_str()
            .context("controller image missing")?
            .split_once('@')
            .context("controller digest missing")?
            .1;
        let host = format!("{}/proofstormd@{expected}", installation.host_registry());
        if manifest(installation, &host).is_ok_and(|actual| actual == expected) {
            verify_published_image(
                installation,
                &host,
                value["image_id"]
                    .as_str()
                    .context("controller ID missing")?,
            )?;
            return Ok(Some(value));
        }
    }
    Ok(None)
}

pub(super) fn prepare(
    installation: &Installation,
    source: &Path,
    sha: &str,
    progress: &dyn Fn(&str),
) -> Result<Value> {
    cluster::owned(installation)?;
    if let Some(value) = reusable(installation, sha)? {
        progress("Reusing verified controller");
        return Ok(value);
    }
    ensure!(
        digest(sha) && source.is_absolute() && source.join("Dockerfile.proofstormd").is_file(),
        "invalid registered controller snapshot"
    );
    progress("Building checkout controller (may take minutes)");
    let iid = tempfile::NamedTempFile::new_in(&installation.home)?;
    let tag = format!("proofstorm-checkout:{}-{sha}", installation.id);
    process::controller_build(
        &installation.home,
        &[
            "buildx",
            "build",
            "--platform",
            &crate::platform::container_platform()?,
            "--load",
            "--provenance=false",
            "--progress",
            "plain",
            "--build-arg",
            "CARGO_BUILD_JOBS=2",
            "--build-arg",
            &format!("PROOFSTORM_CONTROLLER_SOURCE_SHA256={sha}"),
            "--iidfile",
            iid.path().to_str().context("image receipt path")?,
            "--file",
            source
                .join("Dockerfile.proofstormd")
                .to_str()
                .context("controller recipe path")?,
            "--tag",
            &tag,
            source.to_str().context("controller source path")?,
        ],
    )?;
    let build_id = fs::read_to_string(iid.path())?.trim().to_owned();
    ensure!(
        build_id.strip_prefix("sha256:").is_some_and(digest),
        "invalid built controller ID"
    );
    let inspect: Value = serde_json::from_str(&docker(
        &installation.home,
        &["image", "inspect", &tag, "--format", "{{json .}}"],
        15,
    )?)?;
    let image_id = built_identity(&inspect, &build_id, sha)?;
    let info = super::controller_metadata(&installation.home, &image_id)?;
    metadata(&info, sha)?;
    // The only publication destination is the owned loopback registry, never GHCR.
    cluster::owned(installation)?;
    let destination = format!(
        "{}/proofstormd:checkout-{sha}",
        installation.host_registry()
    );
    docker(&installation.home, &["tag", &image_id, &destination], 15)?;
    docker(&installation.home, &["push", &destination], 300)?;
    let registry_sha = manifest(installation, &destination)?;
    verify_published_image(
        installation,
        &format!(
            "{}/proofstormd@{registry_sha}",
            installation.host_registry()
        ),
        &image_id,
    )?;
    let image = format!("{CATALOG_REGISTRY}/proofstormd@{registry_sha}");
    for node in cluster::nodes(installation) {
        cluster::owned(installation)?;
        docker(
            &installation.home,
            &["exec", &node, "crictl", "--timeout=120s", "pull", &image],
            150,
        )?;
    }
    let value = json!({"format_version":1, "installation_id":installation.id, "source_sha256":sha,
        "image_id":image_id, "image":image, "metadata":info, "release_ready":false});
    validate_receipt(installation, &value)?;
    process::save(
        &installation.home.join(RECEIPT),
        &serde_json::to_vec(&value)?,
    )?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn docker_classic_and_containerd_ids_are_bound_to_the_build_receipt() {
        let id = format!("sha256:{}", "a".repeat(64));
        let config = format!("sha256:{}", "b".repeat(64));
        let sha = "c".repeat(64);
        let host_arch = crate::platform::container_arch().unwrap();
        let mut inspect = json!({"Id":id,"Os":"linux","Architecture":host_arch,
            "Config":{"Labels":{"dev.proofstorm.source-sha256":sha}}});
        assert_eq!(built_identity(&inspect, &id, &sha).unwrap(), id);
        assert!(built_identity(&inspect, &config, &sha).is_err());
        inspect["Descriptor"] = json!({"digest":id,"annotations":{"config.digest":config}});
        assert_eq!(built_identity(&inspect, &config, &sha).unwrap(), id);
        // Neither supported architecture is universally wrong: CI is AMD64,
        // while development on Apple Silicon expects ARM64.
        for image_arch in ["amd64", "arm64"] {
            inspect["Architecture"] = json!(image_arch);
            assert_eq!(
                built_identity(&inspect, &config, &sha).is_ok(),
                image_arch == host_arch,
                "image architecture {image_arch}, build host {host_arch}"
            );
        }
    }
    fn fixture(home: &Path) -> (Installation, Value) {
        let installation = Installation {
            format_version: 1,
            id: "a".repeat(32),
            home: home.into(),
            api_port: 42101,
            registry_port: 42102,
        };
        let sha = "b".repeat(64);
        let value = json!({"format_version":1, "installation_id":installation.id, "source_sha256":sha,
            "release_ready":false, "image_id":format!("sha256:{}", "c".repeat(64)),
            "image":format!("{CATALOG_REGISTRY}/proofstormd@sha256:{}", "d".repeat(64)),
            "metadata":{"format_version":1,"version":env!("CARGO_PKG_VERSION"),"source_sha256":sha,"runtime_contract_sha256":crate::release::runtime_contract_sha256()}});
        (installation, value)
    }
    #[test]
    fn receipts_are_installation_bound_and_stale_builds_require_setup() {
        let home = tempfile::tempdir().unwrap();
        let (installation, mut value) = fixture(home.path());
        assert!(current(&installation, &"b".repeat(64)).is_err());
        process::save(
            &home.path().join(RECEIPT),
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(current(&installation, &"b".repeat(64)).is_ok());
        assert!(current(&installation, &"e".repeat(64)).is_err());
        value["installation_id"] = json!("foreign");
        assert!(validate_receipt(&installation, &value).is_err());
        let (_, mut value) = fixture(home.path());
        value["image"] = json!(format!("ghcr.io/foreign/image@sha256:{}", "d".repeat(64)));
        assert!(validate_receipt(&installation, &value).is_err());
    }
    #[test]
    fn mismatched_controller_metadata_and_linked_receipts_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let (installation, mut value) = fixture(home.path());
        value["metadata"]["runtime_contract_sha256"] = json!("other");
        assert!(metadata(&value["metadata"], &"b".repeat(64)).is_err());
        fs::write(home.path().join("foreign"), "{}").unwrap();
        std::os::unix::fs::symlink(home.path().join("foreign"), home.path().join(RECEIPT)).unwrap();
        assert!(cached(&installation, &"b".repeat(64)).is_err());
    }
}
