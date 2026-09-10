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
    controller_for(crate::platform::target())
}

fn controller_for(target: &str) -> Value {
    let explicit = include_str!(concat!(env!("OUT_DIR"), "/controller_receipt.json"));
    controller_with_receipt(target, explicit)
}

fn controller_with_receipt(target: &str, explicit: &str) -> Value {
    let fallback = match target {
        crate::platform::MAC_ARM64 => include_str!("../../../release/controller.json"),
        crate::platform::LINUX_AMD64 => {
            include_str!("../../../release/controller-linux-amd64.json")
        }
        _ => return Value::Null,
    };
    let encoded = if explicit.trim() == "null" {
        fallback
    } else {
        explicit
    };
    let value: Value = serde_json::from_str(encoded).expect("checked controller metadata");
    if crate::platform::container_arch_for(target)
        .is_ok_and(|arch| value["platform"] == format!("linux/{arch}"))
    {
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
    fn ci_controller_receipt_is_used_only_for_its_matching_platform() {
        for (target, other, platform) in [
            (
                crate::platform::LINUX_AMD64,
                crate::platform::MAC_ARM64,
                "linux/amd64",
            ),
            (
                crate::platform::MAC_ARM64,
                crate::platform::LINUX_AMD64,
                "linux/arm64",
            ),
        ] {
            let value = json!({"platform":platform,"image":"fixture-image"});
            assert_eq!(controller_with_receipt(target, &value.to_string()), value);
            // An explicit incompatible receipt must not silently fall back to an old pin.
            assert!(controller_with_receipt(other, &value.to_string()).is_null());
        }
    }

    #[test]
    fn controller_pins_are_platform_specific_and_preserve_the_mac_pin() {
        let mac: Value =
            serde_json::from_str(include_str!("../../../release/controller.json")).unwrap();
        assert_eq!(controller_for(crate::platform::MAC_ARM64), mac);
        let linux = controller_for(crate::platform::LINUX_AMD64);
        assert_eq!(linux["platform"], "linux/amd64");
        assert_eq!(linux["anonymous_verified"], true);
        assert!(linux["image"].as_str().unwrap().contains("@sha256:"));
        assert!(controller_for("unsupported").is_null());
    }

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
