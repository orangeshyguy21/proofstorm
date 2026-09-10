use std::{fs, path::PathBuf};

use proofstorm_core::{
    CatalogPlatform, ConfigurationCoverageManifest, catalog_for_platform,
    configuration_coverage_manifest, default_backend_registry, default_catalog,
};

#[test]
fn checked_in_coverage_matches_catalog_and_backend_contracts() {
    for (platform, name) in [
        (CatalogPlatform::LinuxArm64, "configuration-coverage.json"),
        (
            CatalogPlatform::LinuxAmd64,
            "configuration-coverage-linux-amd64.json",
        ),
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../coverage/v1alpha1")
            .join(name);
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let checked_in: ConfigurationCoverageManifest = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let generated = configuration_coverage_manifest(
            &catalog_for_platform(platform),
            default_backend_registry(),
        )
        .expect("generate coverage manifest");
        // Report digest drift before dumping every entry into the CI log.
        assert_eq!(
            checked_in.catalog_digest,
            generated.catalog_digest,
            "catalog digest drift for {platform:?} in {}; regenerate with cargo run -p proofstorm-core --example export_schemas",
            path.display()
        );
        assert_eq!(
            checked_in,
            generated,
            "coverage drift for {platform:?} in {}",
            path.display()
        );
    }
}

#[test]
fn default_catalog_uses_the_build_hosts_platform_contract() {
    let platform = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        CatalogPlatform::LinuxAmd64
    } else {
        CatalogPlatform::LinuxArm64
    };
    assert_eq!(*default_catalog(), catalog_for_platform(platform));
}

#[test]
fn only_platform_specific_wallet_builds_differ_between_catalogs() {
    let arm = catalog_for_platform(CatalogPlatform::LinuxArm64);
    let amd = catalog_for_platform(CatalogPlatform::LinuxAmd64);
    assert_eq!(arm.entries.len(), amd.entries.len());
    for (arm_entry, amd_entry) in arm.entries.iter().zip(&amd.entries) {
        assert_eq!(arm_entry.id, amd_entry.id);
        if matches!(arm_entry.id.as_str(), "cdk-cli-wallet" | "cocod-wallet") {
            assert_ne!(arm_entry.image, amd_entry.image);
            assert_ne!(arm_entry.source_digest, amd_entry.source_digest);
            assert_eq!(
                arm_entry.build_provenance.as_ref().unwrap().platform,
                "linux/arm64"
            );
            assert_eq!(
                amd_entry.build_provenance.as_ref().unwrap().platform,
                "linux/amd64"
            );
            assert_eq!(
                arm_entry.config_schema_digest,
                amd_entry.config_schema_digest
            );
            assert_eq!(arm_entry.support_matrix, amd_entry.support_matrix);
            if arm_entry.id == "cdk-cli-wallet" {
                let arm_notes = &arm_entry.runtime_endpoints[0].limitations;
                let amd_notes = &amd_entry.runtime_endpoints[0].limitations;
                assert!(
                    arm_notes
                        .iter()
                        .any(|note| note.contains("Initial image is Linux arm64 only."))
                );
                assert!(
                    amd_notes
                        .iter()
                        .any(|note| note.contains("Packaged image is Linux amd64."))
                );
                assert!(
                    !amd_notes
                        .iter()
                        .any(|note| note.contains("Initial image is Linux arm64 only."))
                );
            }
        } else {
            assert_eq!(
                arm_entry, amd_entry,
                "unexpected platform difference in {}",
                arm_entry.id
            );
        }
    }
}
