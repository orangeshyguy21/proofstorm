use std::{fs, path::PathBuf};

use proofstorm_core::{
    ConfigurationCoverageManifest, configuration_coverage_manifest, default_backend_registry,
    default_catalog,
};

#[test]
fn checked_in_coverage_matches_catalog_and_backend_contracts() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../coverage/v1alpha1/configuration-coverage.json");
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let checked_in: ConfigurationCoverageManifest = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    let generated =
        configuration_coverage_manifest(&default_catalog(), &default_backend_registry())
            .expect("generate coverage manifest");
    assert_eq!(
        checked_in,
        generated,
        "coverage drift in {}",
        path.display()
    );
}
