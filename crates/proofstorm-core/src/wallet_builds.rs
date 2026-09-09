//! Published per-architecture wallet builds. Browser WASM consumes the server's
//! catalog; it is not evidence of the server's container architecture.
pub(crate) const LINUX_AMD64: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));

pub(crate) fn cdk(amd64: bool) -> (&'static str, &'static str) {
    if amd64 {
        (
            "proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:a02dfc4f849d011737a8a778aecfa8d159c1b9f4c716f8f869590dc810573136",
            include_str!("../../../docker/wallet/cdk-cli-0.18.0-linux-amd64-provenance.json"),
        )
    } else {
        (
            "proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:bc4ec6943eb505bb7eb5a6d43ddebf0297fe00f70775378e33ae85c26eb6a5a8",
            include_str!("../../../docker/wallet/cdk-cli-0.18.0-provenance.json"),
        )
    }
}

pub(crate) fn cocod(amd64: bool) -> (&'static str, &'static str) {
    if amd64 {
        (
            "proofstorm-registry.localhost:5000/cocod-wallet@sha256:7e313c8f020e3f8eb4989e00006516cac325eb29f64ddf2ca7d1996a68203114",
            include_str!("../../../docker/wallet/cocod-44e5101c-linux-amd64-provenance.json"),
        )
    } else {
        (
            "proofstorm-registry.localhost:5000/cocod-wallet@sha256:88dc907f64530788280b0ba603b1bd7f361c58281171e74ca25b0676fadfcdc7",
            include_str!("../../../docker/wallet/cocod-44e5101c-provenance.json"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_wallets_have_distinct_pins_and_matching_provenance() {
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
