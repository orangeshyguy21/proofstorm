use super::{bundle, sha256, text};
use crate::development::{directory, future_canonical, regular};
use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, GzBuilder, bufread::GzDecoder};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const MAX_ARCHIVE_BYTES: u64 = bundle::MAX_PAYLOAD_BYTES + 16 * 1024 * 1024;

pub(super) fn output_path(path: &Path) -> Result<PathBuf> {
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()),
        "linked output refused"
    );
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Resolve caller-selected parents (including macOS /tmp and /var aliases)
    // before applying strict no-link rules inside our private staging directories.
    future_canonical(&absolute)
}

fn archive_name(manifest: &Value) -> Result<String> {
    let suffix = if manifest["channel"] == "development" {
        let sha = text(&manifest["source"], "sha256")?;
        ensure!(sha256(sha), "invalid source digest");
        format!("-dev-{}-{}", text(manifest, "build_profile")?, &sha[..12])
    } else {
        String::new()
    };
    Ok(format!(
        "proofstorm-{}{suffix}-{}.tar.gz",
        text(manifest, "version")?,
        text(manifest, "target")?
    ))
}

fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".sha256");
    name.into()
}

fn installer_path(name: &str) -> bool {
    bundle::safe_name(name)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
}

fn write_archive(root: &Path, path: &Path, manifest: &Value) -> Result<()> {
    let mut names: BTreeSet<_> = manifest["files"]
        .as_object()
        .context("missing file inventory")?
        .keys()
        .cloned()
        .collect();
    names.insert("manifest.json".to_owned());
    let gzip = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(File::create(path)?, Compression::default());
    let mut tar = tar::Builder::new(gzip);
    for name in names {
        ensure!(
            installer_path(&name),
            "payload path is not installer-compatible: {name}"
        );
        let payload = root.join(&name);
        regular(&payload)?;
        let metadata = fs::metadata(&payload)?;
        // USTAR encodes regular paths without interpreting PAX/GNU extensions.
        let mut header = tar::Header::new_ustar();
        header.set_path(format!("proofstorm/{name}"))?;
        header.set_size(metadata.len());
        header.set_mode(if name.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append(&header, File::open(payload)?)?;
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

pub(super) fn pack(root: &Path, output: &Path) -> Result<Value> {
    bundle::verify(root)?;
    let root = root.canonicalize()?;
    let manifest = bundle::read_json(&root.join("manifest.json"))?;
    let output = output_path(output)?;
    ensure!(
        !output.starts_with(&root),
        "archive output must be outside the bundle"
    );
    directory(&output)?;
    let output = output.canonicalize()?;
    let name = archive_name(&manifest)?;
    let final_path = output.join(&name);
    let final_checksum = sidecar(&final_path);
    ensure!(
        fs::symlink_metadata(&final_path).is_err()
            && fs::symlink_metadata(&final_checksum).is_err(),
        "bundle output already exists"
    );
    let scratch = tempfile::Builder::new()
        .prefix(".proofstorm-package-")
        .tempdir_in(&output)?;
    let archive = scratch.path().join(&name);
    write_archive(&root, &archive, &manifest)?;
    let sha = bundle::checksum(&archive, fs::metadata(&archive)?.len())?;
    let receipt = format!("{sha}  {name}\n");
    fs::write(sidecar(&archive), &receipt)?;
    // Verify what will actually ship, not only the source tree before archiving.
    extract(&archive, &scratch.path().join("verified"))?;
    // Publish locally without replacing either half of an existing artifact pair.
    let mut checksum = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&final_checksum)?;
    checksum.write_all(receipt.as_bytes())?;
    checksum.sync_all()?;
    fs::hard_link(&archive, &final_path)?;
    Ok(
        json!({"archive": final_path, "sha256": sha, "release_ready": false,
        "release_blockers": manifest["release_blockers"]}),
    )
}

fn extract_payload(archive_path: &Path, staging: &Path) -> Result<()> {
    let reader = BufReader::new(File::open(archive_path)?);
    let mut archive = tar::Archive::new(GzDecoder::new(reader).take(MAX_ARCHIVE_BYTES));
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    // Raw entries prevent unbounded extension parsing and hidden sparse/link entries.
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        ensure!(
            entry.header().entry_type().is_file(),
            "archive links and special/extended entries are refused"
        );
        let name = std::str::from_utf8(&entry.path_bytes())?.to_owned();
        let relative = name
            .strip_prefix("proofstorm/")
            .context("unsafe archive member")?;
        ensure!(
            installer_path(relative) && relative.split('/').count() <= 64,
            "unsafe archive member: {name}"
        );
        ensure!(
            names.insert(name.clone()),
            "duplicate archive member: {name}"
        );
        ensure!(
            names.len() <= bundle::MAX_FILES,
            "archive exceeds file limit"
        );
        total = total
            .checked_add(entry.size())
            .context("archive size overflow")?;
        ensure!(
            total <= bundle::MAX_PAYLOAD_BYTES,
            "archive exceeds bundle size limits"
        );
        let expected_mode = if relative.starts_with("bin/") {
            0o755
        } else {
            0o644
        };
        ensure!(
            entry.header().mode()? == expected_mode,
            "unsafe archive mode: {name}"
        );
        let path = staging.join(&name);
        directory(path.parent().context("missing extraction parent")?)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let expected_size = entry.size();
        ensure!(
            std::io::copy(&mut entry, &mut file)? == expected_size,
            "truncated archive entry"
        );
        file.set_permissions(fs::Permissions::from_mode(expected_mode))?;
    }
    // Read the gzip trailer too: tar iteration alone stops at its end marker.
    // Reject concatenated/hidden content, while allowing conventional zero padding.
    let mut decoded = archive.into_inner();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = decoded.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        ensure!(
            buffer[..count].iter().all(|byte| *byte == 0),
            "nonzero data after tar end marker"
        );
    }
    ensure!(decoded.limit() > 0, "archive exceeds expanded size limit");
    ensure!(
        decoded.into_inner().into_inner().fill_buf()?.is_empty(),
        "trailing compressed archive data"
    );
    Ok(())
}

