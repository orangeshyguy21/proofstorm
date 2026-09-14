use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Managed {
    pub prefix: PathBuf,
    pub root: PathBuf,
    pub bundle: PathBuf,
    pub id: String,
    pub version: String,
    pub channel: String,
}

pub fn json(path: &Path, limit: u64) -> Result<Value> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= limit,
        "invalid metadata file: {}",
        path.display()
    );
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

impl Managed {
    pub fn resolve(executable: &Path, embedded: &Value) -> Result<Self> {
        let executable = executable.canonicalize()?;
        let bundle = executable
            .parent()
            .and_then(Path::parent)
            .context("unmanaged executable; install from https://proofstorm.com/install")?
            .to_owned();
        ensure!(
            executable.file_name().is_some_and(|s| s == "proofstorm")
                && executable
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|s| s == "bin")
                && bundle
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|s| s == "versions"),
            "self-update requires an installed release; for a development checkout use just dev-build, or install from https://proofstorm.com/install"
        );
        let id = bundle
            .file_name()
            .and_then(|s| s.to_str())
            .context("invalid bundle ID")?
            .to_owned();
        ensure!(super::schema::digest(&id), "invalid managed bundle ID");
        let root = bundle
            .parent()
            .and_then(Path::parent)
            .context("invalid managed root")?
            .to_owned();
        let owner = json(&root.join("install.json"), 65536)?;
        let prefix = PathBuf::from(
            owner["prefix"]
                .as_str()
                .context("installation prefix missing")?,
        );
        ensure!(
            prefix.is_absolute()
                && prefix.canonicalize()? == prefix
                && prefix.join("lib/proofstorm") == root
                && owner == serde_json::json!({"format_version":1,"prefix":prefix}),
            "installation owner does not match executable location"
        );
        ensure!(
            crate::artifacts::hash(&bundle.join("manifest.json"))? == id,
            "installed manifest identity changed; recover using the official installer"
        );
        let manifest = json(&bundle.join("manifest.json"), 2 * 1024 * 1024)?;
        let info = json(&bundle.join("release-info.json"), 2 * 1024 * 1024)?;
        ensure!(
            info == *embedded
                && manifest["version"] == info["version"]
                && manifest["target"] == info["target"],
            "installed release metadata differs from the running binary"
        );
        let version = manifest["version"]
            .as_str()
            .context("installed version missing")?
            .to_owned();
        semver::Version::parse(&version)?;
        let channel = manifest["channel"]
            .as_str()
            .context("installed channel missing")?
            .to_owned();
        ensure!(
            matches!(channel.as_str(), "alpha" | "release"),
            "development bundles are not self-updatable; rebuild the checkout"
        );
        let result = Self {
            prefix,
            root,
            bundle,
            id,
            version,
            channel,
        };
        ensure!(
            result.active()?.0 == result.id,
            "this bundle is inactive; run {} update",
            result.prefix.join("bin/proofstorm").display()
        );
        Ok(result)
    }

    pub fn active(&self) -> Result<(String, PathBuf)> {
        let target = fs::read_link(self.root.join("current"))?;
        let parts: Vec<_> = target.components().collect();
        ensure!(
            parts.len() == 2 && parts[0].as_os_str() == "versions",
            "unsafe active bundle pointer"
        );
        let id = parts[1].as_os_str().to_str().context("invalid bundle ID")?;
        ensure!(super::schema::digest(id), "invalid active bundle ID");
        let bundle = self.root.join(&target);
        ensure!(
            fs::symlink_metadata(&bundle)?.is_dir()
                && crate::artifacts::hash(&bundle.join("manifest.json"))? == id,
            "active bundle identity is invalid"
        );
        Ok((id.into(), bundle))
    }

    pub async fn verify(&self, bundle: &Path) -> Result<Value> {
        crate::installer::verify(bundle, false, false)?;
        crate::installer::check_launchers(&self.root, &self.prefix, true)?;
        let expected = json(&bundle.join("release-info.json"), 2 * 1024 * 1024)?;
        ensure!(
            super::process::metadata(&bundle.join("bin/proofstorm"), false).await? == expected
                && super::process::metadata(&bundle.join("bin/proofstorm-mcp"), true).await?
                    == expected,
            "CLI/MCP metadata verification failed"
        );
        Ok(expected)
    }
}
