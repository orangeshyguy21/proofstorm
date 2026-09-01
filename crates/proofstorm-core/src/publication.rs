use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CatalogResponse, LabSpec, validate_catalog_component};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    pub component_id: String,
    pub catalog_id: String,
    pub adapter_version: String,
    pub version: String,
    pub config_version: String,
    pub config_digest: String,
    pub image: String,
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedLock {
    pub api_version: String,
    pub digest: String,
    pub entries: Vec<LockEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishedRevision {
    pub workspace_id: String,
    pub digest: String,
    pub lab: LabSpec,
    pub lock: ResolvedLock,
}

/// Resolve every lab component against the installed catalog and return a
/// component-order-independent lock.
///
/// # Errors
///
/// Returns an error when an implementation is not installed or its requested
/// implementation or configuration version differs from the installed catalog
/// entry.
pub fn resolve_lock(lab: &LabSpec, catalog: &CatalogResponse) -> Result<ResolvedLock, String> {
    let mut entries = lab
        .components
        .iter()
        .map(|component| {
            let entry = validate_catalog_component(component, catalog)?;
            Ok(LockEntry {
                component_id: component.id.clone(),
                catalog_id: entry.id.clone(),
                adapter_version: entry.adapter_version.clone(),
                version: entry.version.clone(),
                config_version: entry.config_version.clone(),
                config_digest: digest_json(&component.config),
                image: entry.image.clone(),
                source_digest: entry.source_digest.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    let digest = digest_json(&entries);
    Ok(ResolvedLock {
        api_version: crate::API_VERSION.to_owned(),
        digest,
        entries,
    })
}

#[must_use]
/// Hash a typed Proofstorm value using its deterministic JSON representation.
///
/// # Panics
///
/// Panics if the supplied typed value cannot be represented as JSON.
pub fn digest_json<T: Serialize>(value: &T) -> String {
    let encoded = serde_json::to_vec(value).expect("typed Proofstorm value serializes");
    format!("sha256:{:x}", Sha256::digest(encoded))
}

#[must_use]
pub fn publication_digest(workspace: &str, lab: &LabSpec, lock: &ResolvedLock) -> String {
    digest_json(&(workspace, lab, lock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentKind, ComponentSpec, ControlClass, LabPolicy, default_catalog};
    use std::collections::BTreeMap;

    #[test]
    fn lock_is_deterministic_across_component_order() {
        let component = |id: &str, implementation: &str, kind| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: "v1alpha1".into(),
            control: ControlClass::Laboratory,
            config: BTreeMap::new(),
        };
        let mut lab = LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "lock-test".into(),
            components: vec![
                component("z-lightning", "lnd", ComponentKind::Lightning),
                component("a-chain", "bitcoin-core", ComponentKind::Bitcoin),
            ],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let first = resolve_lock(&lab, &default_catalog()).expect("resolve lock");
        lab.components.reverse();
        let second = resolve_lock(&lab, &default_catalog()).expect("resolve lock");
        assert_eq!(first, second);

        lab.components[0]
            .config
            .insert("txindex".into(), serde_json::json!(true));
        let configured = resolve_lock(&lab, &default_catalog()).expect("resolve configured lock");
        assert_ne!(first.digest, configured.digest);
    }

    #[test]
    fn unsupported_configuration_version_refuses_publication() {
        let mut lab = LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "config-version-test".into(),
            components: vec![ComponentSpec {
                id: "chain".into(),
                kind: ComponentKind::Bitcoin,
                implementation: "bitcoin-core".into(),
                version: None,
                config_version: "v2".into(),
                control: ControlClass::Laboratory,
                config: BTreeMap::new(),
            }],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let error = resolve_lock(&lab, &default_catalog()).expect_err("unsupported config");
        assert!(error.contains("configuration version"));

        lab.components[0].config_version = "v1alpha1".into();
        resolve_lock(&lab, &default_catalog()).expect("supported config");
    }
}
