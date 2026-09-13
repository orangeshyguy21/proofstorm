//! Per-architecture component builds. Browser WASM consumes the server's
//! catalog; it is not evidence of the server's container architecture.
pub(crate) const LINUX_AMD64: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));

pub(crate) fn cdk(amd64: bool) -> (&'static str, &'static str) {
    if amd64 {
        (
            "proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:c7b68212a7af2d35bd9fceb0949d2a3f6929f6796e22d70bdee01bd1c9f06cff",
            include_str!("../../../docker/wallet/cdk-cli-0.18.0-linux-amd64-provenance.json"),
        )
    } else {
        (
            "proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:9f8b536f138897e5f423b4b1648db6042479aec2aba034eed0646d23ba481f42",
            include_str!("../../../docker/wallet/cdk-cli-0.18.0-provenance.json"),
        )
    }
}

pub(crate) fn cocod(amd64: bool) -> (&'static str, &'static str) {
    if amd64 {
        (
            "proofstorm-registry.localhost:5000/cocod-wallet@sha256:859c6f1daf68e0745b5502d4eeddd330823ad8b89aa2c7cf9d901e63caabc00b",
            include_str!("../../../docker/wallet/cocod-44e5101c-linux-amd64-provenance.json"),
        )
    } else {
        (
            "proofstorm-registry.localhost:5000/cocod-wallet@sha256:35f73767b4721019b0554816f53c400b3534bc20aaa6fbffb64495f2df394b8b",
            include_str!("../../../docker/wallet/cocod-44e5101c-provenance.json"),
        )
    }
}

pub(crate) fn nutshell(amd64: bool) -> &'static str {
    if amd64 {
        "proofstorm-registry.localhost:5000/nutshell-mint-management@sha256:c3e4deaf8a9e101ee7bc6ea07d5b8ded4c501e6515fa8263f2e38130a0ee69d7"
    } else {
        "proofstorm-registry.localhost:5000/nutshell-mint-management@sha256:e5fc04eff1956c261bed827fbb10eda77a4f39e80afb641ad8f2ac41eb4ed382"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_builds_have_distinct_pins_and_matching_provenance() {
        assert_ne!(nutshell(true), nutshell(false));
        for builds in [cdk, cocod] {
            assert_ne!(builds(true).0, builds(false).0);
            for amd64 in [false, true] {
                let (image, encoded) = builds(amd64);
                let provenance: crate::BuildProvenance = serde_json::from_str(encoded).unwrap();
                assert_eq!(
                    provenance.platform,
                    if amd64 { "linux/amd64" } else { "linux/arm64" }
                );
                assert_eq!(image.split_once("@sha256:").unwrap().1.len(), 64);
            }
        }
        let cdk_amd64: crate::BuildProvenance = serde_json::from_str(cdk(true).1).unwrap();
        assert!(cdk_amd64.artifact_url.ends_with("-x86_64"));
    }
}
