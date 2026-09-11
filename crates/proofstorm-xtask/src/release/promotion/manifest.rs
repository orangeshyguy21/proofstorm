//! Public download contract, derived only from verified release assets.
use super::{PLATFORMS, reports, version};
use crate::{development::regular, release::bundle};
use anyhow::{Result, ensure};
use serde_json::{Map, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

pub(super) const NAME: &str = "release.json";

pub(super) struct Asset {
    pub(super) size_bytes: u64,
    pub(super) sha256: String,
}

impl Asset {
    pub(super) fn read_verified(path: &Path, expected: &str) -> Result<Self> {
        regular(path)?;
        let size_bytes = fs::metadata(path)?.len();
        // Bind the measured length to the bytes already checked by promotion.
        let sha256 = bundle::checksum(path, size_bytes)?;
        ensure!(sha256 == expected, "release asset changed after validation");
        Ok(Self { size_bytes, sha256 })
    }

    pub(super) fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }
}

pub(super) fn contents(
    repo: &str,
    tag: &str,
    revision: &str,
    assets: &BTreeMap<String, Asset>,
) -> Result<Vec<u8>> {
    let version = version(tag)?;
    let mut platforms = Map::new();
    for (slug, target) in PLATFORMS {
        let (os, arch) = slug.split_once('-').expect("fixed platform slug");
        let archive = format!("proofstorm-{version}-{slug}.tar.gz");
        platforms.insert(
            slug.into(),
            json!({"os":os,"arch":arch,"target":target,"archive":archive,"checksum":format!("{archive}.sha256")}),
        );
    }
    let assets: Map<_, _> = assets
        .iter()
        .map(|(name, asset)| {
            (
                name.clone(),
                json!({"size_bytes":asset.size_bytes,"sha256":asset.sha256}),
            )
        })
        .collect();
    // No timestamp or local paths: repeated verification must produce identical bytes.
    // The manifest cannot hash itself; the private promotion receipt does that.
    let mut bytes = serde_json::to_vec_pretty(&json!({
        "schema_version":1,
        "version":version,
        "tag":tag,
        "source_commit":revision,
        "channel":"alpha",
        "repository":repo,
        "installer":"install.sh",
        "verification_reports":reports::NAME,
        "platforms":platforms,
        "assets":assets,
    }))?;
    bytes.push(b'\n');
    Ok(bytes)
}
