use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CatalogResponse, LabSpec, LinkKind, default_backend_registry, validate_catalog_component,
};

pub const LOCK_API_VERSION: &str = "proofstorm/lock/v2alpha1";
pub const EFFECTIVE_CONFIG_DIGEST_VERSION: &str = "proofstorm/effective-config-digest/v1";
pub const ROLLOUT_DIGEST_VERSION: &str = "proofstorm/rollout-digest/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    pub component_id: String,
    pub catalog_id: String,
    pub adapter_version: String,
    pub version: String,
    pub config_version: String,
    pub effective_config_digest: String,
    pub rollout_digest: String,
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

/// Resolve omitted backend defaults and canonical ordering for publication.
///
/// # Errors
///
/// Returns an error when a component is not installed, violates its catalog
/// contract, or has no registered backend contract.
pub fn resolve_effective_lab(lab: &LabSpec, catalog: &CatalogResponse) -> Result<LabSpec, String> {
    let registry = default_backend_registry();
    let mut effective = lab.clone();
    effective.components = lab
        .components
        .iter()
        .map(|component| {
            validate_catalog_component(component, catalog)?;
            let component = registry.resolve_effective_component(component)?;
            validate_catalog_component(&component, catalog)?;
            Ok(component)
        })
        .collect::<Result<Vec<_>, String>>()?;
    effective
        .components
        .sort_by(|left, right| left.id.cmp(&right.id));
    effective.links.sort();
    Ok(effective)
}

