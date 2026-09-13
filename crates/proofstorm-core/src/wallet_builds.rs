//! Per-architecture component builds. Browser WASM consumes the server's
//! catalog; it is not evidence of the server's container architecture.
pub(crate) const LINUX_AMD64: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));

pub(crate) fn cdk(amd64: bool) -> (&'static str, &'static str) {
    if amd64 {
        (
            "proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:613eab7ee578ad733ccd062830190a0905f3192dec1d976bfb72007382f59bff",
            include_str!("../../../docker/wallet/cdk-cli-0.18.0-linux-amd64-provenance.json"),
        )
    } else {
        (
            "proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:31e18a419bb3cafcc795a5c99dcfbb07460bba5ad76f2c1d104341e151cb22e9",
            include_str!("../../../docker/wallet/cdk-cli-0.18.0-provenance.json"),
        )
    }
}

pub(crate) fn cocod(amd64: bool) -> (&'static str, &'static str) {
    if amd64 {
        (
            "proofstorm-registry.localhost:5000/cocod-wallet@sha256:4c6dfadc79c08a2c2d535859ee750042a28e162c86c2cea788f2b50f7ce39187",
            include_str!("../../../docker/wallet/cocod-44e5101c-linux-amd64-provenance.json"),
        )
    } else {
        (
            "proofstorm-registry.localhost:5000/cocod-wallet@sha256:940d02390f50a7009d5ad50b343663afc042f7af93fdb94ddceec34d1940d29c",
            include_str!("../../../docker/wallet/cocod-44e5101c-provenance.json"),
        )
    }
}

pub(crate) fn nutshell(amd64: bool) -> &'static str {
    if amd64 {
        "proofstorm-registry.localhost:5000/nutshell-mint-management@sha256:7981248be39e217c66790bd2825f157ec9ddf460410489715ca89d83952aa754"
    } else {
        "proofstorm-registry.localhost:5000/nutshell-mint-management@sha256:ce57c9623d564201e37e7a4515b111e07e73e0d74c3e4ec7f97df9fccce47c3f"
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
