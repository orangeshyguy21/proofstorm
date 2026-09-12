//! Host and container targets supported by the alpha. Never infer one from the other.
use anyhow::{Result, bail};

pub use proofstorm_core::tool_pins::{LINUX_AMD64, MAC_ARM64};

#[must_use]
pub fn target() -> &'static str {
    env!("PROOFSTORM_BUILD_TARGET")
}

pub fn bootstrap_pins_for(target: &str) -> Result<&'static str> {
    match target {
        MAC_ARM64 => Ok(include_str!("../../../release/bootstrap-tools.json")),
        LINUX_AMD64 => Ok(include_str!(
            "../../../release/bootstrap-tools-linux-amd64.json"
        )),
        _ => bail!("no bootstrap tools for this host"),
    }
}

pub fn container_arch_for(target: &str) -> Result<&'static str> {
    match target {
        MAC_ARM64 => Ok("arm64"),
        LINUX_AMD64 => Ok("amd64"),
        _ => bail!("this alpha supports macOS Apple Silicon and Linux x86-64"),
    }
}

pub fn container_arch() -> Result<&'static str> {
    container_arch_for(target())
}

pub fn container_platform() -> Result<String> {
    Ok(format!("linux/{}", container_arch()?))
}

#[must_use]
pub fn docker_matches(target: &str, os: &str, arch: &str) -> bool {
    os == "linux"
        && match container_arch_for(target) {
            Ok("arm64") => matches!(arch, "aarch64" | "arm64"),
            Ok("amd64") => matches!(arch, "x86_64" | "amd64"),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_hosts_require_matching_linux_containers() {
        for (host, aliases) in [
            (MAC_ARM64, ["arm64", "aarch64"]),
            (LINUX_AMD64, ["amd64", "x86_64"]),
        ] {
            for arch in aliases {
                assert!(docker_matches(host, "linux", arch));
                assert!(!docker_matches(host, "windows", arch));
            }
        }
        assert!(!docker_matches(LINUX_AMD64, "linux", "arm64"));
        assert!(!docker_matches(MAC_ARM64, "linux", "amd64"));
        assert!(container_arch_for("aarch64-unknown-linux-gnu").is_err());
        assert!(!docker_matches("unknown", "linux", "amd64"));
    }
}
