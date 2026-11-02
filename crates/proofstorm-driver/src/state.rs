//! Owned native state: bounded reads of projected files, seeds, durable
//! writes, and binding a volume to its identity once.
use std::{
    fs,
    io::{Read as _, Write as _},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};

/// Read a regular file of at most `limit` bytes. Projected Secret files are
/// symlinks, so the link is followed.
pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path).context("read required file")?;
    ensure!(
        file.metadata()?.is_file(),
        "required file must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "required file exceeds its size limit"
    );
    Ok(bytes)
}

/// A projected BIP39 seed.
pub(crate) fn mnemonic(path: &Path) -> Result<String> {
    let bytes = read_bounded(path, 512)?;
    let value = std::str::from_utf8(&bytes).context("mnemonic is not UTF-8")?;
    // Do not attach the parser error: credential words must never reach diagnostics.
    value
        .trim()
        .parse::<bip39::Mnemonic>()
        .map(|seed| seed.to_string())
        .map_err(|_| anyhow::anyhow!("invalid mnemonic"))
}

/// What a volume's marker holds: the component and a hash of its seed.
pub(crate) fn identity_record(kind: &str, seed: &str) -> String {
    format!(
        "proofstorm-{kind}/v1\n{:x}\n",
        Sha256::digest(seed.as_bytes())
    )
}

/// Write a file only its owner can read (mode 0600), durably and atomically.
/// Without `replace`, an existing file is kept and the write fails.
pub(crate) fn write(path: &Path, contents: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().context("state directory missing")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    if replace {
        temporary.persist(path).context("replace runtime file")?;
    } else {
        temporary
            .persist_noclobber(path)
            .context("write native state once")?;
    }
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Refuse a volume holding anything but an empty `lost+found`, which a
/// freshly formatted PVC can contain.
pub(crate) fn fresh(data: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(data)?.is_dir(),
        "native state volume must be a directory"
    );
    for entry in fs::read_dir(data)? {
        let entry = entry?;
        ensure!(
            entry.file_name() == "lost+found"
                && entry.file_type()?.is_dir()
                && fs::read_dir(entry.path())?.next().is_none(),
            "native state exists without its identity; refusing reinitialization"
        );
    }
    Ok(())
}

/// Bind an owned volume to `record` once: a fresh volume gets the marker, a
/// bound one must hold the same record. Returns whether it was already bound.
pub(crate) fn bind_marker(data: &Path, marker: &str, record: &str) -> Result<bool> {
    ensure!(
        fs::symlink_metadata(data)?.is_dir(),
        "native state volume must be a directory"
    );
    let marker = data.join(marker);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            ensure!(metadata.is_file(), "identity marker must be a regular file");
            ensure!(
                read_bounded(&marker, 128)? == record.as_bytes(),
                "identity mismatch; refusing to replace existing state"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fresh(data)?;
            write(&marker, record.as_bytes(), false)?;
            Ok(false)
        }
        Err(error) => Err(error).context("inspect identity marker"),
    }
}
