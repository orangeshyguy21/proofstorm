use std::{collections::BTreeSet, fs, path::PathBuf};

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
fn only_platform_specific_component_builds_differ_between_catalogs() {
    let arm = catalog_for_platform(CatalogPlatform::LinuxArm64);
    let amd = catalog_for_platform(CatalogPlatform::LinuxAmd64);
    let bark = ["bark-server", "cdk-bark-processor", "cln-hold"];
    assert_eq!(
        arm.entries
            .iter()
            .filter(|entry| bark.contains(&entry.id.as_str()))
            .map(|entry| entry.id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(bark),
    );
    assert!(
        amd.entries
            .iter()
            .all(|entry| !bark.contains(&entry.id.as_str()))
    );
    let shared = arm
        .entries
        .iter()
        .filter(|entry| !bark.contains(&entry.id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), amd.entries.len());
    for (arm_entry, amd_entry) in shared.into_iter().zip(&amd.entries) {
        assert_eq!(arm_entry.id, amd_entry.id);
        if matches!(
            arm_entry.id.as_str(),
            "cdk-cli-wallet" | "cocod-wallet" | "ldk-server" | "cdk-ldk-server-processor"
        ) {
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
        } else if arm_entry.id == "cdk" && arm_entry.version == "0.18.1" {
            assert_bark_mint_preview(arm_entry, amd_entry);
        } else if matches!(arm_entry.id.as_str(), "nutshell" | "nutshell-wallet") {
            assert_ne!(arm_entry.image, amd_entry.image);
            assert_eq!(arm_entry.source_digest, amd_entry.source_digest);
            let mut normalized = amd_entry.clone();
            normalized.image.clone_from(&arm_entry.image);
            assert_eq!(
                arm_entry, &normalized,
                "only the packaged image identity may differ"
            );
        } else {
            assert_eq!(
                arm_entry, amd_entry,
                "unexpected platform difference in {}",
                arm_entry.id
            );
        }
    }
}

fn assert_bark_mint_preview(
    arm_entry: &proofstorm_core::CatalogEntry,
    amd_entry: &proofstorm_core::CatalogEntry,
) {
    // The sole extra mint binding is the explicit native ARM64 preview.
    let mut normalized = arm_entry.clone();
    let processor = "cdk-bark-processor";
    normalized
        .compatible_dependencies
        .retain(|dependency| dependency.implementation != processor);
    assert!(normalized.support_matrix.payment_backends.remove(processor));
    let removed = normalized
        .support_matrix
        .payment_bindings
        .iter()
        .filter(|binding| binding.backend.implementation == processor)
        .collect::<Vec<_>>();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].method, proofstorm_core::PaymentMethod::Bolt11);
    assert_eq!(removed[0].unit, "sat");
    normalized
        .support_matrix
        .payment_bindings
        .retain(|binding| binding.backend.implementation != processor);
    assert_eq!(
        arm_entry.source_digest,
        proofstorm_core::digest_json(&(
            &amd_entry.source_digest,
            &arm_entry.support_matrix,
            &arm_entry.compatible_dependencies,
        ))
    );
    normalized
        .source_digest
        .clone_from(&amd_entry.source_digest);
    assert_eq!(
        &normalized, amd_entry,
        "only the explicit Bark preview binding may differ"
    );
}
