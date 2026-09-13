//! Reviewed host-tool download contracts, shared by setup and maintainer tooling.
//! No downloads, filesystem access, or host inference belong in this module.
use serde::{Deserialize, Serialize};

pub const MAC_ARM64: &str = "aarch64-apple-darwin";
pub const LINUX_AMD64: &str = "x86_64-unknown-linux-gnu";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub executable_sha256: String,
    pub archive_member: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub format_version: u32,
    pub target: String,
    pub tools: Vec<Tool>,
}

pub struct Source {
    pub url: String,
    pub checksum_url: String,
    pub checksum_name: Option<String>,
    pub archive_member: Option<String>,
}

/// # Errors
/// Rejects targets without a reviewed host-tool contract.
pub fn host_parts(target: &str) -> Result<(&'static str, &'static str), String> {
    match target {
        MAC_ARM64 => Ok(("darwin", "arm64")),
        LINUX_AMD64 => Ok(("linux", "amd64")),
        _ => Err("unsupported host-tool target".into()),
    }
}

/// # Errors
/// Rejects unknown tools/targets and unsafe version strings.
pub fn source(name: &str, version: &str, target: &str) -> Result<Source, String> {
    let (os, arch) = host_parts(target)?;
    if !version.starts_with('v')
        || version.len() < 2
        || version.len() > 64
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
    {
        return Err("invalid tool version".into());
    }
    let (url, checksum_url, checksum_name, archive_member) = match name {
        "k3d" => {
            let base = format!("https://github.com/k3d-io/k3d/releases/download/{version}");
            (
                format!("{base}/k3d-{os}-{arch}"),
                format!("{base}/checksums.txt"),
                Some(format!("_dist/k3d-{os}-{arch}")),
                None,
            )
        }
        "kubectl" => {
            let url = format!("https://dl.k8s.io/release/{version}/bin/{os}/{arch}/kubectl");
            (url.clone(), format!("{url}.sha256"), None, None)
        }
        "helm" => {
            let name = format!("helm-{version}-{os}-{arch}.tar.gz");
            let url = format!("https://get.helm.sh/{name}");
            (
                url.clone(),
                format!("{url}.sha256sum"),
                Some(name),
                Some(format!("{os}-{arch}/helm")),
            )
        }
        _ => return Err("unsupported host tool".into()),
    };
    Ok(Source {
        url,
        checksum_url,
        checksum_name,
        archive_member,
    })
}

#[must_use]
pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Pins {
    /// # Errors
    /// Rejects malformed JSON or a manifest that fails the shared pin contract.
    pub fn parse(target: &str, encoded: &str) -> Result<Self, String> {
        let pins: Self = serde_json::from_str(encoded).map_err(|_| "invalid host-tool manifest")?;
        pins.validate(target)?;
        Ok(pins)
    }

    /// # Errors
    /// Rejects missing tools, foreign sources, target mismatches, or invalid hashes.
    pub fn validate(&self, target: &str) -> Result<(), String> {
        host_parts(target)?;
        if self.format_version != 1
            || self.target != target
            || self
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                != ["k3d", "kubectl", "helm"]
        {
            return Err("incomplete or mismatched host-tool manifest".into());
        }
        for tool in &self.tools {
            let expected = source(&tool.name, &tool.version, target)?;
            if tool.url != expected.url
                || tool.archive_member != expected.archive_member
                || !digest(&tool.sha256)
                || !digest(&tool.executable_sha256)
                || (tool.archive_member.is_none() && tool.sha256 != tool.executable_sha256)
            {
                return Err(format!(
                    "invalid {} download, target, or checksum pin",
                    tool.name
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_reviewed_manifests_validate_without_changing_their_bytes() {
        for (target, encoded) in [
            (
                MAC_ARM64,
                include_str!("../../../release/bootstrap-tools.json"),
            ),
            (
                LINUX_AMD64,
                include_str!("../../../release/bootstrap-tools-linux-amd64.json"),
            ),
        ] {
            Pins::parse(target, encoded).unwrap();
            for (path, value) in [
                ("/target", json!("unknown")),
                ("/tools/0/url", json!("https://unapproved.invalid/k3d")),
                ("/tools/1/executable_sha256", json!("a".repeat(64))),
                ("/tools/2/archive_member", json!("../../helm")),
                ("/tools/0/sha256", json!("A".repeat(64))),
            ] {
                let mut changed: serde_json::Value = serde_json::from_str(encoded).unwrap();
                *changed.pointer_mut(path).unwrap() = value;
                assert!(Pins::parse(target, &changed.to_string()).is_err(), "{path}");
            }
            let mut changed: serde_json::Value = serde_json::from_str(encoded).unwrap();
            changed["tools"].as_array_mut().unwrap().remove(0);
            assert!(Pins::parse(target, &changed.to_string()).is_err());
        }
        assert!(source("helm", "v1/../../bad", MAC_ARM64).is_err());
        assert!(source("helm", "v4.2.3", "unknown").is_err());
    }
}
