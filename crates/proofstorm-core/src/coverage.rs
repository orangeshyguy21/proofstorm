use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BackendContractRegistry, CatalogDependencySupport, CatalogResponse, CatalogSupportMatrix,
    ComponentKind, ConfigSettingClass, ReleaseChannel, SupportLifecycle, digest_json,
};

pub const CONFIGURATION_COVERAGE_API_VERSION: &str = "proofstorm/configuration-coverage/v1alpha1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationFieldCoverage {
    pub classification: ConfigSettingClass,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationCoverageEntry {
    pub implementation: String,
    pub kind: ComponentKind,
    pub upstream_version: String,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
    pub config_version: String,
    pub config_schema_digest: String,
    pub image: String,
    pub fields: BTreeMap<String, ConfigurationFieldCoverage>,
    pub support: CatalogSupportMatrix,
    pub compatible_dependencies: Vec<CatalogDependencySupport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationCoverageManifest {
    pub api_version: String,
    pub catalog_digest: String,
    pub entries: Vec<ConfigurationCoverageEntry>,
}

/// Generate the exact, machine-readable configuration support matrix.
///
/// # Errors
///
/// Returns a stable diagnostic if a catalog entry lacks a backend contract or
/// if their implementation-scoped configuration versions disagree.
pub fn configuration_coverage_manifest(
    catalog: &CatalogResponse,
    backends: &BackendContractRegistry,
) -> Result<ConfigurationCoverageManifest, String> {
    let mut entries = catalog
        .entries
        .iter()
        .map(|entry| {
            let backend = backends.require(&entry.id)?;
            if backend.config_version != entry.config_version {
                return Err(format!(
                    "coverage_config_version_mismatch: catalog {:?} version {:?} uses {:?}, backend uses {:?}",
                    entry.id, entry.version, entry.config_version, backend.config_version
                ));
            }
            let fields = backend
                .config_fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        ConfigurationFieldCoverage {
                            classification: field.classification,
                            required: field.required,
                        },
                    )
                })
                .collect();
            Ok(ConfigurationCoverageEntry {
                implementation: entry.id.clone(),
                kind: entry.kind,
                upstream_version: entry.version.clone(),
                release_channel: entry.release_channel,
                support_lifecycle: entry.support_lifecycle,
                config_version: entry.config_version.clone(),
                config_schema_digest: entry.config_schema_digest.clone(),
                image: entry.image.clone(),
                fields,
                support: entry.support_matrix.clone(),
                compatible_dependencies: entry.compatible_dependencies.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|left, right| {
        (&left.implementation, &left.upstream_version)
            .cmp(&(&right.implementation, &right.upstream_version))
    });
    Ok(ConfigurationCoverageManifest {
        api_version: CONFIGURATION_COVERAGE_API_VERSION.into(),
        catalog_digest: digest_json(catalog),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        AuthenticationMode, PaymentMethod, StorageBackend, default_backend_registry,
        default_catalog,
    };

    #[test]
    fn manifest_maps_exact_support_and_field_ownership() {
        let manifest =
            configuration_coverage_manifest(&default_catalog(), &default_backend_registry())
                .expect("coverage manifest");
        assert_eq!(manifest.entries.len(), 12);
        let cdk = manifest
            .entries
            .iter()
            .find(|entry| entry.implementation == "cdk")
            .expect("CDK coverage");
        assert_eq!(cdk.upstream_version, "0.17.6");
        assert_eq!(cdk.config_version, "cdk-mintd/0.17/v1");
        assert_eq!(
            cdk.support.storage,
            [StorageBackend::Sqlite, StorageBackend::Postgres].into()
        );
        assert_eq!(cdk.support.payment_methods, [PaymentMethod::Bolt11].into());
        assert_eq!(
            cdk.support.payment_backends,
            ["cln".into(), "lnd".into()].into()
        );
        assert_eq!(cdk.support.units, ["sat".into()].into());
        let payments = cdk
            .support
            .payment_bindings
            .iter()
            .map(|binding| binding.backend.implementation.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(payments, ["cln", "lnd"].into());
        assert_eq!(
            cdk.support.authentication,
            [AuthenticationMode::Unauthenticated].into()
        );
        assert_eq!(
            cdk.fields["mnemonic"].classification,
            ConfigSettingClass::RuntimePolicy
        );
        assert_eq!(
            cdk.support.compatible_wallet_adapters[0].implementation,
            "nutshell-wallet"
        );
        assert!(
            cdk.support.compatible_wallet_adapters[0]
                .versions
                .contains("0.20.3")
        );
        let cdk_ldk = manifest
            .entries
            .iter()
            .find(|entry| entry.implementation == "cdk-ldk")
            .expect("embedded LDK coverage");
        assert_eq!(
            cdk_ldk.support.payment_methods,
            [PaymentMethod::Bolt11, PaymentMethod::Bolt12].into()
        );
        assert_eq!(cdk_ldk.support.embedded_payment_bindings.len(), 2);
        assert!(cdk_ldk.support.payment_bindings.is_empty());
        assert_eq!(
            cdk_ldk.fields["ldk_node_mnemonic"].classification,
            ConfigSettingClass::RuntimePolicy
        );
        let nutshell = manifest
            .entries
            .iter()
            .find(|entry| entry.implementation == "nutshell")
            .expect("Nutshell mint coverage");
        assert_eq!(nutshell.upstream_version, "0.20.3");
        assert_eq!(nutshell.config_version, "nutshell-mint/0.20/v1");
        assert_eq!(
            nutshell.support.storage,
            [StorageBackend::Sqlite, StorageBackend::Postgres].into()
        );
        assert_eq!(
            nutshell.support.payment_backends,
            ["cln".into(), "lnd".into()].into()
        );
        assert_eq!(
            nutshell.fields["clnrest_enable_mpp"].classification,
            ConfigSettingClass::AgentAuthorable
        );
        assert_eq!(
            nutshell.fields["mint_private_key"].classification,
            ConfigSettingClass::GeneratedInstanceSecret
        );
        assert_eq!(
            nutshell.support.authentication,
            [
                AuthenticationMode::Unauthenticated,
                AuthenticationMode::Nut21Clear,
                AuthenticationMode::Nut22Blind,
            ]
            .into()
        );
        assert_eq!(
            nutshell.fields["oidc_discovery_url"].classification,
            ConfigSettingClass::AgentAuthorable
        );
        assert_eq!(
            nutshell.fields["authentication_database"].classification,
            ConfigSettingClass::TopologyDerived
        );
    }

    #[test]
    fn manifest_maps_nutshell_redis_cache_ownership_and_compatibility() {
        let manifest =
            configuration_coverage_manifest(&default_catalog(), &default_backend_registry())
                .expect("coverage manifest");
        let nutshell = manifest
            .entries
            .iter()
            .find(|entry| entry.implementation == "nutshell")
            .expect("Nutshell mint coverage");
        assert_eq!(
            nutshell.fields["redis_cache_ttl_seconds"].classification,
            ConfigSettingClass::AgentAuthorable
        );
        assert_eq!(
            nutshell.fields["redis_cache_credentials"].classification,
            ConfigSettingClass::GeneratedInstanceSecret
        );
        assert!(nutshell.compatible_dependencies.iter().any(|dependency| {
            dependency.link_kind == crate::LinkKind::DatabaseBackend
                && dependency.implementation == "redis"
                && dependency.versions.contains("8.10.1")
        }));
        let redis = manifest
            .entries
            .iter()
            .find(|entry| entry.implementation == "redis")
            .expect("Redis coverage");
        assert_eq!(redis.upstream_version, "8.10.1");
        assert_eq!(redis.config_version, "redis/8.10/v1");
        assert_eq!(redis.support.storage, [StorageBackend::Ephemeral].into());
        assert_eq!(
            redis.fields["credentials"].classification,
            ConfigSettingClass::GeneratedInstanceSecret
        );
    }
}