pub(super) fn extract(archive: &Path, destination: &Path) -> Result<Value> {
    ensure!(
        fs::symlink_metadata(archive)?.is_file(),
        "archive must be a regular unlinked file"
    );
    let canonical_archive = archive.canonicalize()?;
    let archive = canonical_archive.as_path();
    let canonical_destination = output_path(destination)?;
    let destination = canonical_destination.as_path();
    regular(archive)?;
    let size = fs::metadata(archive)?.len();
    ensure!(
        size <= MAX_ARCHIVE_BYTES,
        "archive exceeds compressed size limit"
    );
    regular(&sidecar(archive))?;
    ensure!(
        fs::metadata(sidecar(archive))?.len() <= 4096,
        "invalid checksum receipt"
    );
    let checksum = fs::read_to_string(sidecar(archive))?;
    let parts: Vec<_> = checksum.split_whitespace().collect();
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid archive filename")?;
    ensure!(
        parts.len() == 2
            && parts[1] == name
            && sha256(parts[0])
            && parts[0] == bundle::checksum(archive, size)?,
        "archive checksum mismatch"
    );
    ensure!(
        fs::symlink_metadata(destination).is_err(),
        "extraction destination already exists"
    );
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    directory(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".proofstorm-extract-")
        .tempdir_in(parent)?;
    // Snapshot the checked input so a changing caller-owned archive cannot swap
    // the bytes between checksum verification and decompression.
    let snapshot = staging.path().join("input.tar.gz");
    ensure!(
        std::io::copy(
            &mut File::open(archive)?.take(size + 1),
            &mut File::create(&snapshot)?
        )? == size,
        "archive changed during verification"
    );
    ensure!(
        fs::metadata(&snapshot)?.len() == size && bundle::checksum(&snapshot, size)? == parts[0],
        "archive changed during verification"
    );
    extract_payload(&snapshot, staging.path())?;
    let receipt = bundle::verify(&staging.path().join("proofstorm"))?;
    // Reserve the destination atomically. Never rename over an existing directory.
    fs::create_dir(destination)?;
    if let Err(error) = fs::rename(
        staging.path().join("proofstorm"),
        destination.join("proofstorm"),
    ) {
        let _ = fs::remove_dir(destination); // Only the empty directory just reserved.
        return Err(error.into());
    }
    Ok(receipt)
}

pub(super) fn cli(command: &str, mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let input = PathBuf::from(args.next().context("expected input path")?);
    let output = PathBuf::from(args.next().context("expected output directory")?);
    let json_output = match args.next() {
        None => false,
        Some(arg) if arg == "--json" => true,
        _ => bail!("expected only optional --json after the paths"),
    };
    ensure!(args.next().is_none(), "unexpected artifact arguments");
    let result = match command {
        "release-pack" => pack(&input, &output)?,
        "release-extract" => extract(&input, &output)?,
        _ => bail!("unknown archive command"),
    };
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if command == "release-pack" {
        println!("Bundle created: {}", text(&result, "archive")?);
    } else {
        println!(
            "Bundle extracted and verified: {}",
            output.join("proofstorm").display()
        );
    }
    if !json_output {
        println!("Release readiness remains unverified.");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
