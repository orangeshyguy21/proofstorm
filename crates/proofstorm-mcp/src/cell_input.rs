//! Cell documents can stay in the workspace instead of travelling through model context.
use std::{fs::File, io::BufReader, path::Path};

use proofstorm_core::CellSpec;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AuthoredCellSpec, deserialize_authored_cell};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellFile {
    /// JSON path inside the MCP working directory (absolute or relative).
    /// The server reads it directly; do not paste its contents.
    pub file: String,
}

/// Inline cell, workspace JSON file, or JSON string; all use the same validation.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum CellInput {
    Inline(AuthoredCellSpec),
    File(CellFile),
    Json(String),
}

impl<'de> Deserialize<'de> for CellInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value
            .as_object()
            .is_some_and(|object| object.contains_key("file"))
        {
            return serde_json::from_value(value)
                .map(Self::File)
                .map_err(serde::de::Error::custom);
        }
        deserialize_authored_cell(value)
            .map(Self::Inline)
            .map_err(serde::de::Error::custom)
    }
}

impl From<AuthoredCellSpec> for CellInput {
    fn from(cell: AuthoredCellSpec) -> Self {
        Self::Inline(cell)
    }
}

impl TryFrom<CellInput> for CellSpec {
    type Error = String;

    fn try_from(input: CellInput) -> Result<Self, Self::Error> {
        let authored = match input {
            CellInput::Inline(cell) => cell,
            CellInput::Json(encoded) => {
                deserialize_authored_cell(serde_json::Value::String(encoded))
                    .map_err(|error| error.to_string())?
            }
            CellInput::File(reference) => {
                let root = std::env::current_dir().map_err(|error| error.to_string())?;
                read_cell_file(&root, &reference.file)?
            }
        };
        Self::try_from(authored)
    }
}

fn read_cell_file(root: &Path, file: &str) -> Result<AuthoredCellSpec, String> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let requested = Path::new(file);
    let path = if requested.is_absolute() {
        requested.to_owned()
    } else {
        root.join(requested)
    };
    let path = path.canonicalize().map_err(|error| {
        format!(
            "Cannot open cell file {file:?}: {error}. Paths are relative to {}",
            root.display()
        )
    })?;
    if !path.starts_with(&root) {
        return Err(format!(
            "Cell file must be inside the MCP working directory {}. Copy the document there or supply inline JSON",
            root.display()
        ));
    }
    if !path.is_file() {
        return Err("Cell file must be a regular JSON file".into());
    }
    let input = File::open(&path).map_err(|error| format!("Cannot read cell file: {error}"))?;
    let value: serde_json::Value = serde_json::from_reader(BufReader::new(input))
        .map_err(|error| format!("Invalid cell JSON file: {error}"))?;
    deserialize_authored_cell(value).map_err(|error| format!("Invalid cell document: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_preserve_canonical_bindings_and_large_documents() {
        let dir = tempfile::tempdir().unwrap();
        let value = serde_json::json!({
            "api_version":"proofstorm/v1alpha1", "name":"imported", "components":[],
            "links":[{"id":"backend","kind":"chain_backend","from":"node","to":"chain","binding":{"type":"chain","network":"regtest"}}]
        });
        let path = dir.path().join("cell.json");
        // Whitespace exceeds the MCP frame ceiling without changing the document.
        std::fs::write(&path, format!("{}{value}", " ".repeat(1024 * 1024))).unwrap();
        let imported =
            CellSpec::try_from(read_cell_file(dir.path(), "cell.json").unwrap()).unwrap();
        let inline =
            CellSpec::try_from(serde_json::from_value::<CellInput>(value).unwrap()).unwrap();
        assert_eq!(imported, inline);
    }

    #[test]
    fn file_references_do_not_escape_the_workspace_or_ignore_extra_fields() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("cell.json");
        std::fs::write(&path, "{}").unwrap();
        assert!(
            read_cell_file(root.path(), path.to_str().unwrap())
                .unwrap_err()
                .contains("working directory")
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, root.path().join("linked.json")).unwrap();
            assert!(
                read_cell_file(root.path(), "linked.json")
                    .unwrap_err()
                    .contains("working directory")
            );
        }
        assert!(
            read_cell_file(root.path(), ".")
                .unwrap_err()
                .contains("regular JSON file")
        );
        assert!(
            serde_json::from_value::<CellInput>(
                serde_json::json!({"file":"cell.json","components":[]})
            )
            .is_err()
        );
    }
}
