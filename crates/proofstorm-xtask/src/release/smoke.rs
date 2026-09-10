//! Relocation checks execute trusted local build outputs, never arbitrary downloads.
use super::{archive, build::host_target, bundle, text};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn execute(binary: &Path, flag: &str, destination: &Path, policy: Option<&str>) -> Result<Vec<u8>> {
    let mut command = if let Some(policy) = policy {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command.args(["-p", policy]).arg(binary);
        command
    } else {
        Command::new(binary)
    };
    let environment: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !key.starts_with("PROOFSTORM_")
                && !key.starts_with("TRUNK_")
                && !key.starts_with("K3D_")
                && !matches!(key.as_ref(), "CARGO_BUILD_TARGET" | "CARGO_TARGET_DIR")
        })
        .collect();
    let mut output = tempfile::tempfile()?;
    let mut child = command
        .arg(flag)
        .current_dir(destination)
        .env_clear()
        .envs(environment)
        .env("PROOFSTORM_HOME", destination.join("must-not-be-created"))
        .env("PROOFSTORM_PRINCIPAL", "")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::inherit())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "relocated {} {flag} failed: {status}",
                binary.display()
            );
            break;
        }
        if Instant::now() >= deadline || output.metadata()?.len() > super::MAX_METADATA_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("relocated command exceeded its time/output limit");
        }
        thread::sleep(Duration::from_millis(25));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output
        .take(super::MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= super::MAX_METADATA_BYTES,
        "empty or oversized executable output"
    );
    Ok(bytes)
}

fn smoke(archive_path: &Path, destination: &Path, deny_sources: &[PathBuf]) -> Result<Value> {
    ensure!(
        deny_sources.is_empty() || cfg!(target_os = "macos"),
        "--deny-source requires macOS sandbox-exec"
    );
    let policy = if deny_sources.is_empty() {
        None
    } else {
        let mut policy = String::from("(version 1) (allow default)");
        for source in deny_sources {
            write!(
                policy,
                " (deny file-read* (subpath {}))",
                serde_json::to_string(
                    &source
                        .canonicalize()?
                        .to_str()
                        .context("invalid source path")?
                )?
            )?;
        }
        Some(policy)
    };
    let destination = archive::output_path(destination)?;
    archive::extract(archive_path, &destination)?;
    let root = destination.join("proofstorm");
    let manifest = bundle::read_json(&root.join("manifest.json"))?;
    ensure!(
        text(&manifest, "target")? == host_target()?,
        "smoke must run on the bundle's target host"
    );
    let expected = bundle::read_json(&root.join("release-info.json"))?;
    for (name, metadata_flag) in [
        ("proofstorm", "release-info"),
        ("proofstorm-mcp", "--release-info"),
    ] {
        let binary = root.join("bin").join(name);
        for flag in ["--version", "--help"] {
            execute(&binary, flag, &destination, policy.as_deref())?;
        }
        let embedded: Value = serde_json::from_slice(&execute(
            &binary,
            metadata_flag,
            &destination,
            policy.as_deref(),
        )?)?;
        ensure!(
            embedded == expected,
            "relocated metadata mismatch for {name}"
        );
    }
    ensure!(
        fs::symlink_metadata(destination.join("must-not-be-created")).is_err()
            && fs::symlink_metadata(destination.join(".proofstorm")).is_err(),
        "metadata command created runtime state"
    );
    let report = json!({"integrity_verified": true, "relocated_binaries_verified": true,
        "source_read_access_denied": !deny_sources.is_empty(), "release_ready": manifest["release_ready"]});
    fs::write(
        destination.join("smoke-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

pub(super) fn cli(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let archive = PathBuf::from(args.next().context("expected archive")?);
    let destination = PathBuf::from(args.next().context("expected new destination")?);
    let mut deny_sources = Vec::new();
    let mut json_output = false;
    while let Some(arg) = args.next() {
        if arg == "--deny-source" {
            deny_sources.push(PathBuf::from(
                args.next().context("expected denied source directory")?,
            ));
        } else if arg == "--json" {
            json_output = true;
        } else {
            anyhow::bail!("unexpected relocation option");
        }
    }
    let result = smoke(&archive, &destination, &deny_sources)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Bundle integrity and relocated CLI/MCP checks passed.\nRuntime and release readiness remain unverified."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
