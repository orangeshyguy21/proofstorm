use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use crate::{
    BackendContractRegistry, ComponentKind, ComponentSpec, ControlClass, LinkKind, PaymentMethod,
    default_backend_registry,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Prerelease,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SupportLifecycle {
    Preferred,
    Supported,
    Deprecated,
    Experimental,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFeature {
    NativeCli,
    Regtest,
    PersistentState,
    Bolt11,
    Bolt12,
    Onchain,
    ClearAuth,
    BlindAuth,
    Sqlite,
    Postgres,
    WalletOperations,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    Ephemeral,
    PersistentVolume,
    Sqlite,
    Postgres,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMode {
    Unauthenticated,
    Nut21Clear,
    Nut22Blind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogSupportMatrix {
    pub storage: BTreeSet<StorageBackend>,
    pub payment_methods: BTreeSet<PaymentMethod>,
    pub payment_backends: BTreeSet<String>,
    pub units: BTreeSet<String>,
    /// Exact mint payment tuples; the summary sets above are projections, not
    /// permission to assume their Cartesian product is supported.
    pub payment_bindings: BTreeSet<CatalogPaymentBindingSupport>,
    /// Exact payment tuples implemented inside the component image rather
    /// than reached through a topology dependency.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub embedded_payment_bindings: BTreeSet<CatalogEmbeddedPaymentBindingSupport>,
    pub authentication: BTreeSet<AuthenticationMode>,
    pub compatible_wallet_adapters: Vec<CatalogVersionSupport>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEmbeddedPaymentBindingSupport {
    pub method: PaymentMethod,
    pub unit: String,
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogPaymentBindingSupport {
    pub method: PaymentMethod,
    pub unit: String,
    pub backend: CatalogVersionSupport,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogVersionSupport {
    pub implementation: String,
    pub versions: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogDependencySupport {
    pub link_kind: LinkKind,
    pub implementation: String,
    pub versions: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    pub kind: ComponentKind,
    pub description: String,
    pub adapter_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_action_adapter_version: Option<String>,
    pub version: String,
    pub release_channel: ReleaseChannel,
    pub support_lifecycle: SupportLifecycle,
    pub config_version: String,
    pub config_schema: Value,
    pub config_schema_digest: String,
    pub features: BTreeSet<CatalogFeature>,
    pub compatible_dependencies: Vec<CatalogDependencySupport>,
    pub support_matrix: CatalogSupportMatrix,
    pub image: String,
    pub source_digest: String,
    pub allowed_control: Vec<ControlClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogImplementationSupport {
    pub implementation: String,
    pub minimum_supported: String,
    pub preferred_version: String,
    pub supported_versions: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogResponse {
    pub api_version: String,
    pub implementations: Vec<CatalogImplementationSupport>,
    pub entries: Vec<CatalogEntry>,
}

impl CatalogResponse {
    /// Build one internally consistent exact-version catalog.
    ///
    /// Entries for the same implementation are ordered from the minimum
    /// supported version to newer versions. Exactly one must be preferred.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic for duplicate release identities, ambiguous
    /// defaults, mutable images, or a configuration schema whose recorded
    /// digest is stale.
    pub fn try_new(entries: Vec<CatalogEntry>) -> Result<Self, String> {
        let implementations = implementation_support(&entries)?;
        Ok(Self {
            api_version: crate::API_VERSION.to_owned(),
            implementations,
            entries,
        })
    }
}

#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the default catalog deliberately declares every support-contract field inline"
)]
/// Return the built-in, internally validated catalog.
///
/// # Panics
///
/// Panics when a built-in entry violates a catalog invariant. This indicates a
/// programmer error caught by the catalog contract tests.
pub fn default_catalog() -> CatalogResponse {
    let adapter_version = "0.1.0-alpha.1";
    let backends = default_backend_registry();
    let entries = vec![
        catalog_entry(
            "bitcoin-core",
            &backends,
            ComponentKind::Bitcoin,
            "Bitcoin Core regtest node",
            adapter_version,
            "30.0",
            ReleaseChannel::Stable,
            "docker.io/polarlightning/bitcoind@sha256:6b15e7efb79995a18441806f509e40316428a901f1cdc5c54cd25b03ac513cb9",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
            ]),
            vec![],
            support_matrix(
                &[StorageBackend::PersistentVolume],
                &[],
                &[],
                &[],
                &[],
                &[],
                vec![],
            ),
            vec![ControlClass::Laboratory, ControlClass::Attacker],
        ),
        catalog_entry(
            "lnd",
            &backends,
            ComponentKind::Lightning,
            "LND regtest Lightning node",
            adapter_version,
            "0.20.0-beta",
            ReleaseChannel::Prerelease,
            "docker.io/polarlightning/lnd@sha256:ad708a2dacccd6ae104e78577f6a724095b80bac76ddf363f4bf8d22fbe0979f",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
            ]),
            vec![dependency(
                LinkKind::ChainBackend,
                "bitcoin-core",
                &["30.0"],
            )],
            support_matrix(
                &[StorageBackend::PersistentVolume],
                &[PaymentMethod::Bolt11],
                &[],
                &["sat"],
                &[],
                &[],
                vec![],
            ),
            vec![ControlClass::Laboratory, ControlClass::Attacker],
        ),
        catalog_entry(
            "cln",
            &backends,
            ComponentKind::Lightning,
            "Core Lightning regtest node",
            adapter_version,
            "26.06.7",
            ReleaseChannel::Stable,
            "docker.io/elementsproject/lightningd@sha256:f0bd6bf244b815adf1b633bcfff6fc0cf5fd026efefa1367839552f1490f7fbd",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
            ]),
            vec![dependency(
                LinkKind::ChainBackend,
                "bitcoin-core",
                &["30.0"],
            )],
            support_matrix(
                &[StorageBackend::PersistentVolume],
                &[PaymentMethod::Bolt11],
                &[],
                &["sat"],
                &[],
                &[],
                vec![],
            ),
            vec![ControlClass::Laboratory, ControlClass::Attacker],
        ),
        catalog_entry(
            "cdk",
            &backends,
            ComponentKind::Mint,
            "CDK Cashu mint",
            adapter_version,
            "0.17.6",
            ReleaseChannel::Stable,
            "docker.io/cashubtc/mintd@sha256:e6018ad5ed3e9914c7892a53239cf602250e788c1fd7c055d4123803cee8dd00",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
                CatalogFeature::Sqlite,
                CatalogFeature::Postgres,
            ]),
            vec![
                dependency(LinkKind::PaymentBackend, "lnd", &["0.20.0-beta"]),
                dependency(LinkKind::PaymentBackend, "cln", &["26.06.7"]),
                dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
            ],
            support_matrix(
                &[StorageBackend::Sqlite, StorageBackend::Postgres],
                &[PaymentMethod::Bolt11],
                &["cln", "lnd"],
                &["sat"],
                &[
                    payment_binding(PaymentMethod::Bolt11, "sat", "lnd", &["0.20.0-beta"]),
                    payment_binding(PaymentMethod::Bolt11, "sat", "cln", &["26.06.7"]),
                ],
                &[AuthenticationMode::Unauthenticated],
                vec![version_support("nutshell-wallet", &["0.20.2"])],
            ),
            vec![ControlClass::Target],
        ),
        catalog_entry(
            "cdk-ldk",
            &backends,
            ComponentKind::Mint,
            "CDK Cashu mint with embedded LDK Node",
            adapter_version,
            "0.17.6",
            ReleaseChannel::Stable,
            "docker.io/cashubtc/mintd@sha256:418527bb3642a2cfd9091caca9d706b5f7582c5c5923cb852f3fe6c29f587392",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
                CatalogFeature::Bolt12,
                CatalogFeature::Sqlite,
                CatalogFeature::Postgres,
            ]),
            vec![
                dependency(LinkKind::ChainBackend, "bitcoin-core", &["30.0"]),
                dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
            ],
            with_embedded_payment_bindings(
                support_matrix(
                    &[StorageBackend::Sqlite, StorageBackend::Postgres],
                    &[PaymentMethod::Bolt11, PaymentMethod::Bolt12],
                    &["ldk-node"],
                    &["sat"],
                    &[],
                    &[AuthenticationMode::Unauthenticated],
                    vec![version_support("nutshell-wallet", &["0.20.2"])],
                ),
                &[
                    embedded_payment_binding(PaymentMethod::Bolt11, "sat", "ldk-node"),
                    embedded_payment_binding(PaymentMethod::Bolt12, "sat", "ldk-node"),
                ],
            ),
            vec![ControlClass::Target],
        ),
        catalog_entry(
            "cdk-bdk",
            &backends,
            ComponentKind::Mint,
            "CDK Cashu mint with embedded BDK on-chain backend",
            adapter_version,
            "0.17.6",
            ReleaseChannel::Stable,
            "docker.io/cashubtc/mintd@sha256:e6018ad5ed3e9914c7892a53239cf602250e788c1fd7c055d4123803cee8dd00",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Onchain,
                CatalogFeature::Sqlite,
                CatalogFeature::Postgres,
            ]),
            vec![
                dependency(LinkKind::ChainBackend, "bitcoin-core", &["30.0"]),
                dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
            ],
            with_embedded_payment_bindings(
                support_matrix(
                    &[StorageBackend::Sqlite, StorageBackend::Postgres],
                    &[PaymentMethod::Onchain],
                    &["bdk"],
                    &["sat"],
                    &[],
                    &[AuthenticationMode::Unauthenticated],
                    vec![version_support("nutshell-wallet", &["0.20.2"])],
                ),
                &[embedded_payment_binding(
                    PaymentMethod::Onchain,
                    "sat",
                    "bdk",
                )],
            ),
            vec![ControlClass::Target],
        ),
        catalog_entry(
            "postgresql",
            &backends,
            ComponentKind::Database,
            "PostgreSQL database service",
            adapter_version,
            "17.11",
            ReleaseChannel::Stable,
            "docker.io/library/postgres@sha256:18cfe3ef5e6815560c98237d6216d1e5119702fb0f3894c8785dd58b8bbe5d73",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::PersistentState,
                CatalogFeature::Postgres,
            ]),
            vec![],
            support_matrix(
                &[StorageBackend::PersistentVolume],
                &[],
                &[],
                &[],
                &[],
                &[],
                vec![],
            ),
            vec![ControlClass::Laboratory],
        ),
        catalog_entry(
            "nutshell-wallet",
            &backends,
            ComponentKind::Wallet,
            "Persistent Cashu Nutshell wallet workspace",
            adapter_version,
            "0.20.2",
            ReleaseChannel::Stable,
            "docker.io/cashubtc/nutshell@sha256:65e9cbe23aaa1aeb27ce7206fa854a80f39ce8db1c9121eaecfc053a22506574",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
                CatalogFeature::WalletOperations,
            ]),
            vec![],
            support_matrix(
                &[StorageBackend::PersistentVolume],
                &[PaymentMethod::Bolt11],
                &[],
                &["sat"],
                &[],
                &[AuthenticationMode::Unauthenticated],
                vec![],
            ),
            vec![ControlClass::Laboratory, ControlClass::Attacker],
        ),
        catalog_entry(
            "attacker-workspace",
            &backends,
            ComponentKind::Attacker,
            "Disposable adversarial client workspace",
            adapter_version,
            "0.1.0-alpha.1",
            ReleaseChannel::Prerelease,
            "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662",
            BTreeSet::from([CatalogFeature::NativeCli]),
            vec![],
            support_matrix(
                &[StorageBackend::Ephemeral],
                &[],
                &[],
                &[],
                &[],
                &[],
                vec![],
            ),
            vec![ControlClass::Attacker],
        ),
    ];
    CatalogResponse::try_new(entries).expect("default catalog support contracts are valid")
}

