//! Checked public inputs and receipts for the Bash source-free installer test.
use super::{archive::output_path, bundle, text};
use crate::development::regular;
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const IMAGE: &str =
    "debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171";

fn input_file(path: &Path) -> Result<PathBuf> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "installer input symlink/non-file refused"
    );
    // Resolve caller-selected parents, including macOS /tmp and /var aliases.
    let path = path.canonicalize()?;
    regular(&path)?;
    Ok(path)
}

fn checksum(path: &Path, max: u64) -> Result<String> {
    let path = input_file(path)?;
    let size = fs::metadata(&path)?.len();
    ensure!(size <= max, "installer input is too large");
    bundle::checksum(&path, size)
}

fn input_digests(archive: &Path, installer: &Path) -> Result<(String, String)> {
    input_digests_for(archive, installer, "x86_64-unknown-linux-gnu")
}

pub(super) fn input_digests_for(
    archive: &Path,
    installer: &Path,
    target: &str,
) -> Result<(String, String)> {
    let name = archive
        .file_name()
        .and_then(|v| v.to_str())
        .context("invalid archive name")?;
    ensure!(
        name.starts_with("proofstorm-")
            && (name.ends_with(&format!("-{}.tar.gz", super::artifact_platform(target)?))
                || name.ends_with(&format!("-{target}.tar.gz")))
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
        "expected a {target} archive"
    );
    let mut receipt = archive.as_os_str().to_owned();
    receipt.push(".sha256");
    let receipt = input_file(Path::new(&receipt))?;
    ensure!(
        fs::metadata(&receipt)?.len() <= 1024,
        "checksum receipt is too large"
    );
    let digest = checksum(archive, bundle::MAX_PAYLOAD_BYTES + 16 * 1024 * 1024)?;
    ensure!(
        fs::read_to_string(receipt)?
            .split_whitespace()
            .collect::<Vec<_>>()
            == [digest.as_str(), name],
        "archive checksum mismatch"
    );
    Ok((digest, checksum(installer, super::MAX_METADATA_BYTES)?))
}

fn prepare(
    source: &Path,
    archive: &Path,
    installer: &Path,
    work: &Path,
    development: bool,
) -> Result<Vec<OsString>> {
    let work = output_path(work)?;
    ensure!(
        !work.starts_with(source.canonicalize()?),
        "choose a work directory outside the checkout"
    );
    ensure!(
        fs::symlink_metadata(&work).is_err(),
        "smoke work directory must be new"
    );
    let (archive_digest, installer_digest) = input_digests(archive, installer)?;
    let name = archive.file_name().context("missing archive name")?;
    // Reserve exactly the requested directory; never replace existing contents.
    fs::create_dir(&work)?;
    let inputs = work.join("input");
    fs::create_dir(&inputs)?;
    fs::set_permissions(&inputs, fs::Permissions::from_mode(0o755))?;
    fs::copy(archive, inputs.join(name))?;
    let receipt_name = format!("{}.sha256", name.to_str().context("invalid archive name")?);
    fs::write(
        inputs.join(&receipt_name),
        format!("{archive_digest}  {}\n", name.to_string_lossy()),
    )?;
    fs::copy(installer, inputs.join("install.sh"))?;
    for file in [
        inputs.join(name),
        inputs.join(receipt_name),
        inputs.join("install.sh"),
    ] {
        fs::set_permissions(file, fs::Permissions::from_mode(0o644))?;
    }
    ensure!(
        input_digests(&inputs.join(name), &inputs.join("install.sh"))?
            == (archive_digest.clone(), installer_digest.clone()),
        "installer inputs changed during staging"
    );
    let reservation = tempfile::Builder::new()
        .prefix("proofstorm-linux-install-")
        .tempdir_in(&work)?;
    let container = reservation
        .path()
        .file_name()
        .context("missing container name")?
        .to_string_lossy()
        .to_ascii_lowercase();
    reservation.close()?;
    let tag = format!("{container}:inputs");
    fs::write(
        work.join("Dockerfile"),
        format!("FROM {IMAGE}\nCOPY --chown=1000:1000 input/ /input/\n"),
    )?;
    fs::write(
        work.join(".dockerignore"),
        "*\n!Dockerfile\n!input/\n!input/**\n",
    )?;
    let run = json!({
        "container": container, "base_image": IMAGE, "input_image": tag,
        "archive": name.to_str(), "archive_sha256": archive_digest,
        "installer_sha256": installer_digest, "network": "none", "host_mounts": [],
        "privileged": false, "user": "1000:1000", "development_override": development
    });
    fs::write(work.join("run.json"), serde_json::to_vec_pretty(&run)?)?;
    Ok(vec![
        work.into_os_string(),
        container.into(),
        tag.into(),
        name.to_owned(),
    ])
}

fn finish(work: &Path, status: &str) -> Result<()> {
    let work = output_path(work)?;
    ensure!(
        status == "0",
        "source-free installer check failed (container exit {status})"
    );
    let run = bundle::read_json(&work.join("run.json"))?;
    let name = text(&run, "archive")?;
    ensure!(
        bundle::safe_name(name) && !name.contains('/'),
        "invalid staged archive name"
    );
    let (archive, installer) = input_digests(
        &work.join("input").join(name),
        &work.join("input/install.sh"),
    )?;
    ensure!(
        run["archive_sha256"] == archive && run["installer_sha256"] == installer,
        "staged installer inputs changed"
    );
    ensure!(
        run["development_override"].is_boolean(),
        "invalid development override"
    );
    let report = json!({
        "local_install": true, "reinstall": true, "cli_mcp_metadata_match": true,
        "source_checkout_present": false, "build_tools_present": false,
        "network_enabled": false, "runtime_tested": false, "github_download_tested": false,
        "archive_sha256": archive, "installer_sha256": installer,
        "development_override": run["development_override"]
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(work.join("install-smoke-report.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn bounded(args: &[OsString]) -> Result<()> {
    ensure!(args.len() >= 2, "expected timeout seconds and command");
    let seconds: u64 = args[0].to_str().context("invalid timeout")?.parse()?;
    ensure!(
        (1..=3600).contains(&seconds),
        "timeout must be 1–3600 seconds"
    );
    let mut child = Command::new(&args[1]).args(&args[2..]).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "command failed: {status}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("command exceeded {seconds}s deadline");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn cli(command: &str, args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<_> = args.collect();
    match command {
        "linux-install-prepare" => {
            ensure!(
                args.len() == 5,
                "expected checkout, archive, installer, new work directory, development boolean"
            );
            let development = match args[4].to_str() {
                Some("true") => true,
                Some("false") => false,
                _ => bail!("invalid development boolean"),
            };
            for field in prepare(
                Path::new(&args[0]),
                Path::new(&args[1]),
                Path::new(&args[2]),
                Path::new(&args[3]),
                development,
            )? {
                std::io::stdout().write_all(field.as_encoded_bytes())?;
                std::io::stdout().write_all(&[0])?;
            }
            Ok(())
        }
        "linux-install-finish" => {
            ensure!(
                args.len() == 2,
                "expected work directory and container exit code"
            );
            finish(
                Path::new(&args[0]),
                args[1].to_str().context("invalid exit code")?,
            )
        }
        "release-run" => bounded(&args),
        _ => bail!("invalid Linux installer helper command"),
    }
}

#[cfg(test)]
mod tests;
