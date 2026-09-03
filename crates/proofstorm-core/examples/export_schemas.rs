use std::{fs, path::PathBuf};

use proofstorm_core::{
    configuration_coverage_manifest, default_backend_registry, default_catalog, schema_documents,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v1alpha1");
    fs::create_dir_all(&output)?;
    for (name, schema) in schema_documents() {
        let mut bytes = serde_json::to_vec_pretty(&schema)?;
        bytes.push(b'\n');
        fs::write(output.join(name), bytes)?;
    }
    let coverage_output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../coverage/v1alpha1/configuration-coverage.json");
    fs::create_dir_all(
        coverage_output
            .parent()
            .expect("coverage output has a parent"),
    )?;
    let manifest = configuration_coverage_manifest(default_catalog(), default_backend_registry())?;
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(coverage_output, bytes)?;
    Ok(())
}
