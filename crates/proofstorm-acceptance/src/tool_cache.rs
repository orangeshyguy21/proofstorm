//! Explicit immutable host-tool reuse for disposable installations. Setup still
//! checks its own pins; no source installation state or ownership is copied.
use anyhow::{Result, ensure};
use proofstorm_core::tool_pins::{Pins, Tool};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

pub(crate) fn seed(cache: &Path, work: &Path) -> Result<()> {
    let target = proofstorm_app::platform::target();
    let pins = Pins::parse(
        target,
        proofstorm_app::platform::bootstrap_pins_for(target)?,
    )
    .map_err(anyhow::Error::msg)?;
    let receipt = copy_verified(cache, &work.join("state"), &pins.tools)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(work.join("bootstrap-tool-cache.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    Ok(())
}

fn copy_verified(cache: &Path, home: &Path, pins: &[Tool]) -> Result<BTreeMap<String, String>> {
    ensure!(
        fs::symlink_metadata(cache)?.is_dir(),
        "linked tool cache refused"
    );
    let mut verified = Vec::new();
    for tool in pins {
        let name = format!("{}-{}", tool.name, tool.executable_sha256);
        let source = cache.join(&name);
        let metadata = fs::symlink_metadata(&source)?;
        ensure!(
            metadata.is_file() && metadata.len() <= 256 * 1024 * 1024,
            "invalid cached tool file"
        );
        let bytes = fs::read(source)?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == tool.executable_sha256,
            "cached {} checksum differs",
            tool.name
        );
        verified.push((name, bytes));
    }
    // Verify the entire selection before writing anything. Never adopt an
    // existing home, follow a destination symlink or hard-link mutable tools.
    fs::DirBuilder::new().mode(0o700).create(home)?;
    let destination = home.join("tools");
    fs::DirBuilder::new().mode(0o700).create(&destination)?;
    for (name, bytes) in verified {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(destination.join(name))?;
        file.write_all(&bytes)?;
    }
    Ok(pins
        .iter()
        .map(|tool| (tool.name.clone(), tool.executable_sha256.clone()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn only_complete_verified_regular_files_are_copied_into_a_new_home() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let bytes = b"pinned executable";
        let tool = Tool {
            name: "kubectl".into(),
            version: "test".into(),
            url: String::new(),
            sha256: String::new(),
            executable_sha256: format!("{:x}", Sha256::digest(bytes)),
            archive_member: None,
        };
        let pins = [tool];
        let name = format!("kubectl-{}", pins[0].executable_sha256);
        let source = cache.join(&name);
        let home = root.path().join("home");
        assert!(copy_verified(&cache, &home, &pins).is_err());
        assert!(!home.exists());
        fs::write(&source, "corrupted").unwrap();
        assert!(copy_verified(&cache, &home, &pins).is_err());
        assert!(!home.exists());
        fs::remove_file(&source).unwrap();
        let other = root.path().join("other");
        fs::write(&other, bytes).unwrap();
        symlink(&other, &source).unwrap();
        assert!(copy_verified(&cache, &home, &pins).is_err());
        assert!(!home.exists());
        fs::remove_file(&source).unwrap();
        fs::write(&source, bytes).unwrap();
        let incomplete = [
            pins[0].clone(),
            Tool {
                name: "helm".into(),
                ..pins[0].clone()
            },
        ];
        assert!(copy_verified(&cache, &home, &incomplete).is_err());
        assert!(!home.exists());
        copy_verified(&cache, &home, &pins).unwrap();
        let copied = home.join("tools").join(name);
        assert_eq!(fs::read(&copied).unwrap(), bytes);
        assert_ne!(
            fs::metadata(&source).unwrap().ino(),
            fs::metadata(&copied).unwrap().ino()
        );
        assert!(copy_verified(&cache, &home, &pins).is_err());
        let link = root.path().join("cache-link");
        symlink(cache, &link).unwrap();
        assert!(copy_verified(&link, &root.path().join("new"), &pins).is_err());
    }
}
