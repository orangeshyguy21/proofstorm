use std::{error::Error, path::Path};
fn main() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/v1alpha1/environment.schema.json");
    let schema = schemars::schema_for!(proofstorm_app::environment::EnvironmentView);
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&schema)?),
    )?;
    Ok(())
}
