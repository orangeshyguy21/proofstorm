use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CatalogDependencySupport, CatalogEntry, CatalogFeature, CatalogResponse, DatabaseRole,
    DependencyBinding, LabSpec, LinkKind, LinkSpec, default_backend_registry,
    validate_catalog_component,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_action_adapter_version: Option<String>,
    pub version: String,
    pub config_version: String,
    pub config_schema_digest: String,
    pub features: BTreeSet<CatalogFeature>,
    pub compatible_dependencies: Vec<CatalogDependencySupport>,
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
    let report = crate::validate_lab(lab);
    if !report.valid {
        let issues = serde_json::to_string(&report.issues)
            .map_err(|error| format!("validation_diagnostic_serialization_failed: {error}"))?;
        return Err(format!("lab_validation_failed: {issues}"));
    }
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
                &entry.config_schema_digest,
                &component.config,
            ));
            let relevant_links = effective
                .links
                .iter()
                .filter(|link| link.from == component.id && is_rollout_relevant_link(link.kind))
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
                    require_compatible_dependency(
                        component.id.as_str(),
                        entry,
                        link,
                        target_entry,
                    )?;
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
                &entry.config_schema_digest,
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
                protocol_action_adapter_version: entry.protocol_action_adapter_version.clone(),
                version: entry.version.clone(),
                config_version: entry.config_version.clone(),
                config_schema_digest: entry.config_schema_digest.clone(),
                features: entry.features.clone(),
                compatible_dependencies: entry.compatible_dependencies.clone(),
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

fn is_rollout_relevant_link(kind: LinkKind) -> bool {
    matches!(
        kind,
        LinkKind::ChainBackend
            | LinkKind::PaymentBackend
            | LinkKind::DatabaseBackend
            | LinkKind::AuthenticationBackend
            | LinkKind::NetworkPath
    )
}

