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
    process::run(
        home,
        Path::new("curl"),
        &[
            "--fail",
            "--location",
            "--retry",
            "3",
            "--max-time",
            "180",
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
        200,
    )?;
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
