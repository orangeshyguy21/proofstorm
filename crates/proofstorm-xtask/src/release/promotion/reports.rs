//! Deterministic public evidence archive; no payload binaries are executed.
use super::{PLATFORMS, REPORTS, manifest::Asset};
use crate::{
    development::regular,
    release::{MAX_METADATA_BYTES, archive::output_path},
};
use anyhow::{Result, ensure};
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};

pub(super) const NAME: &str = "verification-reports.tar.gz";

fn contents(candidate: &Path, expected: Option<&BTreeMap<String, String>>) -> Result<Vec<u8>> {
    let mut names = BTreeMap::new();
    for (platform, _) in PLATFORMS {
        for report in REPORTS {
            names.insert(
                format!("{report}-{platform}.json"),
                format!("{platform}/{report}.json"),
            );
        }
    }
    let gzip = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(gzip);
    for (name, relative) in names {
        let source = candidate.join(&relative);
        regular(&source)?;
        let mut bytes = Vec::new();
        fs::File::open(&source)?
            .take(MAX_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_METADATA_BYTES,
            "verification report is too large"
        );
        serde_json::from_slice::<serde_json::Value>(&bytes)?;
        if let Some(expected) = expected {
            ensure!(
                expected.get(&relative) == Some(&format!("{:x}", Sha256::digest(&bytes))),
                "verification report changed after validation"
            );
        }
        let mut header = tar::Header::new_ustar();
        header.set_path(name)?;
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive.append(&header, bytes.as_slice())?;
    }
    Ok(archive.into_inner()?.finish()?)
}

pub(super) fn asset(candidate: &Path, files: &BTreeMap<String, String>) -> Result<Asset> {
    Ok(Asset::from_bytes(&contents(candidate, Some(files))?))
}

pub(super) fn pack(candidate: &Path, destination: &Path) -> Result<()> {
    let bytes = contents(candidate, None)?;
    let destination = output_path(destination)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    Ok(())
}