fn implementation_support(
    entries: &[CatalogEntry],
) -> Result<Vec<CatalogImplementationSupport>, String> {
    let mut grouped = BTreeMap::<String, Vec<&CatalogEntry>>::new();
    let mut identities = BTreeSet::new();
    for entry in entries {
        if !identities.insert((entry.id.as_str(), entry.version.as_str())) {
            return Err(format!(
                "catalog_version_duplicate: implementation {:?} version {:?} is registered more than once",
                entry.id, entry.version
            ));
        }
        if !is_sha256_image(&entry.image) {
            return Err(format!(
                "catalog_image_not_immutable: implementation {:?} version {:?} image {:?} must end in an exact sha256 digest",
                entry.id, entry.version, entry.image
            ));
        }
        let actual_schema_digest = crate::digest_json(&entry.config_schema);
        if actual_schema_digest != entry.config_schema_digest {
            return Err(format!(
                "catalog_config_schema_digest_mismatch: implementation {:?} version {:?} records {:?}, actual digest is {:?}",
                entry.id, entry.version, entry.config_schema_digest, actual_schema_digest
            ));
        }
        grouped.entry(entry.id.clone()).or_default().push(entry);
    }
    for entry in entries {
        validate_support_matrix(entry, entries)?;
    }
    grouped
        .into_iter()
        .map(|(implementation, entries)| {
            let preferred = entries
                .iter()
                .copied()
                .filter(|entry| entry.support_lifecycle == SupportLifecycle::Preferred)
                .collect::<Vec<_>>();
            if preferred.len() != 1 {
                return Err(format!(
                    "catalog_preferred_version_ambiguous: implementation {implementation:?} has {} preferred versions",
                    preferred.len()
                ));
            }
            let supported_versions = entries
                .iter()
                .map(|entry| entry.version.clone())
                .collect::<BTreeSet<_>>();
            Ok(CatalogImplementationSupport {
                implementation,
                minimum_supported: entries
                    .first()
                    .expect("catalog implementation has an entry")
                    .version
                    .clone(),
                preferred_version: preferred[0].version.clone(),
                supported_versions,
            })
        })
        .collect()
}

