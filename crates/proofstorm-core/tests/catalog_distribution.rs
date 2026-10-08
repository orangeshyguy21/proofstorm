//! Distributed Bark pins must retain their verified native publication lineage.
use proofstorm_core::{
    CatalogPlatform, PaymentMethod, SupportLifecycle, catalog_for_platform, catalog_image_source,
    distributed_catalog_for_platform,
    processor_ids::{BARK_PROCESSOR, BARK_SERVER, CLN_HOLD},
};

#[test]
fn both_catalogs_distribute_verified_bark_images_with_experimental_contracts() {
    let publication: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docker/payment/bark-publication.json"
    ))
    .unwrap();
    let images = publication["images"].as_array().unwrap();
    assert_eq!(images.len(), 6);
    for (platform, name) in [
        (CatalogPlatform::LinuxArm64, "linux/arm64"),
        (CatalogPlatform::LinuxAmd64, "linux/amd64"),
    ] {
        let runtime = catalog_for_platform(platform);
        let distributed = distributed_catalog_for_platform(platform);
        assert_eq!(runtime, distributed);
        for id in [BARK_PROCESSOR, BARK_SERVER, CLN_HOLD] {
            let entry = distributed
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap();
            let receipts: Vec<_> = images
                .iter()
                .filter(|image| image["repository"] == id && image["platform"] == name)
                .collect();
            assert_eq!(receipts.len(), 1);
            let receipt = receipts[0];
            assert_eq!(receipt["version"], entry.version);
            assert_eq!(
                receipt["image"],
                catalog_image_source(&entry.image).unwrap()
            );
            assert_eq!(entry.build_provenance.as_ref().unwrap().platform, name);
            for check in [
                "config_and_rootfs_match_qualification",
                "ghcr_manifest_readback_verified",
                "anonymous_registry_verified",
                "anonymous_docker_pull_verified",
            ] {
                assert_eq!(receipt[check], true, "{id} {name} {check}");
            }
            assert_eq!(entry.support_lifecycle, SupportLifecycle::Experimental);
        }
        let mint = distributed
            .entries
            .iter()
            .find(|entry| entry.id == "cdk" && entry.version == "0.18.1")
            .unwrap();
        let bindings: Vec<_> = mint
            .support_matrix
            .payment_bindings
            .iter()
            .filter(|binding| binding.backend.implementation == BARK_PROCESSOR)
            .collect();
        // CDK registers every method the processor can advertise.
        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.method.clone())
                .collect::<Vec<_>>(),
            [
                PaymentMethod::Bolt11,
                PaymentMethod::Onchain,
                PaymentMethod::Custom("arkoor".into())
            ]
        );
        for binding in bindings {
            assert_eq!(binding.unit, "sat");
            assert_eq!(binding.backend.versions, ["0.1.0-fe468ca".into()].into());
        }
    }
}
