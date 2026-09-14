//! Consumer contract for the website feed (not the release asset manifest).
use anyhow::{Result, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};

pub const ENDPOINT: &str = "https://proofstorm.com/release.json";
pub const REPOSITORY: &str = "orangeshyguy21/proofstorm";
pub const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Platform {
    pub id: String,
    pub archive: Asset,
    pub checksum: Asset,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "field names follow the public website schema"
)]
pub struct Release {
    pub schema_version: u32,
    pub repository: String,
    pub release_id: u64,
    pub version: String,
    pub tag: String,
    pub channel: String,
    pub published_at: String,
    pub installer: Asset,
    pub platforms: Vec<Platform>,
}

pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Asset {
    fn validate(&self, tag: &str, name: &str, maximum: u64) -> Result<()> {
        ensure!(
            self.name == name
                && self.url
                    == format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}"),
            "asset URL/name does not match the selected repository and tag"
        );
        ensure!(
            self.bytes > 0 && self.bytes <= maximum && digest(&self.sha256),
            "invalid asset size or SHA-256"
        );
        Ok(())
    }
}

impl Release {
    pub fn selected(&self, platform: &str, channel: &str) -> Result<&Platform> {
        ensure!(
            self.schema_version == 1,
            "unsupported website release schema"
        );
        ensure!(
            self.repository == REPOSITORY && self.release_id > 0,
            "untrusted release identity"
        );
        let version = Version::parse(&self.version)?;
        ensure!(
            self.version == version.to_string()
                && self.tag == format!("v{version}")
                && version.build.is_empty(),
            "release tag and version disagree"
        );
        ensure!(
            self.channel == channel,
            "release channel differs from this installation; channel switching is not supported"
        );
        ensure!(
            (channel == "alpha"
                && version
                    .pre
                    .as_str()
                    .strip_prefix("alpha.")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())))
                || (channel == "release" && version.pre.is_empty()),
            "version does not match its release channel"
        );
        ensure!(
            !self.published_at.is_empty() && self.published_at.len() <= 64,
            "invalid publication time"
        );
        self.installer
            .validate(&self.tag, "install.sh", 1024 * 1024)?;
        let mut ids = std::collections::BTreeSet::new();
        ensure!(
            !self.platforms.is_empty() && self.platforms.len() <= 16,
            "invalid platform inventory"
        );
        for item in &self.platforms {
            ensure!(ids.insert(&item.id), "duplicate platform ID");
            ensure!(
                matches!(item.id.as_str(), "linux-amd64" | "macos-arm64"),
                "unsupported platform in schema v1"
            );
            let name = format!("proofstorm-{version}-{}.tar.gz", item.id);
            item.archive.validate(&self.tag, &name, MAX_ARCHIVE)?;
            item.checksum
                .validate(&self.tag, &format!("{name}.sha256"), 4096)?;
        }
        self.platforms
            .iter()
            .find(|item| item.id == platform)
            .ok_or_else(|| anyhow::anyhow!("release does not support this platform"))
    }
}

pub fn platform() -> Result<&'static str> {
    match crate::platform::target() {
        crate::platform::MAC_ARM64 => Ok("macos-arm64"),
        crate::platform::LINUX_AMD64 => Ok("linux-amd64"),
        _ => anyhow::bail!("updates support Linux AMD64 and macOS ARM64"),
    }
}