fn validate_support_matrix(entry: &CatalogEntry, entries: &[CatalogEntry]) -> Result<(), String> {
    let required_features = entry
        .support_matrix
        .storage
        .iter()
        .filter_map(|storage| match storage {
            StorageBackend::PersistentVolume => Some(CatalogFeature::PersistentState),
            StorageBackend::Sqlite => Some(CatalogFeature::Sqlite),
            StorageBackend::Postgres => Some(CatalogFeature::Postgres),
            StorageBackend::Ephemeral => None,
        })
        .chain(
            entry
                .support_matrix
                .payment_methods
                .iter()
                .map(|method| match method {
                    PaymentMethod::Bolt11 => CatalogFeature::Bolt11,
                    PaymentMethod::Bolt12 => CatalogFeature::Bolt12,
                    PaymentMethod::Onchain => CatalogFeature::Onchain,
                }),
        )
        .chain(
            entry
                .support_matrix
                .authentication
                .iter()
                .filter_map(|mode| match mode {
                    AuthenticationMode::Unauthenticated => None,
                    AuthenticationMode::Nut21Clear => Some(CatalogFeature::ClearAuth),
                    AuthenticationMode::Nut22Blind => Some(CatalogFeature::BlindAuth),
                }),
        );
    for feature in required_features {
        if !entry.features.contains(&feature) {
            return Err(format!(
                "catalog_support_feature_missing: implementation {:?} version {:?} support matrix requires feature {feature:?}",
                entry.id, entry.version
            ));
        }
    }
    for dependency in &entry.compatible_dependencies {
        for version in &dependency.versions {
            if !entries.iter().any(|candidate| {
                candidate.id == dependency.implementation && candidate.version == *version
            }) {
                return Err(format!(
                    "catalog_dependency_version_missing: implementation {:?} version {:?} references unavailable dependency {:?} version {version:?}",
                    entry.id, entry.version, dependency.implementation
                ));
            }
        }
    }
    validate_payment_bindings(entry, entries)?;
    let embedded_backends = entry
        .support_matrix
        .embedded_payment_bindings
        .iter()
        .map(|binding| binding.backend.as_str())
        .collect::<BTreeSet<_>>();
    for backend in &entry.support_matrix.payment_backends {
        if embedded_backends.contains(backend.as_str()) {
            continue;
        }
        if !entry.compatible_dependencies.iter().any(|dependency| {
            dependency.link_kind == LinkKind::PaymentBackend
                && dependency.implementation == *backend
        }) {
            return Err(format!(
                "catalog_payment_backend_dependency_missing: implementation {:?} version {:?} advertises payment backend {backend:?} without a compatible payment dependency",
                entry.id, entry.version
            ));
        }
    }
    for wallet in &entry.support_matrix.compatible_wallet_adapters {
        for version in &wallet.versions {
            if !entries.iter().any(|candidate| {
                candidate.id == wallet.implementation
                    && candidate.version == *version
                    && candidate.kind == ComponentKind::Wallet
            }) {
                return Err(format!(
                    "catalog_wallet_adapter_version_missing: implementation {:?} version {:?} references unavailable wallet {:?} version {version:?}",
                    entry.id, entry.version, wallet.implementation
                ));
            }
        }
    }
    Ok(())
}

