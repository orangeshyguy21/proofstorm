use std::{fs, path::PathBuf};

use proofstorm_core::schema_documents;

#[test]
fn checked_in_schemas_match_the_typed_contracts() {
    let schema_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v1alpha1");
    for (name, generated) in schema_documents() {
        let path = schema_dir.join(name);
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let checked_in: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        assert_eq!(checked_in, generated, "schema drift in {}", path.display());
    }
}
