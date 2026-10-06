//! Local preview contracts must not become ordinary distribution obligations.
use proofstorm_core::{
    CatalogPlatform, SupportLifecycle, catalog_for_platform, distributed_catalog_for_platform,
    processor_ids::{BARK_PROCESSOR, BARK_SERVER, CLN_HOLD},
};

#[test]
fn distribution_excludes_preview_entries_and_bindings_but_keeps_shipped_experiments() {
    let previews = [BARK_PROCESSOR, BARK_SERVER, CLN_HOLD];
    for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
        let runtime = catalog_for_platform(platform);
        let distributed = distributed_catalog_for_platform(platform);
        for entry in &distributed.entries {
            assert!(!previews.contains(&entry.id.as_str()));
            assert!(
                entry
                    .compatible_dependencies
                    .iter()
                    .all(|dependency| !previews.contains(&dependency.implementation.as_str()))
            );
            assert!(
                entry
                    .support_matrix
                    .payment_backends
                    .iter()
                    .all(|backend| !previews.contains(&backend.as_str()))
            );
            assert!(
                entry
                    .support_matrix
                    .payment_bindings
                    .iter()
                    .all(|binding| !previews.contains(&binding.backend.implementation.as_str()))
            );
            let live = runtime
                .entries
                .iter()
                .find(|live| live.id == entry.id && live.version == entry.version)
                .unwrap();
            // The preview only extends CDK's contract; it never substitutes published images.
            assert_eq!(entry.image, live.image);
            if platform == CatalogPlatform::LinuxAmd64 || entry.id != "cdk" {
                assert_eq!(entry, live);
            }
        }
        for id in ["ldk-server", "cdk-ldk-server-processor"] {
            assert!(
                distributed.entries.iter().any(|entry| entry.id == id
                    && entry.support_lifecycle == SupportLifecycle::Experimental)
            );
        }
        for id in previews {
            assert_eq!(
                runtime.entries.iter().any(|entry| entry.id == id),
                platform == CatalogPlatform::LinuxArm64
            );
        }
    }
}