fn validate_payment_bindings(entry: &CatalogEntry, entries: &[CatalogEntry]) -> Result<(), String> {
    let bindings = &entry.support_matrix.payment_bindings;
    validate_embedded_payment_bindings(entry)?;
    let embedded_bindings = &entry.support_matrix.embedded_payment_bindings;
    if entry.kind == ComponentKind::Mint {
        let methods = bindings
            .iter()
            .map(|binding| binding.method)
            .chain(embedded_bindings.iter().map(|binding| binding.method))
            .collect::<BTreeSet<_>>();
        let units = bindings
            .iter()
            .map(|binding| binding.unit.clone())
            .chain(embedded_bindings.iter().map(|binding| binding.unit.clone()))
            .collect::<BTreeSet<_>>();
        let backends = bindings
            .iter()
            .map(|binding| binding.backend.implementation.clone())
            .chain(
                embedded_bindings
                    .iter()
                    .map(|binding| binding.backend.clone()),
            )
            .collect::<BTreeSet<_>>();
        if methods != entry.support_matrix.payment_methods
            || units != entry.support_matrix.units
            || backends != entry.support_matrix.payment_backends
        {
            return Err(format!(
                "catalog_payment_binding_projection_mismatch: mint {:?} version {:?} summary sets must exactly project its payment bindings",
                entry.id, entry.version
            ));
        }
    }
    for binding in bindings {
        if binding.backend.versions.is_empty() {
            return Err(format!(
                "catalog_payment_binding_versions_empty: implementation {:?} version {:?} payment binding backend {:?} has no versions",
                entry.id, entry.version, binding.backend.implementation
            ));
        }
        let dependency = entry.compatible_dependencies.iter().find(|dependency| {
            dependency.link_kind == LinkKind::PaymentBackend
                && dependency.implementation == binding.backend.implementation
        });
        if !dependency
            .is_some_and(|dependency| binding.backend.versions.is_subset(&dependency.versions))
        {
            return Err(format!(
                "catalog_payment_binding_dependency_mismatch: implementation {:?} version {:?} binding {:?}/{:?} backend {:?} versions {:?} exceed its compatible payment dependency",
                entry.id,
                entry.version,
                binding.method,
                binding.unit,
                binding.backend.implementation,
                binding.backend.versions
            ));
        }
        for version in &binding.backend.versions {
            let target = entries.iter().find(|candidate| {
                candidate.id == binding.backend.implementation && candidate.version == *version
            });
            if !target.is_some_and(|target| {
                target
                    .support_matrix
                    .payment_methods
                    .contains(&binding.method)
                    && target.support_matrix.units.contains(&binding.unit)
            }) {
                return Err(format!(
                    "catalog_payment_binding_target_unsupported: implementation {:?} version {:?} binding {:?}/{:?} is not supported by backend {:?} version {version:?}",
                    entry.id,
                    entry.version,
                    binding.method,
                    binding.unit,
                    binding.backend.implementation
                ));
            }
        }
    }
    Ok(())
}