/// Resolve every lab component against the installed catalog and return a
/// component-order-independent, rollout-aware lock.
///
/// # Errors
///
/// Returns an error when an implementation is not installed or its requested
/// implementation or configuration version differs from the installed catalog
/// entry.
pub fn resolve_lock(lab: &LabSpec, catalog: &CatalogResponse) -> Result<ResolvedLock, String> {
    let effective = resolve_effective_lab(lab, catalog)?;
    let backends = default_backend_registry();
    let mut entries = effective
        .components
        .iter()
        .map(|component| {
            let entry = validate_catalog_component(component, catalog)?;
            let backend = backends.require(&entry.id)?;
            let effective_config_digest = digest_json(&(
                EFFECTIVE_CONFIG_DIGEST_VERSION,
                &component.config_version,
                &component.config,
            ));
            let relevant_links = effective
                .links
                .iter()
                .filter(|link| {
                    link.from == component.id
                        && matches!(
                            link.kind,
                            LinkKind::ChainBackend
                                | LinkKind::LightningBackend
                                | LinkKind::NetworkPath
                        )
                })
                .collect::<Vec<_>>();
            let linked_target_contracts = relevant_links
                .iter()
                .map(|link| {
                    let target = effective
                        .components
                        .iter()
                        .find(|target| target.id == link.to)
                        .ok_or_else(|| {
                            format!(
                                "component {:?} references missing linked target {:?}",
                                component.id, link.to
                            )
                        })?;
                    let target_entry = validate_catalog_component(target, catalog)?;
                    let target_backend = backends.require(&target_entry.id)?;
                    Ok((
                        *link,
                        &target_entry.id,
                        &target_entry.adapter_version,
                        &target_backend.service_ports,
                        &target_backend.execution_state_contract,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let rollout_digest = digest_json(&(
                ROLLOUT_DIGEST_VERSION,
                &entry.id,
                &entry.adapter_version,
                &entry.image,
                &effective_config_digest,
                &linked_target_contracts,
                &backend.execution_state_contract,
                &backend.execution_mounts,
                &backend.execution_environment,
            ));
            Ok(LockEntry {
                component_id: component.id.clone(),
                catalog_id: entry.id.clone(),
                adapter_version: entry.adapter_version.clone(),
                version: entry.version.clone(),
                config_version: entry.config_version.clone(),
                effective_config_digest,
                rollout_digest,
                image: entry.image.clone(),
                source_digest: entry.source_digest.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    let digest = digest_json(&entries);
    Ok(ResolvedLock {
        api_version: LOCK_API_VERSION.to_owned(),
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
    use crate::{ComponentKind, ComponentSpec, ControlClass, LabPolicy, LinkSpec, default_catalog};
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
            .insert("txindex".into(), serde_json::json!(false));
        let configured = resolve_lock(&lab, &default_catalog()).expect("resolve configured lock");
        assert_ne!(first.digest, configured.digest);
    }

    #[test]
    fn omitted_and_explicit_defaults_publish_identically() {
        let mut omitted = LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "effective-defaults".into(),
            components: vec![ComponentSpec {
                id: "chain".into(),
                kind: ComponentKind::Bitcoin,
                implementation: "bitcoin-core".into(),
                version: None,
                config_version: "v1alpha1".into(),
                control: ControlClass::Laboratory,
                config: BTreeMap::new(),
            }],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let catalog = default_catalog();
        let effective_omitted =
            resolve_effective_lab(&omitted, &catalog).expect("resolve omitted defaults");
        omitted.components[0]
            .config
            .insert("txindex".into(), serde_json::json!(true));
        omitted.components[0]
            .config
            .insert("fallback_fee".into(), serde_json::json!(0.0002));
        let effective_explicit =
            resolve_effective_lab(&omitted, &catalog).expect("resolve explicit defaults");
        assert_eq!(effective_omitted, effective_explicit);
        assert_eq!(
            resolve_lock(&effective_omitted, &catalog).expect("lock omitted"),
            resolve_lock(&effective_explicit, &catalog).expect("lock explicit")
        );
    }

    #[test]
    fn rollout_digest_changes_only_for_render_affecting_input() {
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
            name: "rollout-digest".into(),
            components: vec![
                component("chain-a", "bitcoin-core", ComponentKind::Bitcoin),
                component("chain-b", "bitcoin-core", ComponentKind::Bitcoin),
                component("alice", "lnd", ComponentKind::Lightning),
            ],
            links: vec![LinkSpec {
                kind: LinkKind::ChainBackend,
                from: "alice".into(),
                to: "chain-a".into(),
            }],
            policy: LabPolicy::default(),
        };
        let catalog = default_catalog();
        let first = resolve_lock(&lab, &catalog).expect("first lock");
        let first_alice = first
            .entries
            .iter()
            .find(|entry| entry.component_id == "alice")
            .expect("alice lock");
        let first_chain = first
            .entries
            .iter()
            .find(|entry| entry.component_id == "chain-a")
            .expect("chain lock");

        lab.name = "non-rendering-metadata".into();
        let renamed = resolve_lock(&lab, &catalog).expect("renamed lock");
        assert_eq!(first, renamed);

        lab.links[0].to = "chain-b".into();
        let relinked = resolve_lock(&lab, &catalog).expect("relinked lock");
        let relinked_alice = relinked
            .entries
            .iter()
            .find(|entry| entry.component_id == "alice")
            .expect("relinked alice lock");
        let relinked_chain = relinked
            .entries
            .iter()
            .find(|entry| entry.component_id == "chain-a")
            .expect("relinked chain lock");
        assert_ne!(first_alice.rollout_digest, relinked_alice.rollout_digest);
        assert_eq!(first_chain.rollout_digest, relinked_chain.rollout_digest);

        let mut upgraded_catalog = catalog.clone();
        upgraded_catalog
            .entries
            .iter_mut()
            .find(|entry| entry.id == "bitcoin-core")
            .expect("bitcoin catalog entry")
            .adapter_version = "0.1.0-alpha.2".into();
        let upgraded =
            resolve_lock(&lab, &upgraded_catalog).expect("upgraded target contract lock");
        let upgraded_alice = upgraded
            .entries
            .iter()
            .find(|entry| entry.component_id == "alice")
            .expect("upgraded alice lock");
        assert_ne!(
            relinked_alice.rollout_digest, upgraded_alice.rollout_digest,
            "a linked target contract change must roll its dependent"
        );
    }

    #[test]
    fn rollout_digest_covers_catalog_and_canonical_map_inputs() {
        let component = |config| ComponentSpec {
            id: "chain".into(),
            kind: ComponentKind::Bitcoin,
            implementation: "bitcoin-core".into(),
            version: None,
            config_version: "v1alpha1".into(),
            control: ControlClass::Laboratory,
            config,
        };
        let mut first_config = BTreeMap::new();
        first_config.insert("txindex".into(), serde_json::json!(false));
        first_config.insert("fallback_fee".into(), serde_json::json!(0.001));
        let mut reversed_config = BTreeMap::new();
        reversed_config.insert("fallback_fee".into(), serde_json::json!(0.001));
        reversed_config.insert("txindex".into(), serde_json::json!(false));
        let lab = |config| LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "catalog-digest-inputs".into(),
            components: vec![component(config)],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let catalog = default_catalog();
        let baseline = resolve_lock(&lab(first_config), &catalog).expect("baseline lock");
        let reordered =
            resolve_lock(&lab(reversed_config.clone()), &catalog).expect("reordered config lock");
        assert_eq!(baseline, reordered);

        let mut adapter_catalog = catalog.clone();
        adapter_catalog.entries[0].adapter_version = "0.1.0-alpha.2".into();
        let changed_adapter =
            resolve_lock(&lab(reversed_config.clone()), &adapter_catalog).expect("adapter lock");
        assert_ne!(
            baseline.entries[0].rollout_digest,
            changed_adapter.entries[0].rollout_digest
        );
        assert_eq!(
            baseline.entries[0].effective_config_digest,
            changed_adapter.entries[0].effective_config_digest
        );

        let mut image_catalog = catalog;
        image_catalog.entries[0].image =
            format!("example.invalid/bitcoin@sha256:{}", "1".repeat(64));
        let changed_image =
            resolve_lock(&lab(reversed_config), &image_catalog).expect("image lock");
        assert_ne!(
            baseline.entries[0].rollout_digest,
            changed_image.entries[0].rollout_digest
        );
        assert_eq!(
            baseline.entries[0].effective_config_digest,
            changed_image.entries[0].effective_config_digest
        );
    }

    #[test]
    fn new_lock_contract_is_strict_and_versioned() {
        let lab = LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "strict-lock".into(),
            components: vec![ComponentSpec {
                id: "chain".into(),
                kind: ComponentKind::Bitcoin,
                implementation: "bitcoin-core".into(),
                version: None,
                config_version: "v1alpha1".into(),
                control: ControlClass::Laboratory,
                config: BTreeMap::new(),
            }],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("strict lock");
        assert_eq!(lock.api_version, LOCK_API_VERSION);
        assert!(
            lock.entries[0]
                .effective_config_digest
                .starts_with("sha256:")
        );
        let mut encoded = serde_json::to_value(&lock).expect("lock serializes");
        encoded["entries"][0]
            .as_object_mut()
            .expect("entry object")
            .remove("rollout_digest");
        let error = serde_json::from_value::<ResolvedLock>(encoded)
            .expect_err("pre-B1 lock must not deserialize");
        assert!(error.to_string().contains("rollout_digest"));
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
