//! Embedded package metadata. This path never reads installation state.
use std::collections::BTreeSet;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[must_use]
/// # Panics
/// Panics only if the checked-in controller or bootstrap-tool JSON is malformed.
pub fn describe() -> Value {
    let catalog = proofstorm_core::default_catalog();
    let mut images: BTreeSet<&str> = catalog
        .entries
        .iter()
        .map(|entry| entry.image.as_str())
        .collect();
    images.extend(proofstorm_kube::images::HELPER_IMAGES);
    let assets: Vec<_> = crate::http::WEB_ASSETS
        .iter()
        .map(|(name, mime, bytes)| {
            json!({
                "path": name, "content_type": mime, "size": bytes.len(),
                "sha256": format!("{:x}", Sha256::digest(bytes))
            })
        })
        .collect();
    json!({
        "format_version": 1, "version": env!("CARGO_PKG_VERSION"),
        "target": env!("PROOFSTORM_BUILD_TARGET"),
        "build_profile": env!("PROOFSTORM_BUILD_PROFILE"),
        "source_revision": env!("PROOFSTORM_BUILD_REVISION"),
        "source_sha256": env!("PROOFSTORM_BUILD_SOURCE_SHA256"),
        "runtime_contract_sha256": runtime_contract_sha256(),
        "controller": controller(),
        "bootstrap_tools": crate::platform::bootstrap_pins_for(crate::platform::target())
            .ok().map(|pins| serde_json::from_str::<Value>(pins).expect("checked bootstrap pins")),
        "web_assets": assets, "catalog": catalog, "workload_images": images,
        "tools": include_str!("../../../tools/versions.env"),
        "image_publication": include_str!("../../../release/ghcr.json")
    })
}

/// Never advertise the ARM controller as an AMD64 runtime. A platform-matching
/// published pin must be supplied before that platform can run installed setup.
pub(crate) fn controller() -> Value {
    let value: Value = serde_json::from_str(include_str!("../../../release/controller.json"))
        .expect("checked controller metadata");
    if crate::platform::container_platform().is_ok_and(|platform| value["platform"] == platform) {
        value
    } else {
        Value::Null
    }
}

#[must_use]
pub fn runtime_contract_sha256() -> String {
    format!(
        "{:x}",
        Sha256::digest(proofstorm_kube::release::contract().to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_covers_catalog_and_helpers_without_mutable_tags() {
        let metadata = describe();
        let images = metadata["workload_images"].as_array().unwrap();
        for image in images {
            let (_, digest) = image.as_str().unwrap().split_once("@sha256:").unwrap();
            assert_eq!(digest.len(), 64);
        }
        for helper in proofstorm_kube::images::HELPER_IMAGES {
            assert!(images.contains(&json!(helper)));
        }
        assert_eq!(metadata["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata["bootstrap_tools"]["target"], metadata["target"]);
    }
}
