use std::{fs, path::PathBuf};

use proofstorm_core::{
    CatalogPlatform, catalog_for_platform, configuration_coverage_manifest,
    default_backend_registry, schema_documents,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v1alpha1");
    fs::create_dir_all(&output)?;
    for (name, schema) in schema_documents() {
        let mut bytes = serde_json::to_vec_pretty(&schema)?;
        bytes.push(b'\n');
        fs::write(output.join(name), bytes)?;
    }
    let coverage_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../coverage/v1alpha1");
    fs::create_dir_all(&coverage_dir)?;
    for (platform, name) in [
        // Preserve the existing ARM64 document's path for its consumers.
        (CatalogPlatform::LinuxArm64, "configuration-coverage.json"),
        (
            CatalogPlatform::LinuxAmd64,
            "configuration-coverage-linux-amd64.json",
        ),
    ] {
        let catalog = catalog_for_platform(platform);
        let manifest = configuration_coverage_manifest(&catalog, default_backend_registry())?;
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        fs::write(coverage_dir.join(name), bytes)?;
    }
    Ok(())
}
