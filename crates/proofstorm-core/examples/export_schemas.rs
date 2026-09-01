use std::{fs, path::PathBuf};

use proofstorm_core::schema_documents;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v1alpha1");
    fs::create_dir_all(&output)?;
    for (name, schema) in schema_documents() {
        let mut bytes = serde_json::to_vec_pretty(&schema)?;
        bytes.push(b'\n');
        fs::write(output.join(name), bytes)?;
    }
    Ok(())
}
