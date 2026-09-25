//! Cell and link inputs share strict parsing across inline documents, JSON strings and files.
use std::{fs::File, io::BufReader, path::Path};

use proofstorm_core::{
    AuthenticationProtocol, BitcoinNetwork, CellPolicy, CellSpec, ComponentSpec, DatabaseRole,
    DependencyBinding, LinkKind, LinkSpec, PaymentMethod,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A link authored through MCP. Backend binding fields are flattened into each
/// kind-specific wire variant so a client cannot silently lose the entire
/// nested binding object. Proofstorm constructs the canonical persisted
/// `DependencyBinding`; peer and network-path variants admit no binding fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AddLinkInput {
    BitcoinPeer {
        id: String,
        from: String,
        to: String,
    },
    LightningPeer {
        id: String,
        from: String,
        to: String,
    },
    ChainBackend {
        id: String,
        from: String,
        to: String,
        network: BitcoinNetwork,
    },
    PaymentBackend {
        id: String,
        from: String,
        to: String,
        method: PaymentMethod,
        unit: String,
    },
    DatabaseBackend {
        id: String,
        from: String,
        to: String,
        role: DatabaseRole,
        /// Database to create on the target server; defaults to `<from>_<role>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        database: Option<String>,
    },
    AuthenticationBackend {
        id: String,
        from: String,
        to: String,
        protocol: AuthenticationProtocol,
    },
    NetworkPath {
        id: String,
        from: String,
        to: String,
    },
}

impl TryFrom<AddLinkInput> for LinkSpec {
    type Error = String;

    fn try_from(input: AddLinkInput) -> Result<Self, Self::Error> {
        let link = match input {
            AddLinkInput::BitcoinPeer { id, from, to } => Self {
                id,
                kind: LinkKind::BitcoinPeer,
                from,
                to,
                binding: None,
            },
            AddLinkInput::LightningPeer { id, from, to } => Self {
                id,
                kind: LinkKind::LightningPeer,
                from,
                to,
                binding: None,
            },
            AddLinkInput::ChainBackend {
                id,
                from,
                to,
                network,
            } => Self {
                id,
                kind: LinkKind::ChainBackend,
                from,
                to,
                binding: Some(DependencyBinding::Chain { network }),
            },
            AddLinkInput::PaymentBackend {
                id,
                from,
                to,
                method,
                unit,
            } => Self {
                id,
                kind: LinkKind::PaymentBackend,
                from,
                to,
                binding: Some(DependencyBinding::Payment { method, unit }),
            },
            AddLinkInput::DatabaseBackend {
                id,
                from,
                to,
                role,
                database,
            } => Self {
                id,
                kind: LinkKind::DatabaseBackend,
                from,
                to,
                binding: Some(DependencyBinding::Database { role, database }),
            },
            AddLinkInput::AuthenticationBackend {
                id,
                from,
                to,
                protocol,
            } => Self {
                id,
                kind: LinkKind::AuthenticationBackend,
                from,
                to,
                binding: Some(DependencyBinding::Authentication { protocol }),
            },
            AddLinkInput::NetworkPath { id, from, to } => Self {
                id,
                kind: LinkKind::NetworkPath,
                from,
                to,
                binding: None,
            },
        };
        Ok(link)
    }
}

impl TryFrom<LinkSpec> for AddLinkInput {
    type Error = String;

    fn try_from(link: LinkSpec) -> Result<Self, Self::Error> {
        let LinkSpec {
            id,
            kind,
            from,
            to,
            binding,
        } = link;
        match (kind, binding) {
            (LinkKind::BitcoinPeer, None) => Ok(Self::BitcoinPeer { id, from, to }),
            (LinkKind::LightningPeer, None) => Ok(Self::LightningPeer { id, from, to }),
            (LinkKind::ChainBackend, Some(DependencyBinding::Chain { network })) => {
                Ok(Self::ChainBackend {
                    id,
                    from,
                    to,
                    network,
                })
            }
            (LinkKind::PaymentBackend, Some(DependencyBinding::Payment { method, unit })) => {
                Ok(Self::PaymentBackend {
                    id,
                    from,
                    to,
                    method,
                    unit,
                })
            }
            (LinkKind::DatabaseBackend, Some(DependencyBinding::Database { role, database })) => {
                Ok(Self::DatabaseBackend {
                    id,
                    from,
                    to,
                    role,
                    database,
                })
            }
            (
                LinkKind::AuthenticationBackend,
                Some(DependencyBinding::Authentication { protocol }),
            ) => Ok(Self::AuthenticationBackend {
                id,
                from,
                to,
                protocol,
            }),
            (LinkKind::NetworkPath, None) => Ok(Self::NetworkPath { id, from, to }),
            _ => Err(format!(
                "canonical {kind:?} link {id:?} has a missing or mismatched binding"
            )),
        }
    }
}

/// Complete cell input accepted at the MCP boundary. Unlike the persisted core
/// model, backend-link variants require their binding fields as flat scalars.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoredCellSpec {
    pub api_version: String,
    pub name: String,
    pub components: Vec<ComponentSpec>,
    pub links: Vec<AddLinkInput>,
    /// Optional capability restrictions. Omit this field for the safe default
    /// policy unless the experiment deliberately needs a narrower envelope.
    #[serde(default)]
    pub policy: CellPolicy,
}

/// Accept the canonical JSON object and the common MCP-client failure mode where
/// that object is encoded one extra time as a JSON string. Parsing still lands
/// in the same strict `AuthoredCellSpec` contract, including unknown-field and
/// required-field checks; malformed or incomplete strings remain fail-closed.
fn deserialize_authored_cell<'de, D>(deserializer: D) -> Result<AuthoredCellSpec, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let value = match value {
        serde_json::Value::String(encoded) => {
            serde_json::from_str(&encoded).map_err(serde::de::Error::custom)?
        }
        value => value,
    };
    let authored_error = match serde_json::from_value(value.clone()) {
        Ok(authored) => return Ok(authored),
        Err(error) => error,
    };
    let has_canonical_binding = value
        .get("links")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|links| {
            links.iter().any(|link| {
                link.as_object()
                    .is_some_and(|link| link.contains_key("binding"))
            })
        });
    if !has_canonical_binding {
        return Err(serde::de::Error::custom(authored_error));
    }
    let canonical: CellSpec = match serde_json::from_value(value) {
        Ok(canonical) => canonical,
        Err(_) => return Err(serde::de::Error::custom(authored_error)),
    };
    Ok(AuthoredCellSpec {
        api_version: canonical.api_version,
        name: canonical.name,
        components: canonical.components,
        links: canonical
            .links
            .into_iter()
            .map(AddLinkInput::try_from)
            .collect::<Result<_, _>>()
            .map_err(serde::de::Error::custom)?,
        policy: canonical.policy,
    })
}

impl TryFrom<AuthoredCellSpec> for CellSpec {
    type Error = String;

    fn try_from(input: AuthoredCellSpec) -> Result<Self, Self::Error> {
        Ok(Self {
            api_version: input.api_version,
            name: input.name,
            components: input.components,
            links: input
                .links
                .into_iter()
                .map(LinkSpec::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            policy: input.policy,
        })
    }
}

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
    use proofstorm_core::API_VERSION;

    #[test]
    fn authored_cell_policy_is_optional_and_defaults_safely() {
        let authored = serde_json::from_value::<AuthoredCellSpec>(serde_json::json!({
            "api_version": API_VERSION,
            "name": "default-policy",
            "components": [],
            "links": []
        }))
        .expect("policy may be omitted");
        assert_eq!(authored.policy, CellPolicy::default());
    }

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
