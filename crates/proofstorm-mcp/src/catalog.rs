//! Catalog discovery details and configuration-schema reads.
use std::collections::{BTreeMap, BTreeSet};

use proofstorm_core::{
    CandidateSource, CatalogDependencySupport, CatalogEntry, CatalogFeature, CatalogResponse,
    CatalogRuntimeEndpoint, CatalogSupportMatrix, ComponentKind, ControlClass, ReleaseChannel,
    SupportLifecycle,
};
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coded_invalid_request;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntryRequest {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfigSchemaRequest {
    pub id: String,
    pub version: String,
    /// RFC 6901 JSON Pointer. Empty reads the complete configuration schema.
    #[serde(default)]
    pub pointer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CatalogEntrySummary {
    pub id: String,
    pub kind: ComponentKind,
    pub version: String,
    pub preferred: bool,
    pub adapter_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_action_adapter_version: Option<String>,
    pub config_version: String,
    pub config_schema_digest: String,
    /// Controls accepted by publication for this exact component release.
    pub allowed_control: Vec<ControlClass>,
    /// Safe default control for ordinary cell authoring.
    pub recommended_control: ControlClass,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CatalogListResponse {
    pub api_version: String,
    pub catalog_digest: String,
    pub items: Vec<CatalogEntrySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntryDetail {
    pub origin: proofstorm_core::CatalogOrigin,
    pub build_profile: Option<proofstorm_core::CandidateBuildProfile>,
    pub id: String,
    pub kind: ComponentKind,
    pub description: String,
    pub adapter_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_action_adapter_version: Option<String>,
    pub version: String,
    pub preferred: bool,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
    pub config_version: String,
    pub config_schema_digest: String,
    pub features: BTreeSet<CatalogFeature>,
    pub compatible_dependencies: Vec<CatalogDependencySupport>,
    pub support_matrix: CatalogSupportMatrix,
    pub runtime_endpoints: Vec<CatalogRuntimeEndpoint>,
    pub image: String,
    pub source_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CandidateSource>,
    pub allowed_control: Vec<ControlClass>,
    /// Safe default control for ordinary cell authoring.
    pub recommended_control: ControlClass,
    /// Names of all agent-authorable configuration properties. An empty
    /// configuration is valid when `required_config_fields` is empty.
    pub authorable_config_fields: Vec<String>,
    pub required_config_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_defaults: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfigSchemaResponse {
    pub id: String,
    pub version: String,
    pub config_version: String,
    pub config_schema_digest: String,
    pub pointer: String,
    pub fragment: bool,
    pub schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub referenced_schemas: BTreeMap<String, serde_json::Value>,
}

impl CatalogEntryDetail {
    pub(super) fn from_entry(entry: &CatalogEntry, preferred: bool) -> Self {
        Self {
            origin: entry.origin(),
            build_profile: entry.source.as_ref().map_or_else(
                || proofstorm_core::candidate_build_profile(&entry.id),
                |s| s.provenance.as_ref().map(|p| p.profile.clone()),
            ),
            id: entry.id.clone(),
            kind: entry.kind,
            description: entry.description.clone(),
            adapter_version: entry.adapter_version.clone(),
            protocol_action_adapter_version: entry.protocol_action_adapter_version.clone(),
            version: entry.version.clone(),
            preferred,
            release_channel: entry.release_channel,
            support_lifecycle: entry.support_lifecycle,
            config_version: entry.config_version.clone(),
            config_schema_digest: entry.config_schema_digest.clone(),
            features: entry.features.clone(),
            compatible_dependencies: entry.compatible_dependencies.clone(),
            support_matrix: entry.support_matrix.clone(),
            runtime_endpoints: public_endpoints(&entry.runtime_endpoints),
            image: entry.image.clone(),
            source_digest: entry.source_digest.clone(),
            source: entry.source.clone(),
            allowed_control: entry.allowed_control.clone(),
            recommended_control: recommended_control(entry),
            authorable_config_fields: authorable_config_fields(entry),
            required_config_fields: required_config_fields(entry),
            config_defaults: config_defaults(entry),
        }
    }
}

fn recommended_control(entry: &CatalogEntry) -> ControlClass {
    [
        ControlClass::Target,
        ControlClass::Cell,
        ControlClass::Attacker,
        ControlClass::Oracle,
    ]
    .into_iter()
    .find(|control| entry.allowed_control.contains(control))
    .expect("catalog contract requires at least one allowed control")
}

fn authorable_config_fields(entry: &CatalogEntry) -> Vec<String> {
    let mut fields = entry
        .config_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    fields.sort();
    fields
}

fn required_config_fields(entry: &CatalogEntry) -> Vec<String> {
    let mut fields = entry
        .config_schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    fields.sort();
    fields
}

fn config_defaults(entry: &CatalogEntry) -> BTreeMap<String, serde_json::Value> {
    entry
        .config_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.iter())
        .filter_map(|(name, schema)| {
            schema
                .get("default")
                .cloned()
                .map(|default| (name.clone(), default))
        })
        .collect()
}

pub(super) fn exact_catalog_entry<'a>(
    entries: &'a [CatalogEntry],
    id: &str,
    version: &str,
) -> Result<&'a CatalogEntry, ErrorData> {
    entries
        .iter()
        .find(|entry| entry.id == id && entry.version == version)
        .ok_or_else(|| {
            let available_versions = entries
                .iter()
                .filter(|entry| entry.id == id)
                .map(|entry| entry.version.as_str())
                .collect::<Vec<_>>();
            let alternatives = if available_versions.is_empty() {
                "no versions are installed for that implementation".to_owned()
            } else {
                format!("available exact versions: {}", available_versions.join(", "))
            };
            ErrorData::resource_not_found(
                format!(
                    "[catalog_entry_not_found] catalog entry {id:?} version {version:?} was not found; no changes were made. Recovery: use one of the {alternatives}"
                ),
                Some(serde_json::json!({"code": "catalog_entry_not_found"})),
            )
        })
}

pub(super) fn catalog_config_schema_with_catalog(
    request: CatalogConfigSchemaRequest,
    catalog: &CatalogResponse,
) -> Result<CatalogConfigSchemaResponse, ErrorData> {
    if !request.pointer.is_empty() && !request.pointer.starts_with('/') {
        return Err(coded_invalid_request(
            "catalog_schema_pointer_invalid",
            "configuration schema pointer must be empty or begin with '/'",
        ));
    }
    let entry = exact_catalog_entry(&catalog.entries, &request.id, &request.version)?;
    let schema = if request.pointer.is_empty() {
        entry.config_schema.clone()
    } else {
        entry
            .config_schema
            .pointer(&request.pointer)
            .cloned()
            .ok_or_else(|| {
                ErrorData::resource_not_found(
                    format!(
                        "configuration schema pointer {:?} was not found for {:?} version {:?}",
                        request.pointer, request.id, request.version
                    ),
                    Some(serde_json::json!({"code": "catalog_schema_pointer_not_found"})),
                )
            })?
    };
    let mut referenced_schemas = BTreeMap::new();
    collect_local_schema_references(&schema, &entry.config_schema, &mut referenced_schemas)?;
    Ok(CatalogConfigSchemaResponse {
        id: entry.id.clone(),
        version: entry.version.clone(),
        config_version: entry.config_version.clone(),
        config_schema_digest: entry.config_schema_digest.clone(),
        fragment: !request.pointer.is_empty(),
        pointer: request.pointer,
        schema,
        referenced_schemas,
    })
}

fn collect_local_schema_references(
    value: &serde_json::Value,
    root: &serde_json::Value,
    referenced: &mut BTreeMap<String, serde_json::Value>,
) -> Result<(), ErrorData> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_local_schema_references(value, root, referenced)?;
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str)
                && let Some(pointer) = reference.strip_prefix('#')
                && !referenced.contains_key(reference)
            {
                let target = if pointer.is_empty() {
                    root
                } else {
                    root.pointer(pointer).ok_or_else(|| {
                        coded_invalid_request(
                            "catalog_schema_reference_invalid",
                            format!(
                                "configuration schema contains unresolved reference {reference:?}"
                            ),
                        )
                    })?
                };
                referenced.insert(reference.to_owned(), target.clone());
                collect_local_schema_references(target, root, referenced)?;
            }
            for nested in object.values() {
                collect_local_schema_references(nested, root, referenced)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn public_endpoints(endpoints: &[CatalogRuntimeEndpoint]) -> Vec<CatalogRuntimeEndpoint> {
    endpoints
        .iter()
        .cloned()
        .map(|mut endpoint| {
            endpoint.controls = endpoint
                .controls
                .iter()
                .filter_map(|name| {
                    let name = match name.as_str() {
                        "component_exec_live" => "cell_exec",
                        "node_start" => "component_start",
                        "node_stop" => "component_stop",
                        "node_restart" => "component_restart",
                        "reachability_oracle" => "network_probe",
                        other => other,
                    };
                    proofstorm_core::mcp::tool(name).map(|_| name.to_owned())
                })
                .collect();
            if endpoint.id == "component" {
                endpoint.controls.extend(
                    [
                        "cell_exec",
                        "component_forensics",
                        "component_start",
                        "component_stop",
                        "component_restart",
                    ]
                    .map(str::to_owned),
                );
            }
            endpoint.limitations = endpoint
                .limitations
                .into_iter()
                .map(|line| {
                    line.replace("component_exec_live", "cell_exec")
                        .replace("All native and typed operations", "Native operations")
                })
                .collect();
            endpoint
        })
        .collect()
}
