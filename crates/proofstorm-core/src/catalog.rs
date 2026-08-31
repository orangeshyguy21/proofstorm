use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ComponentKind, ComponentSpec, ControlClass};

type ConfigField = (&'static str, fn(&Value) -> bool);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    pub kind: ComponentKind,
    pub description: String,
    pub adapter_version: String,
    pub version: String,
    pub config_version: String,
    pub config_schema: Value,
    pub image: String,
    pub source_digest: String,
    pub allowed_control: Vec<ControlClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogResponse {
    pub api_version: String,
    pub entries: Vec<CatalogEntry>,
}

#[must_use]
pub fn default_catalog() -> CatalogResponse {
    let config_version = "v1alpha1";
    let adapter_version = "0.1.0-alpha.1";
    let source_digest = |id: &str| crate::digest_json(&(id, adapter_version, config_version));
    CatalogResponse {
        api_version: crate::API_VERSION.to_owned(),
        entries: vec![
            CatalogEntry {
                id: "bitcoin-core".into(),
                kind: ComponentKind::Bitcoin,
                description: "Bitcoin Core regtest node".into(),
                adapter_version: adapter_version.into(),
                version: "30.0".into(),
                config_version: config_version.into(),
                config_schema: object_schema(&[
                    ("txindex", "boolean"),
                    ("fallback_fee", "number"),
                ]),
                image: "docker.io/polarlightning/bitcoind@sha256:6b15e7efb79995a18441806f509e40316428a901f1cdc5c54cd25b03ac513cb9".into(),
                source_digest: source_digest("bitcoin-core"),
                allowed_control: vec![ControlClass::Laboratory, ControlClass::Attacker],
            },
            CatalogEntry {
                id: "lnd".into(),
                kind: ComponentKind::Lightning,
                description: "LND regtest Lightning node".into(),
                adapter_version: adapter_version.into(),
                version: "0.20.0-beta".into(),
                config_version: config_version.into(),
                config_schema: object_schema(&[("alias", "string")]),
                image: "docker.io/polarlightning/lnd@sha256:ad708a2dacccd6ae104e78577f6a724095b80bac76ddf363f4bf8d22fbe0979f".into(),
                source_digest: source_digest("lnd"),
                allowed_control: vec![ControlClass::Laboratory, ControlClass::Attacker],
            },
            CatalogEntry {
                id: "cln".into(),
                kind: ComponentKind::Lightning,
                description: "Core Lightning regtest node".into(),
                adapter_version: adapter_version.into(),
                version: "26.06.7".into(),
                config_version: config_version.into(),
                config_schema: object_schema(&[("alias", "string")]),
                image: "docker.io/elementsproject/lightningd@sha256:f0bd6bf244b815adf1b633bcfff6fc0cf5fd026efefa1367839552f1490f7fbd".into(),
                source_digest: source_digest("cln"),
                allowed_control: vec![ControlClass::Laboratory, ControlClass::Attacker],
            },
            CatalogEntry {
                id: "cdk".into(),
                kind: ComponentKind::Mint,
                description: "CDK Cashu mint".into(),
                adapter_version: adapter_version.into(),
                version: "0.17.1".into(),
                config_version: config_version.into(),
                config_schema: object_schema(&[
                    ("name", "string"),
                    ("description", "string"),
                ]),
                image: "docker.io/cashubtc/mintd@sha256:b17af7ed8bce85086c011afeae1d578ccde1c0b098b16fd961140931dec06f8a".into(),
                source_digest: source_digest("cdk"),
                allowed_control: vec![ControlClass::Target],
            },
            CatalogEntry {
                id: "nutshell-wallet".into(),
                kind: ComponentKind::Wallet,
                description: "Persistent Cashu Nutshell wallet workspace".into(),
                adapter_version: adapter_version.into(),
                version: "0.20.2".into(),
                config_version: config_version.into(),
                config_schema: object_schema(&[]),
                image: "docker.io/cashubtc/nutshell@sha256:65e9cbe23aaa1aeb27ce7206fa854a80f39ce8db1c9121eaecfc053a22506574".into(),
                source_digest: source_digest("nutshell-wallet"),
                allowed_control: vec![ControlClass::Laboratory, ControlClass::Attacker],
            },
            CatalogEntry {
                id: "attacker-workspace".into(),
                kind: ComponentKind::Attacker,
                description: "Disposable adversarial client workspace".into(),
                adapter_version: adapter_version.into(),
                version: "0.1.0-alpha.1".into(),
                config_version: config_version.into(),
                config_schema: object_schema(&[]),
                image: "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662".into(),
                source_digest: source_digest("attacker-workspace"),
                allowed_control: vec![ControlClass::Attacker],
            },
        ],
    }
}

fn object_schema(properties: &[(&str, &str)]) -> Value {
    let properties = properties
        .iter()
        .map(|(name, type_)| ((*name).to_owned(), json!({"type": type_})))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties
    })
}

/// Validate adapter configuration against the installed v1alpha1 contract.
///
/// # Errors
///
/// Returns an error for unknown fields or values of the wrong JSON type.
pub fn validate_component_config(component: &ComponentSpec) -> Result<(), String> {
    let allowed: &[ConfigField] = match component.implementation.as_str() {
        "bitcoin-core" => &[
            ("txindex", Value::is_boolean),
            ("fallback_fee", Value::is_number),
        ],
        "lnd" | "cln" => &[("alias", Value::is_string)],
        "cdk" => &[
            ("name", Value::is_string),
            ("description", Value::is_string),
        ],
        "nutshell-wallet" | "attacker-workspace" => &[],
        _ => return Ok(()),
    };
    for (name, value) in &component.config {
        let Some((_, predicate)) = allowed.iter().find(|(allowed, _)| allowed == name) else {
            return Err(format!(
                "component {:?} configuration field {name:?} is unsupported",
                component.id
            ));
        };
        if !predicate(value) {
            return Err(format!(
                "component {:?} configuration field {name:?} has the wrong type",
                component.id
            ));
        }
    }
    Ok(())
}

/// Resolve and validate one component against an installed catalog entry.
///
/// # Errors
///
/// Returns an error for an unknown implementation, kind/control mismatch,
/// unsupported version or configuration contract, or invalid configuration.
pub fn validate_catalog_component<'a>(
    component: &ComponentSpec,
    catalog: &'a CatalogResponse,
) -> Result<&'a CatalogEntry, String> {
    validate_component_config(component)?;
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == component.implementation)
        .ok_or_else(|| {
            format!(
                "catalog entry {:?} is not installed",
                component.implementation
            )
        })?;
    if component.kind != entry.kind {
        return Err(format!(
            "component {:?} kind {:?} does not match catalog kind {:?}",
            component.id, component.kind, entry.kind
        ));
    }
    if !entry.allowed_control.contains(&component.control) {
        return Err(format!(
            "component {:?} control class {:?} is not allowed by catalog entry {:?}",
            component.id, component.control, entry.id
        ));
    }
    let requested = component.version.as_deref().unwrap_or(&entry.version);
    if requested != entry.version {
        return Err(format!(
            "component {:?} requests version {:?}, installed version is {:?}",
            component.id, requested, entry.version
        ));
    }
    if component.config_version != entry.config_version {
        return Err(format!(
            "component {:?} requests configuration version {:?}, installed version is {:?}",
            component.id, component.config_version, entry.config_version
        ));
    }
    Ok(entry)
}