fn require_compatible_dependency(
    component_id: &str,
    entry: &CatalogEntry,
    link: &LinkSpec,
    target: &CatalogEntry,
) -> Result<(), String> {
    if matches!(
        link.kind,
        LinkKind::ChainBackend
            | LinkKind::PaymentBackend
            | LinkKind::DatabaseBackend
            | LinkKind::AuthenticationBackend
    ) && !entry.compatible_dependencies.iter().any(|support| {
        support.link_kind == link.kind
            && support.implementation == target.id
            && support.versions.contains(&target.version)
    }) {
        return Err(format!(
            "component {component_id:?} version {:?} does not declare {:?} compatibility with target {:?} version {:?}",
            entry.version, link.kind, link.to, target.version
        ));
    }
    if link.kind == LinkKind::PaymentBackend {
        let Some(DependencyBinding::Payment { method, unit }) = &link.binding else {
            return Err(format!(
                "component {component_id:?} binding {:?} lacks a typed payment method and unit",
                link.id
            ));
        };
        if !entry.support_matrix.payment_bindings.iter().any(|support| {
            support.method == *method
                && support.unit == *unit
                && support.backend.implementation == target.id
                && support.backend.versions.contains(&target.version)
        }) {
            return Err(format!(
                "component {component_id:?} version {:?} binding {:?} does not support payment tuple method {method:?}, unit {unit:?}, backend {:?} version {:?}",
                entry.version, link.id, target.id, target.version
            ));
        }
    }
    if link.kind == LinkKind::DatabaseBackend {
        let Some(DependencyBinding::Database { role }) = link.binding else {
            return Err(format!(
                "component {component_id:?} binding {:?} lacks a typed database role",
                link.id
            ));
        };
        let supported = match role {
            DatabaseRole::Primary => target.id == "postgresql",
            DatabaseRole::Cache => entry.id == "nutshell" && target.id == "redis",
            DatabaseRole::Authentication => false,
        };
        if !supported {
            return Err(format!(
                "component {component_id:?} version {:?} binding {:?} does not support database role {role:?} with backend {:?} version {:?}",
                entry.version, link.id, target.id, target.version
            ));
        }
    }
    if link.kind == LinkKind::AuthenticationBackend {
        let Some(crate::DependencyBinding::Authentication { protocol }) = link.binding else {
            return Err(format!(
                "component {component_id:?} binding {:?} lacks a typed authentication protocol",
                link.id
            ));
        };
        if entry.id != "nutshell"
            || target.id != "keycloak"
            || protocol != crate::AuthenticationProtocol::Oidc
        {
            return Err(format!(
                "component {component_id:?} version {:?} binding {:?} does not support authentication protocol {protocol:?} with backend {:?} version {:?}",
                entry.version, link.id, target.id, target.version
            ));
        }
    }
    Ok(())
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
    use crate::{
        ComponentKind, ComponentSpec, ControlClass, LabPolicy, LinkSpec, SupportLifecycle,
        default_catalog,
    };
    use std::collections::BTreeMap;

    fn test_config_version(implementation: &str) -> &'static str {
        match implementation {
            "bitcoin-core" => "bitcoin-core/30/v1",
            "lnd" => "lnd/0.20/v1",
            "cdk" => "cdk-mintd/0.18/v1",
            "nutshell" => "nutshell-mint/0.20/v1",
            "postgresql" => "postgresql/17/v1",
            "redis" => "redis/8.10/v1",
            _ => panic!("unknown test implementation {implementation:?}"),
        }
    }

    fn payment_lab(method: crate::PaymentMethod, unit: &str) -> LabSpec {
        let component = |id: &str, implementation: &str, kind, control| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: test_config_version(implementation).into(),
            control,
            config: BTreeMap::new(),
        };
        LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "payment-contract".into(),
            components: vec![
                component("mint", "cdk", ComponentKind::Mint, ControlClass::Target),
                component(
                    "lightning",
                    "lnd",
                    ComponentKind::Lightning,
                    ControlClass::Laboratory,
                ),
            ],
            links: vec![LinkSpec {
                id: "mint-payment".into(),
                kind: LinkKind::PaymentBackend,
                from: "mint".into(),
                to: "lightning".into(),
                binding: Some(DependencyBinding::Payment {
                    method,
                    unit: unit.into(),
                }),
            }],
            policy: LabPolicy::default(),
        }
    }

    #[test]
    fn lock_is_deterministic_across_component_order() {
        let component = |id: &str, implementation: &str, kind| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: test_config_version(implementation).into(),
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
    fn payment_binding_tuple_must_be_supported_by_the_exact_mint_version() {
        resolve_lock(
            &payment_lab(crate::PaymentMethod::Bolt11, "sat"),
            &default_catalog(),
        )
        .expect("supported payment tuple");

        let mut false_cross_product_catalog = default_catalog();
        let cdk = false_cross_product_catalog
            .entries
            .iter_mut()
            .find(|entry| entry.id == "cdk")
            .expect("CDK entry");
        cdk.features.insert(crate::CatalogFeature::Bolt12);
        cdk.support_matrix
            .payment_methods
            .insert(crate::PaymentMethod::Bolt12);
        let method_error = resolve_lock(
            &payment_lab(crate::PaymentMethod::Bolt12, "sat"),
            &false_cross_product_catalog,
        )
        .expect_err("unsupported method must refuse publication");
        assert!(method_error.contains("does not support payment tuple method Bolt12"));

        let unit_error = resolve_lock(
            &payment_lab(crate::PaymentMethod::Bolt11, "msat"),
            &default_catalog(),
        )
        .expect_err("unsupported unit must refuse publication");
        assert!(unit_error.contains("unit \"msat\""));
    }

    #[test]
    fn database_roles_refuse_crossed_primary_and_cache_implementations() {
        let component = |id: &str, implementation: &str, kind, control| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: test_config_version(implementation).into(),
            control,
            config: BTreeMap::new(),
        };
        let mut lab = LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "database-roles".into(),
            components: vec![
                component(
                    "mint",
                    "nutshell",
                    ComponentKind::Mint,
                    ControlClass::Target,
                ),
                component(
                    "cache",
                    "redis",
                    ComponentKind::Database,
                    ControlClass::Laboratory,
                ),
            ],
            links: vec![LinkSpec {
                id: "mint-cache".into(),
                kind: LinkKind::DatabaseBackend,
                from: "mint".into(),
                to: "cache".into(),
                binding: Some(DependencyBinding::Database {
                    role: DatabaseRole::Cache,
                }),
            }],
            policy: LabPolicy::default(),
        };
        resolve_lock(&lab, &default_catalog()).expect("Nutshell Redis cache binding");

        lab.links[0].binding = Some(DependencyBinding::Database {
            role: DatabaseRole::Primary,
        });
        let error = resolve_lock(&lab, &default_catalog()).expect_err("Redis cannot be primary");
        assert!(error.contains("does not support database role Primary"));

        lab.components[1] = component(
            "cache",
            "postgresql",
            ComponentKind::Database,
            ControlClass::Laboratory,
        );
        lab.links[0].binding = Some(DependencyBinding::Database {
            role: DatabaseRole::Cache,
        });
        let error = resolve_lock(&lab, &default_catalog()).expect_err("PostgreSQL cannot be cache");
        assert!(error.contains("does not support database role Cache"));
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
                config_version: "bitcoin-core/30/v1".into(),
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
    fn every_cdk_policy_field_is_locked_as_rollout_affecting_input() {
        let lab = payment_lab(crate::PaymentMethod::Bolt11, "sat");
        let catalog = default_catalog();
        let baseline = resolve_lock(&lab, &catalog).expect("baseline CDK lock");
        let baseline_mint = baseline
            .entries
            .iter()
            .find(|entry| entry.component_id == "mint")
            .expect("baseline mint lock");
        let cases = BTreeMap::from([
            (
                "contact_email",
                serde_json::json!("operator@example.invalid"),
            ),
            (
                "contact_nostr_public_key",
                serde_json::json!(
                    "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
                ),
            ),
            ("description", serde_json::json!("Custom regtest mint")),
            ("description_long", serde_json::json!("Long-form metadata")),
            ("enable_info_page", serde_json::json!(true)),
            ("http_cache_tti_seconds", serde_json::json!(61)),
            ("http_cache_ttl_seconds", serde_json::json!(62)),
            (
                "icon_url",
                serde_json::json!("https://proofstorm.invalid/mint.png"),
            ),
            ("input_fee_ppk", serde_json::json!(321)),
            ("max_melt_sat", serde_json::json!(499_999)),
            ("max_mint_sat", serde_json::json!(499_999)),
            ("max_inputs", serde_json::json!(999)),
            ("max_outputs", serde_json::json!(998)),
            ("melt_quote_ttl_seconds", serde_json::json!(333)),
            ("min_melt_sat", serde_json::json!(2)),
            ("min_mint_sat", serde_json::json!(2)),
            ("mint_quote_ttl_seconds", serde_json::json!(777)),
            ("motd", serde_json::json!("Agents welcome")),
            ("name", serde_json::json!("Custom CDK")),
            (
                "tos_url",
                serde_json::json!("https://proofstorm.invalid/terms"),
            ),
            ("use_keyset_v2", serde_json::json!(false)),
        ]);

        for (field, value) in cases {
            let mut configured = lab.clone();
            configured.components[0].config.insert(field.into(), value);
            let lock = resolve_lock(&configured, &catalog)
                .unwrap_or_else(|error| panic!("field {field:?} must publish: {error}"));
            let mint = lock
                .entries
                .iter()
                .find(|entry| entry.component_id == "mint")
                .expect("configured mint lock");
            assert_ne!(
                baseline_mint.effective_config_digest, mint.effective_config_digest,
                "field {field:?} must affect effective configuration identity"
            );
            assert_ne!(
                baseline_mint.rollout_digest, mint.rollout_digest,
                "field {field:?} must trigger a mint rollout"
            );
        }
    }

    #[test]
    fn rollout_digest_changes_only_for_render_affecting_input() {
        let component = |id: &str, implementation: &str, kind| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: test_config_version(implementation).into(),
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
                id: "alice-chain".into(),
                kind: LinkKind::ChainBackend,
                from: "alice".into(),
                to: "chain-a".into(),
                binding: Some(DependencyBinding::Chain {
                    network: crate::BitcoinNetwork::Regtest,
                }),
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
            config_version: "bitcoin-core/30/v1".into(),
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
                config_version: "bitcoin-core/30/v1".into(),
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

        lab.components[0].config_version = "bitcoin-core/30/v1".into();
        resolve_lock(&lab, &default_catalog()).expect("supported config");
    }

    #[test]
    fn omitted_versions_resolve_only_to_one_preferred_exact_entry() {
        let mut catalog = default_catalog();
        let mut older = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "bitcoin-core")
            .expect("bitcoin entry")
            .clone();
        older.version = "29.1".into();
        older.config_version = "bitcoin-core/29/v1".into();
        older.support_lifecycle = SupportLifecycle::Supported;
        catalog.entries.push(older);
        let mut component = ComponentSpec {
            id: "chain".into(),
            kind: ComponentKind::Bitcoin,
            implementation: "bitcoin-core".into(),
            version: None,
            config_version: "bitcoin-core/30/v1".into(),
            control: ControlClass::Laboratory,
            config: BTreeMap::new(),
        };

        let selected = validate_catalog_component(&component, &catalog).expect("preferred entry");
        assert_eq!(selected.version, "30.0");

        component.version = Some("29.1".into());
        component.config_version = "bitcoin-core/29/v1".into();
        let selected = validate_catalog_component(&component, &catalog).expect("exact entry");
        assert_eq!(selected.version, "29.1");

        component.version = Some("31.0".into());
        let error = validate_catalog_component(&component, &catalog).expect_err("unsupported");
        assert!(error.contains("explicitly supported versions"));

        catalog
            .entries
            .iter_mut()
            .find(|entry| entry.id == "bitcoin-core" && entry.version == "29.1")
            .expect("older entry")
            .support_lifecycle = SupportLifecycle::Preferred;
        component.version = None;
        component.config_version = "bitcoin-core/30/v1".into();
        let error = validate_catalog_component(&component, &catalog).expect_err("ambiguous");
        assert!(error.contains("exactly one is required"));
    }

    #[test]
    fn lock_carries_the_exact_configuration_and_action_contract() {
        let lab = LabSpec {
            api_version: crate::API_VERSION.into(),
            name: "support-contract-lock".into(),
            components: vec![ComponentSpec {
                id: "chain".into(),
                kind: ComponentKind::Bitcoin,
                implementation: "bitcoin-core".into(),
                version: None,
                config_version: "bitcoin-core/30/v1".into(),
                control: ControlClass::Laboratory,
                config: BTreeMap::new(),
            }],
            links: vec![],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let entry = &lock.entries[0];
        assert_eq!(entry.version, "30.0");
        assert_eq!(entry.config_version, "bitcoin-core/30/v1");
        assert!(entry.config_schema_digest.starts_with("sha256:"));
        assert!(entry.protocol_action_adapter_version.is_some());
        assert!(entry.features.contains(&CatalogFeature::Regtest));
        let encoded = serde_json::to_string(&lock).expect("lock serializes");
        assert!(!encoded.contains("rpc_credentials"));
        assert!(!encoded.contains("proofstorm-regtest-only"));
    }
}