fn validate_embedded_payment_bindings(entry: &CatalogEntry) -> Result<(), String> {
    let bindings = &entry.support_matrix.payment_bindings;
    let embedded_bindings = &entry.support_matrix.embedded_payment_bindings;
    if !embedded_bindings.is_empty() && entry.kind != ComponentKind::Mint {
        return Err(format!(
            "catalog_embedded_payment_binding_kind: implementation {:?} version {:?} embeds payment bindings but is not a mint",
            entry.id, entry.version
        ));
    }
    for binding in embedded_bindings {
        if binding.backend.trim().is_empty() {
            return Err(format!(
                "catalog_embedded_payment_binding_backend_empty: implementation {:?} version {:?} has an embedded payment binding without a backend identifier",
                entry.id, entry.version
            ));
        }
        if bindings.iter().any(|external| {
            external.method == binding.method
                && external.unit == binding.unit
                && external.backend.implementation == binding.backend
        }) {
            return Err(format!(
                "catalog_payment_binding_ambiguous: implementation {:?} version {:?} declares {:?}/{:?} through backend {:?} as both embedded and external",
                entry.id, entry.version, binding.method, binding.unit, binding.backend
            ));
        }
    }
    Ok(())
}

fn is_sha256_image(image: &str) -> bool {
    let Some((_, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[allow(
    clippy::too_many_arguments,
    reason = "catalog entries deliberately spell out the complete support contract"
)]
fn catalog_entry(
    id: &str,
    backends: &BackendContractRegistry,
    kind: ComponentKind,
    description: &str,
    adapter_version: &str,
    version: &str,
    release_channel: ReleaseChannel,
    image: &str,
    features: BTreeSet<CatalogFeature>,
    compatible_dependencies: Vec<CatalogDependencySupport>,
    support_matrix: CatalogSupportMatrix,
    allowed_control: Vec<ControlClass>,
) -> CatalogEntry {
    let backend = backends
        .require(id)
        .expect("catalog entry has a registered backend contract");
    let config_version = backend.config_version.as_str();
    let config_schema = backends
        .config_schema(id)
        .expect("catalog entry backend schema is available");
    let config_schema_digest = crate::digest_json(&config_schema);
    CatalogEntry {
        id: id.into(),
        kind,
        description: description.into(),
        adapter_version: adapter_version.into(),
        protocol_action_adapter_version: Some(adapter_version.into()),
        version: version.into(),
        release_channel,
        support_lifecycle: SupportLifecycle::Preferred,
        config_version: config_version.into(),
        config_schema,
        config_schema_digest,
        features,
        compatible_dependencies,
        support_matrix,
        image: image.into(),
        source_digest: crate::digest_json(&(id, version, adapter_version, config_version)),
        allowed_control,
    }
}

fn support_matrix(
    storage: &[StorageBackend],
    payment_methods: &[PaymentMethod],
    payment_backends: &[&str],
    units: &[&str],
    payment_bindings: &[CatalogPaymentBindingSupport],
    authentication: &[AuthenticationMode],
    compatible_wallet_adapters: Vec<CatalogVersionSupport>,
) -> CatalogSupportMatrix {
    CatalogSupportMatrix {
        storage: storage.iter().copied().collect(),
        payment_methods: payment_methods.iter().copied().collect(),
        payment_backends: payment_backends
            .iter()
            .map(|value| (*value).into())
            .collect(),
        units: units.iter().map(|value| (*value).into()).collect(),
        payment_bindings: payment_bindings.iter().cloned().collect(),
        embedded_payment_bindings: BTreeSet::new(),
        authentication: authentication.iter().copied().collect(),
        compatible_wallet_adapters,
    }
}

fn with_embedded_payment_bindings(
    mut matrix: CatalogSupportMatrix,
    bindings: &[CatalogEmbeddedPaymentBindingSupport],
) -> CatalogSupportMatrix {
    matrix.embedded_payment_bindings = bindings.iter().cloned().collect();
    matrix
}

fn embedded_payment_binding(
    method: PaymentMethod,
    unit: &str,
    backend: &str,
) -> CatalogEmbeddedPaymentBindingSupport {
    CatalogEmbeddedPaymentBindingSupport {
        method,
        unit: unit.into(),
        backend: backend.into(),
    }
}

fn payment_binding(
    method: PaymentMethod,
    unit: &str,
    implementation: &str,
    versions: &[&str],
) -> CatalogPaymentBindingSupport {
    CatalogPaymentBindingSupport {
        method,
        unit: unit.into(),
        backend: version_support(implementation, versions),
    }
}

fn version_support(implementation: &str, versions: &[&str]) -> CatalogVersionSupport {
    CatalogVersionSupport {
        implementation: implementation.into(),
        versions: versions.iter().map(|version| (*version).into()).collect(),
    }
}

fn dependency(
    link_kind: LinkKind,
    implementation: &str,
    versions: &[&str],
) -> CatalogDependencySupport {
    CatalogDependencySupport {
        link_kind,
        implementation: implementation.into(),
        versions: versions.iter().map(|version| (*version).into()).collect(),
    }
}

/// Validate adapter configuration against its backend-owned contract.
///
/// # Errors
///
/// Returns an error for unknown fields or values of the wrong JSON type.
pub fn validate_component_config(component: &ComponentSpec) -> Result<(), String> {
    default_backend_registry().validate_component_config(component)
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
    let candidates = catalog
        .entries
        .iter()
        .filter(|entry| entry.id == component.implementation)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(format!(
            "catalog entry {:?} is not installed",
            component.implementation
        ));
    }
    let entry = if let Some(requested) = component.version.as_deref() {
        candidates
            .iter()
            .copied()
            .find(|entry| entry.version == requested)
            .ok_or_else(|| {
                let installed = candidates
                    .iter()
                    .map(|entry| entry.version.as_str())
                    .collect::<Vec<_>>();
                format!(
                    "component {:?} requests version {requested:?}, explicitly supported versions are {installed:?}",
                    component.id
                )
            })?
    } else {
        let preferred = candidates
            .iter()
            .copied()
            .filter(|entry| entry.support_lifecycle == SupportLifecycle::Preferred)
            .collect::<Vec<_>>();
        if preferred.len() != 1 {
            return Err(format!(
                "catalog implementation {:?} has {} preferred versions; exactly one is required when a component omits version",
                component.implementation,
                preferred.len()
            ));
        }
        preferred[0]
    };
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
    if component.config_version != entry.config_version {
        return Err(format!(
            "component {:?} requests configuration version {:?}, installed version {:?} requires {:?}",
            component.id, component.config_version, entry.version, entry.config_version
        ));
    }
    validate_component_config(component)?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_support_summary_is_exact_and_invariants_fail_closed() {
        let catalog = default_catalog();
        assert_eq!(catalog.entries.len(), catalog.implementations.len());
        assert!(catalog.implementations.iter().all(|support| {
            support.minimum_supported == support.preferred_version
                && support.supported_versions.len() == 1
        }));

        let mut duplicate = catalog.entries.clone();
        duplicate.push(duplicate[0].clone());
        assert!(
            CatalogResponse::try_new(duplicate)
                .expect_err("duplicate version")
                .contains("catalog_version_duplicate")
        );

        let mut stale_schema = catalog.entries.clone();
        stale_schema[0].config_schema["properties"]["new_field"] = json!({"type": "string"});
        assert!(
            CatalogResponse::try_new(stale_schema)
                .expect_err("stale schema digest")
                .contains("catalog_config_schema_digest_mismatch")
        );

        let mut mutable_image = catalog.entries.clone();
        mutable_image[0].image = "docker.io/bitcoin/bitcoin:30.0".into();
        assert!(
            CatalogResponse::try_new(mutable_image)
                .expect_err("mutable image")
                .contains("catalog_image_not_immutable")
        );

        let mut unsupported_claim = catalog.entries.clone();
        unsupported_claim
            .iter_mut()
            .find(|entry| entry.id == "cdk")
            .expect("CDK entry")
            .support_matrix
            .payment_methods
            .insert(PaymentMethod::Bolt12);
        assert!(
            CatalogResponse::try_new(unsupported_claim)
                .expect_err("unsupported capability claim")
                .contains("catalog_support_feature_missing")
        );

        let mut false_cross_product = catalog.entries.clone();
        let cdk = false_cross_product
            .iter_mut()
            .find(|entry| entry.id == "cdk")
            .expect("CDK entry");
        cdk.features.insert(CatalogFeature::Bolt12);
        cdk.support_matrix
            .payment_methods
            .insert(PaymentMethod::Bolt12);
        assert!(
            CatalogResponse::try_new(false_cross_product)
                .expect_err("summary sets must not imply an untested cross product")
                .contains("catalog_payment_binding_projection_mismatch")
        );

        let mut unsupported_by_target = catalog.entries.clone();
        let cdk = unsupported_by_target
            .iter_mut()
            .find(|entry| entry.id == "cdk")
            .expect("CDK entry");
        cdk.features.insert(CatalogFeature::Bolt12);
        cdk.support_matrix.payment_methods = [PaymentMethod::Bolt12].into();
        cdk.support_matrix.payment_backends = ["lnd".into()].into();
        let mut binding = cdk
            .support_matrix
            .payment_bindings
            .iter()
            .find(|binding| binding.backend.implementation == "lnd")
            .cloned()
            .expect("CDK payment binding");
        binding.method = PaymentMethod::Bolt12;
        cdk.support_matrix.payment_bindings = [binding].into();
        assert!(
            CatalogResponse::try_new(unsupported_by_target)
                .expect_err("target backend must support the exact tuple")
                .contains("catalog_payment_binding_target_unsupported")
        );

        let mut missing_wallet = catalog.entries.clone();
        missing_wallet
            .iter_mut()
            .find(|entry| entry.id == "cdk")
            .expect("CDK entry")
            .support_matrix
            .compatible_wallet_adapters[0]
            .versions
            .insert("999.0".into());
        assert!(
            CatalogResponse::try_new(missing_wallet)
                .expect_err("unavailable wallet version")
                .contains("catalog_wallet_adapter_version_missing")
        );
    }
}
