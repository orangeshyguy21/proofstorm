use super::process;
use anyhow::{Context, Result, ensure};
use proofstorm_core::tool_pins::{Pins, Tool};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) fn pins() -> Result<Vec<Tool>> {
    pins_for(crate::platform::target())
}

fn pins_for(target: &str) -> Result<Vec<Tool>> {
    Ok(
        Pins::parse(target, crate::platform::bootstrap_pins_for(target)?)
            .map_err(anyhow::Error::msg)?
            .tools,
    )
}

pub(super) fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn path(home: &Path, tool: &Tool) -> PathBuf {
    home.join("tools")
        .join(format!("{}-{}", tool.name, tool.executable_sha256))
}

pub(super) fn verified(home: &Path, tool: &Tool) -> Result<PathBuf> {
    let path = path(home, tool);
    ensure!(
        fs::symlink_metadata(&path)?.is_file() && hash(&path)? == tool.executable_sha256,
        "{} is damaged; remove only {} and rerun setup",
        tool.name,
        path.display()
    );
    Ok(path)
}

pub(super) fn install(home: &Path, tool: &Tool) -> Result<()> {
    install_with(home, tool, |download| {
        retry_download(download, || {
            process::run(
                home,
                Path::new("curl"),
                &[
                    "--fail",
                    "--location",
                    "--connect-timeout",
                    "15",
                    "--max-time",
                    "60",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--silent",
                    "--show-error",
                    &tool.url,
                    "--output",
                    download.to_str().context("non-UTF-8 tool path")?,
                ],
                65,
            )
            .map(|_| ())
        })
    })
}

// Retry the complete transfer, including partial transfers and TLS failures
// which curl's default retry policy excludes. Three bounded attempts, no nested
// curl retries, and never resume a partial file. Integrity errors are not retried.
fn retry_download(path: &Path, mut transfer: impl FnMut() -> Result<()>) -> Result<()> {
    for attempt in 1..=3 {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match transfer() {
            Ok(()) => return Ok(()),
            Err(error) if attempt == 3 => {
                return Err(error.context("pinned tool download failed after 3 attempts"));
            }
            Err(_) => eprintln!("Pinned tool transfer failed; retrying ({}/3)", attempt + 1),
        }
    }
    unreachable!()
}

fn install_with(
    home: &Path,
    tool: &Tool,
    transfer: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    if fs::symlink_metadata(path(home, tool)).is_ok() {
        verified(home, tool)?;
        return Ok(());
    }
    let directory = home.join("tools");
    if directory.exists() {
        ensure!(
            fs::symlink_metadata(&directory)?.is_dir(),
            "refusing linked tools directory"
        );
    } else {
        fs::create_dir(&directory)?;
    }
    let temporary = tempfile::tempdir_in(&directory)?;
    let download = temporary.path().join("download");
    transfer(&download)?;
    ensure!(
        hash(&download)? == tool.sha256,
        "{} download checksum mismatch; retry setup",
        tool.name
    );
    let executable = temporary.path().join("executable");
    if let Some(member) = &tool.archive_member {
        // Extract exactly one reviewed regular file, never an archive tree.
        let result = std::process::Command::new("tar")
            .args(["-xOzf"])
            .arg(&download)
            .arg(member)
            .stdout(fs::File::create(&executable)?)
            .stderr(std::process::Stdio::null())
            .status()?;
        ensure!(result.success(), "cannot extract pinned helm executable");
    } else {
        fs::copy(download, &executable)?;
    }
    ensure!(
        hash(&executable)? == tool.executable_sha256,
        "{} executable checksum mismatch",
        tool.name
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    }
    fs::hard_link(executable, path(home, tool))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_tls_transfer_failures_retry_from_an_empty_file_and_stop_at_three() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download");
        let mut attempts = 0;
        retry_download(&path, || {
            attempts += 1;
            assert!(!path.exists());
            fs::write(&path, b"partial")?;
            match attempts {
                1 => anyhow::bail!("curl exit 18"),
                2 => anyhow::bail!("curl exit 35"),
                _ => Ok(()),
            }
        })
        .unwrap();
        assert_eq!(attempts, 3);
        attempts = 0;
        assert!(
            retry_download(&path, || {
                attempts += 1;
                anyhow::bail!("offline")
            })
            .is_err()
        );
        assert_eq!(attempts, 3);
    }

    #[test]
    fn successful_transfer_with_wrong_checksum_is_never_installed_or_retried() {
        let home = tempfile::tempdir().unwrap();
        let tool = pins_for(crate::platform::MAC_ARM64).unwrap().remove(0);
        let mut transfers = 0;
        let error = install_with(home.path(), &tool, |path| {
            transfers += 1;
            fs::write(path, b"wrong bytes")?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        assert_eq!(transfers, 1);
        assert!(!path(home.path(), &tool).exists());
    }

    #[test]
    fn executable_hash_is_checked_even_after_a_valid_transfer() {
        let home = tempfile::tempdir().unwrap();
        let mut tool = pins_for(crate::platform::MAC_ARM64).unwrap().remove(0);
        tool.archive_member = None;
        tool.sha256 = format!("{:x}", Sha256::digest(b"fixture executable"));
        let error = install_with(home.path(), &tool, |path| {
            fs::write(path, b"fixture executable")?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("executable checksum mismatch"));
        assert!(!path(home.path(), &tool).exists());
        tool.executable_sha256.clone_from(&tool.sha256);
        install_with(home.path(), &tool, |path| {
            fs::write(path, b"fixture executable")?;
            Ok(())
        })
        .unwrap();
        install_with(home.path(), &tool, |_| {
            panic!("verified tool must not be downloaded again")
        })
        .unwrap();
        fs::write(path(home.path(), &tool), b"changed installed binary").unwrap();
        assert!(
            install_with(home.path(), &tool, |_| panic!(
                "damaged installed tool must be reported"
            ))
            .is_err()
        );
    }

    #[test]
    fn every_supported_target_has_distinct_valid_pins() {
        let mac = pins_for(crate::platform::MAC_ARM64).unwrap();
        let linux = pins_for(crate::platform::LINUX_AMD64).unwrap();
        for (mac, linux) in mac.iter().zip(&linux) {
            assert_eq!(mac.name, linux.name);
            assert_eq!(mac.version, linux.version);
            assert_ne!(mac.sha256, linux.sha256);
            assert_ne!(mac.url, linux.url);
        }
        assert!(pins_for("unknown").is_err());
    }
}
