// Retained for exact 0.18.0 locks and archived catalog entries.
const LEGACY_CDK_MINT_IMAGE: &str = "proofstorm-registry.localhost:5000/cdk-mint@sha256:6cbed49864bf15139a474b9dbec3248f35f45143f460f51eb97280c24b8a520a";

// Shared 0.18.1 daemon with LDK, BDK and PostgreSQL support.
pub const CDK_MINT_IMAGE: &str = "proofstorm-registry.localhost:5000/cdk-mint@sha256:d0544631da1645457956345b39c5243ae13ae98cfed2529d2bb0c1cc242603df";

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use crate::{
    BackendContractRegistry, CandidateSource, ComponentKind, ComponentSpec, ControlClass, LinkKind,
    PaymentMethod, default_backend_registry,
};

#[path = "processor_catalog.rs"]
mod processor;

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

impl SupportLifecycle {
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Preferred | Self::Supported)
    }
}

/// Catalog origin is independent of release stability or compatibility.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CatalogOrigin {
    BuiltIn,
    Candidate,
}

impl CatalogEntry {
    #[must_use]
    pub const fn origin(&self) -> CatalogOrigin {
        if self.source.is_some() {
            CatalogOrigin::Candidate
        } else {
            CatalogOrigin::BuiltIn
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFeature {
    NativeCli,
    NativeCliEntrypoints,
    MintManagementRpc,
    Regtest,
    PersistentState,
    Bolt11,
    Bolt12,
    Onchain,
    ClearAuth,
    BlindAuth,
    Sqlite,
    Postgres,
    RedisCache,
    OidcProvider,
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

/// A logical runtime endpoint exposed by one catalog component.
///
/// Endpoint and control identifiers intentionally remain open strings. New
/// adapters can therefore extend the runtime contract without growing the MCP
/// tool schema. `component` names the component's primary endpoint; embedded
/// backends use their catalog binding identifier (for example `ldk-node`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogRuntimeEndpoint {
    pub id: String,
    pub kind: String,
    pub controls: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildProvenance {
    pub repository: String,
    pub commit_sha: String,
    pub artifact_url: String,
    pub artifact_sha256: String,
    pub platform: String,
    pub runtime_image: String,
    pub recipe_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_lock_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_image: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transformations: Vec<String>,
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
    /// Runtime operations implemented by the installed driver, independently
    /// from protocol support advertised by the component itself.
    pub runtime_endpoints: Vec<CatalogRuntimeEndpoint>,
    pub image: String,
    pub source_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CandidateSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_provenance: Option<BuildProvenance>,
    pub allowed_control: Vec<ControlClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogImplementationSupport {
    pub implementation: String,
    pub minimum_supported: Option<String>,
    pub preferred_version: Option<String>,
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
    /// supported version to newer versions. Exactly one must be preferred,
    /// except implementations with only experimental entries have no default.
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
/// Return the built-in, internally validated catalog.
///
/// Built once per process: generating a JSON Schema for every entry and
/// digesting each one is far too expensive to repeat per request.
///
/// # Panics
///
/// Panics when a built-in entry violates a catalog invariant. This indicates a
/// programmer error caught by the catalog contract tests.
pub fn default_catalog() -> &'static CatalogResponse {
    static CATALOG: std::sync::LazyLock<CatalogResponse> =
        std::sync::LazyLock::new(|| build_default_catalog(crate::wallet_builds::LINUX_AMD64));
    &CATALOG
}

/// Container platform used to select published wallet images and provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogPlatform {
    LinuxArm64,
    LinuxAmd64,
}

/// Build a catalog for an explicit platform, independent of the build host.
///
/// This lets maintainers generate and test both coverage contracts on either host.
/// Runtime callers should continue using [`default_catalog`].
///
/// # Panics
///
/// Panics if a built-in entry violates a catalog invariant.
#[must_use]
pub fn catalog_for_platform(platform: CatalogPlatform) -> CatalogResponse {
    build_default_catalog(platform == CatalogPlatform::LinuxAmd64)
}

#[allow(
    clippy::too_many_lines,
    reason = "the default catalog deliberately declares every support-contract field inline"
)]
fn build_default_catalog(amd64: bool) -> CatalogResponse {
    let adapter_version = "0.1.0-alpha.1";
    let backends = default_backend_registry();
    let mut entries = vec![
        catalog_entry(
            amd64,
            "bitcoin-core",
            backends,
            ComponentKind::Bitcoin,
            "Bitcoin Core regtest node",
            adapter_version,
            "31.1",
            ReleaseChannel::Stable,
            "proofstorm-registry.localhost:5000/bitcoin-core@sha256:b3faffac8d3414faf57df61889c354d15fb2851782c2002251ea8df5be358ad6",
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
            vec![ControlClass::Cell, ControlClass::Attacker],
        ),
        catalog_entry_with_lifecycle(
            amd64,
            "lnd",
            backends,
            ComponentKind::Lightning,
            "LND regtest Lightning node",
            adapter_version,
            "0.20.4-beta",
            ReleaseChannel::Prerelease,
            SupportLifecycle::Supported,
            "docker.io/lightninglabs/lnd@sha256:4d6e02cb80ea48db2ef011823bcb2087d4379b70e2797fc7f6e857b32d5d7a09",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
            ]),
            vec![dependency(
                LinkKind::ChainBackend,
                "bitcoin-core",
                &["31.1"],
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
            vec![ControlClass::Cell, ControlClass::Attacker],
        ),
        catalog_entry_with_lifecycle(
            amd64,
            "lnd",
            backends,
            ComponentKind::Lightning,
            "LND regtest Lightning node",
            adapter_version,
            "0.21.3-beta",
            ReleaseChannel::Prerelease,
            SupportLifecycle::Preferred,
            "docker.io/lightninglabs/lnd@sha256:d29074335f3bffb2ac0e789b0d023c24fbb85ce67ecbfb7d677399842fe0535c",
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
            ]),
            vec![dependency(
                LinkKind::ChainBackend,
                "bitcoin-core",
                &["31.1"],
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
            vec![ControlClass::Cell, ControlClass::Attacker],
        ),
        catalog_entry(
            amd64,
            "cln",
            backends,
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
                &["31.1"],
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
            vec![ControlClass::Cell, ControlClass::Attacker],
        ),
        catalog_entry(
            amd64,
            "cdk",
            backends,
            ComponentKind::Mint,
            "CDK Cashu mint",
            adapter_version,
            "0.18.0",
            ReleaseChannel::Stable,
            LEGACY_CDK_MINT_IMAGE,
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
                CatalogFeature::Sqlite,
                CatalogFeature::Postgres,
            ]),
            vec![
                dependency(
                    LinkKind::PaymentBackend,
                    "lnd",
                    &["0.20.4-beta", "0.21.3-beta"],
                ),
                dependency(LinkKind::PaymentBackend, "cln", &["26.06.7"]),
                dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
            ],
            support_matrix(
                &[StorageBackend::Sqlite, StorageBackend::Postgres],
                &[PaymentMethod::Bolt11],
                &["cln", "lnd"],
                &["sat"],
                &[
                    payment_binding(
                        PaymentMethod::Bolt11,
                        "sat",
                        "lnd",
                        &["0.20.4-beta", "0.21.3-beta"],
                    ),
                    payment_binding(PaymentMethod::Bolt11, "sat", "cln", &["26.06.7"]),
                ],
                &[AuthenticationMode::Unauthenticated],
                vec![version_support("nutshell-wallet", &["0.20.3"])],
            ),
            vec![ControlClass::Target],
        ),
        catalog_entry(
            amd64,
            "cdk-ldk",
            backends,
            ComponentKind::Mint,
            "CDK Cashu mint with embedded LDK Node",
            adapter_version,
            "0.18.0",
            ReleaseChannel::Stable,
            LEGACY_CDK_MINT_IMAGE,
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
                dependency(LinkKind::ChainBackend, "bitcoin-core", &["31.1"]),
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
                    vec![version_support("nutshell-wallet", &["0.20.3"])],
                ),
                &[
                    embedded_payment_binding(PaymentMethod::Bolt11, "sat", "ldk-node"),
                    embedded_payment_binding(PaymentMethod::Bolt12, "sat", "ldk-node"),
                ],
            ),
            vec![ControlClass::Target],
        ),
        catalog_entry(
            amd64,
            "cdk-bdk",
            backends,
            ComponentKind::Mint,
            "CDK Cashu mint with embedded BDK on-chain backend",
            adapter_version,
            "0.18.0",
            ReleaseChannel::Stable,
            LEGACY_CDK_MINT_IMAGE,
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Onchain,
                CatalogFeature::Sqlite,
                CatalogFeature::Postgres,
            ]),
            vec![
                dependency(LinkKind::ChainBackend, "bitcoin-core", &["31.1"]),
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
                    vec![version_support("nutshell-wallet", &["0.20.3"])],
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
            amd64,
            "nutshell",
            backends,
            ComponentKind::Mint,
            "Nutshell Cashu mint",
            adapter_version,
            "0.20.3",
            ReleaseChannel::Stable,
            crate::wallet_builds::nutshell(amd64),
            BTreeSet::from([
                CatalogFeature::NativeCli,
                CatalogFeature::Regtest,
                CatalogFeature::PersistentState,
                CatalogFeature::Bolt11,
                CatalogFeature::ClearAuth,
                CatalogFeature::BlindAuth,
                CatalogFeature::Sqlite,
                CatalogFeature::Postgres,
                CatalogFeature::RedisCache,
            ]),
            vec![
                dependency(
                    LinkKind::PaymentBackend,
                    "lnd",
                    &["0.20.4-beta", "0.21.3-beta"],
                ),
                dependency(LinkKind::PaymentBackend, "cln", &["26.06.7"]),
                dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
                dependency(LinkKind::DatabaseBackend, "redis", &["8.10.1"]),
                dependency(LinkKind::AuthenticationBackend, "keycloak", &["25.0.6"]),
            ],
            support_matrix(
                &[StorageBackend::Sqlite, StorageBackend::Postgres],
                &[PaymentMethod::Bolt11],
                &["cln", "lnd"],
                &["sat"],
                &[
                    payment_binding(PaymentMethod::Bolt11, "sat", "cln", &["26.06.7"]),
                    payment_binding(
                        PaymentMethod::Bolt11,
                        "sat",
                        "lnd",
                        &["0.20.4-beta", "0.21.3-beta"],
                    ),
                ],
                &[
                    AuthenticationMode::Unauthenticated,
                    AuthenticationMode::Nut21Clear,
                    AuthenticationMode::Nut22Blind,
                ],
                vec![version_support("nutshell-wallet", &["0.20.3"])],
            ),
            vec![ControlClass::Target],
        ),
        catalog_entry(
            amd64,
            "keycloak",
            backends,
            ComponentKind::IdentityProvider,
            "Disposable OpenID Connect provider for NUT-21 authentication",
            adapter_version,
            "25.0.6",
            ReleaseChannel::Stable,
            "quay.io/keycloak/keycloak@sha256:82c5b7a110456dbd42b86ea572e728878549954cc8bd03cd65410d75328095d2",
            BTreeSet::from([CatalogFeature::OidcProvider]),
            vec![dependency(
                LinkKind::DatabaseBackend,
                "postgresql",
                &["17.11"],
            )],
            support_matrix(&[], &[], &[], &[], &[], &[], vec![]),
            vec![ControlClass::Cell],
        ),
        catalog_entry(
            amd64,
            "redis",
            backends,
            ComponentKind::Database,
            "Redis cache service",
            adapter_version,
            "8.10.1",
            ReleaseChannel::Stable,
            "docker.io/library/redis@sha256:becdda6c7f4b3fb42e42fd7f120bbf5c54c4caaaf16f26da24e4563d2c1f0576",
            BTreeSet::from([CatalogFeature::NativeCli, CatalogFeature::RedisCache]),
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
            vec![ControlClass::Cell],
        ),
        catalog_entry(
            amd64,
            "postgresql",
            backends,
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
            vec![ControlClass::Cell],
        ),
        catalog_entry(
            amd64,
            "nutshell-wallet",
            backends,
            ComponentKind::Wallet,
            "Persistent Cashu Nutshell wallet workspace",
            adapter_version,
            "0.20.3",
            ReleaseChannel::Stable,
            crate::wallet_builds::nutshell(amd64),
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
            vec![ControlClass::Cell, ControlClass::Attacker],
        ),
        cdk_cli_wallet_entry(amd64, backends, adapter_version),
        cocod_wallet_entry(amd64, backends, adapter_version),
        catalog_entry(
            amd64,
            "workspace",
            backends,
            ComponentKind::Attacker,
            "Persistent programmable workspace with managed tasks, source snapshots, rotating logs and optional custom runtime",
            adapter_version,
            "0.1.0-alpha.1",
            ReleaseChannel::Prerelease,
            "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662",
            BTreeSet::from([CatalogFeature::NativeCli, CatalogFeature::PersistentState]),
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
            vec![ControlClass::Attacker],
        ),
    ];
    for entry in &mut entries {
        if matches!(entry.id.as_str(), "nutshell" | "nutshell-wallet") {
            entry.features.insert(CatalogFeature::NativeCliEntrypoints);
        }
        if matches!(
            entry.id.as_str(),
            "cdk" | "cdk-ldk" | "cdk-bdk" | "nutshell"
        ) {
            entry.features.insert(CatalogFeature::MintManagementRpc);
        }
        let encoded = match entry.id.as_str() {
            "bitcoin-core" => include_str!("../../../docker/bitcoin/bitcoin-31.1-provenance.json"),
            "cdk" | "cdk-bdk" | "cdk-ldk" => {
                include_str!("../../../docker/mint/cdk-ldk-management-provenance.json")
            }
            _ => continue,
        };
        let provenance: BuildProvenance =
            serde_json::from_str(encoded).expect("pinned mint management build provenance");
        entry.source_digest = crate::digest_json(&(&entry.source_digest, &provenance));
        entry.build_provenance = Some(provenance);
    }
    promote_component_releases(&mut entries, amd64);
    processor::extend(&mut entries, amd64, backends, adapter_version);
    CatalogResponse::try_new(entries).expect("default catalog support contracts are valid")
}

/// Keep the previous exact entries available for saved locks while new cells
/// select the current patch and the newest Nutshell release family.
fn promote_component_releases(entries: &mut Vec<CatalogEntry>, amd64: bool) {
    let mut previous = Vec::new();
    for entry in entries.iter_mut() {
        let (version, image, encoded, lifecycle) = match entry.id.as_str() {
            "cdk" | "cdk-ldk" | "cdk-bdk" => (
                "0.18.1",
                CDK_MINT_IMAGE,
                include_str!("../../../docker/mint/cdk-0.18.1-provenance.json"),
                SupportLifecycle::Deprecated,
            ),
            "cdk-cli-wallet" => {
                let (image, provenance) = crate::wallet_builds::cdk_0181(amd64);
                ("0.18.1", image, provenance, SupportLifecycle::Deprecated)
            }
            "nutshell" | "nutshell-wallet" => (
                "0.21.0",
                crate::wallet_builds::nutshell_021(amd64),
                include_str!("../../../docker/mint/nutshell-0.21.0-provenance.json"),
                SupportLifecycle::Supported,
            ),
            _ => continue,
        };
        let mut old = entry.clone();
        old.support_lifecycle = lifecycle;
        previous.push(old);
        entry.version = version.into();
        entry.description = entry.description.replace("0.18.0", version);
        entry.image = image.into();
        if entry.id == "nutshell" {
            entry.protocol_action_adapter_version = Some("nutshell-mint/0.21/v1".into());
        }
        if entry.kind == ComponentKind::Mint {
            entry.support_matrix.compatible_wallet_adapters =
                vec![version_support("nutshell-wallet", &["0.20.3", "0.21.0"])];
        }
        let provenance: BuildProvenance =
            serde_json::from_str(encoded).expect("promoted release build provenance");
        entry.source_digest = crate::digest_json(&(
            &entry.source_digest,
            version,
            &entry.protocol_action_adapter_version,
            &provenance,
        ));
        entry.build_provenance = Some(provenance);
    }
    entries.extend(previous);
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
            if preferred.len() > 1 || (preferred.is_empty() && entries.iter().any(|entry| entry.support_lifecycle.is_supported())) {
                return Err(format!(
                    "catalog_preferred_version_ambiguous: implementation {implementation:?} has {} preferred versions",
                    preferred.len()
                ));
            }
            let supported = entries
                .iter()
                .copied()
                .filter(|entry| entry.support_lifecycle.is_supported())
                .collect::<Vec<_>>();
            let policy = crate::release_policy::release_policy(&implementation);
            let mut families = BTreeSet::new();
            for entry in &supported {
                if let Some(policy) = policy {
                    let version = policy.parse(&entry.version).filter(|version| policy.is_eligible(*version)).ok_or_else(|| format!(
                        "catalog_release_ineligible: {implementation:?} version {:?} is not an eligible release", entry.version
                    ))?;
                    if !families.insert(policy.family(version)) {
                        return Err(format!("catalog_release_family_duplicate: {implementation:?} has multiple supported patches in one family"));
                    }
                    if families.len() > policy.families {
                        return Err(format!("catalog_support_window_exceeded: {implementation:?} supports at most {} release families", policy.families));
                    }
                }
            }
            let minimum = supported.iter().min_by(|left, right| {
                policy.map_or_else(
                    || left.version.cmp(&right.version),
                    |policy| policy.parse(&left.version).cmp(&policy.parse(&right.version)),
                )
            });
            let supported_versions = supported.iter()
                .map(|entry| entry.version.clone())
                .collect::<BTreeSet<_>>();
            Ok(CatalogImplementationSupport {
                implementation,
                minimum_supported: minimum.map(|entry| entry.version.clone()),
                preferred_version: preferred.first().map(|entry| entry.version.clone()),
                supported_versions,
            })
        })
        .collect()
}

fn validate_support_matrix(entry: &CatalogEntry, entries: &[CatalogEntry]) -> Result<(), String> {
    validate_runtime_endpoints(entry)?;
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

fn validate_runtime_endpoints(entry: &CatalogEntry) -> Result<(), String> {
    let mut endpoint_ids = BTreeSet::new();
    for endpoint in &entry.runtime_endpoints {
        if endpoint.id.trim().is_empty() || endpoint.kind.trim().is_empty() {
            return Err(format!(
                "catalog_runtime_endpoint_invalid: implementation {:?} version {:?} has an empty endpoint id or kind",
                entry.id, entry.version
            ));
        }
        if !endpoint_ids.insert(endpoint.id.as_str()) {
            return Err(format!(
                "catalog_runtime_endpoint_duplicate: implementation {:?} version {:?} repeats endpoint {:?}",
                entry.id, entry.version, endpoint.id
            ));
        }
        if endpoint
            .controls
            .iter()
            .any(|control| control.trim().is_empty())
        {
            return Err(format!(
                "catalog_runtime_control_invalid: implementation {:?} version {:?} endpoint {:?} has an empty control identifier",
                entry.id, entry.version, endpoint.id
            ));
        }
    }
    for binding in &entry.support_matrix.embedded_payment_bindings {
        if !endpoint_ids.contains(binding.backend.as_str()) {
            return Err(format!(
                "catalog_embedded_runtime_endpoint_missing: implementation {:?} version {:?} embeds backend {:?} without runtime endpoint metadata",
                entry.id, entry.version, binding.backend
            ));
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
    amd64: bool,
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
    catalog_entry_with_lifecycle(
        amd64,
        id,
        backends,
        kind,
        description,
        adapter_version,
        version,
        release_channel,
        SupportLifecycle::Preferred,
        image,
        features,
        compatible_dependencies,
        support_matrix,
        allowed_control,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "catalog entries deliberately spell out the complete support contract"
)]
fn catalog_entry_with_lifecycle(
    amd64: bool,
    id: &str,
    backends: &BackendContractRegistry,
    kind: ComponentKind,
    description: &str,
    adapter_version: &str,
    version: &str,
    release_channel: ReleaseChannel,
    support_lifecycle: SupportLifecycle,
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
    let runtime_endpoints = catalog_runtime_endpoints(id, amd64);
    CatalogEntry {
        id: id.into(),
        kind,
        description: description.into(),
        adapter_version: adapter_version.into(),
        protocol_action_adapter_version: Some(adapter_version.into()),
        version: version.into(),
        release_channel,
        support_lifecycle,
        config_version: config_version.into(),
        config_schema,
        config_schema_digest,
        features,
        compatible_dependencies,
        support_matrix,
        runtime_endpoints: runtime_endpoints.clone(),
        image: mirror_image(image),
        source_digest: crate::digest_json(&(
            id,
            version,
            adapter_version,
            config_version,
            &runtime_endpoints,
        )),
        source: None,
        build_provenance: None,
        allowed_control,
    }
}

/// Preserve the upstream repository and digest while serving published images
/// from the local registry. Locally packaged images already name that registry.
pub(crate) fn mirror_image(image: &str) -> String {
    if image.starts_with("docker.io/") || image.starts_with("quay.io/") {
        format!("proofstorm-registry.localhost:5000/upstream/{image}")
    } else {
        image.into()
    }
}

fn cdk_cli_wallet_entry(
    amd64: bool,
    backends: &BackendContractRegistry,
    adapter_version: &str,
) -> CatalogEntry {
    let (image, encoded) = crate::wallet_builds::cdk(amd64);
    let mut entry = catalog_entry(
        amd64,
        "cdk-cli-wallet",
        backends,
        ComponentKind::Wallet,
        if amd64 {
            "CDK CLI 0.18.0 persistent wallet; Linux amd64 cell build"
        } else {
            "CDK CLI 0.18.0 persistent wallet; initial Linux arm64 cell build"
        },
        adapter_version,
        "0.18.0",
        ReleaseChannel::Stable,
        image,
        BTreeSet::from([
            CatalogFeature::NativeCli,
            CatalogFeature::PersistentState,
            CatalogFeature::Sqlite,
            CatalogFeature::Bolt11,
        ]),
        vec![],
        support_matrix(
            &[StorageBackend::PersistentVolume, StorageBackend::Sqlite],
            &[PaymentMethod::Bolt11],
            &[],
            &["sat"],
            &[],
            &[AuthenticationMode::Unauthenticated],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Attacker],
    );
    entry.protocol_action_adapter_version = Some("cdk-cli/0.18/observations/v1".into());
    let provenance: BuildProvenance =
        serde_json::from_str(encoded).expect("pinned wallet build provenance");
    entry.source_digest = crate::digest_json(&(&entry.source_digest, &provenance));
    entry.build_provenance = Some(provenance);
    entry
}

fn cocod_wallet_entry(
    amd64: bool,
    backends: &BackendContractRegistry,
    adapter_version: &str,
) -> CatalogEntry {
    let (image, encoded) = crate::wallet_builds::cocod(amd64);
    let mut entry = catalog_entry(
        amd64,
        "cocod-wallet",
        backends,
        ComponentKind::Wallet,
        if amd64 {
            "Unreleased cocod daemon from Coco 44e5101c; experimental Linux amd64 cell build"
        } else {
            "Unreleased cocod daemon from Coco 44e5101c; experimental Linux arm64 cell build"
        },
        adapter_version,
        "0.0.17-dev.44e5101c",
        ReleaseChannel::Prerelease,
        image,
        BTreeSet::from([
            CatalogFeature::NativeCli,
            CatalogFeature::PersistentState,
            CatalogFeature::Sqlite,
            CatalogFeature::Bolt11,
        ]),
        vec![],
        support_matrix(
            &[StorageBackend::PersistentVolume, StorageBackend::Sqlite],
            &[PaymentMethod::Bolt11],
            &[],
            &["sat"],
            &[],
            &[AuthenticationMode::Unauthenticated],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Attacker],
    );
    entry.support_lifecycle = SupportLifecycle::Experimental;
    entry.protocol_action_adapter_version = Some("cocod/44e5101c/observations/v1".into());
    let provenance: BuildProvenance =
        serde_json::from_str(encoded).expect("pinned wallet build provenance");
    entry.source_digest = crate::digest_json(&(&entry.source_digest, &provenance));
    entry.build_provenance = Some(provenance);
    entry
}

fn runtime_endpoint(
    id: &str,
    kind: &str,
    controls: &[&str],
    limitations: &[&str],
) -> CatalogRuntimeEndpoint {
    let mut controls = controls
        .iter()
        .map(|control| (*control).to_owned())
        .collect::<BTreeSet<_>>();
    // These controls operate against the primary workload itself and are
    // intentionally implementation-agnostic. New component implementations
    // inherit them without growing a new MCP surface.
    if id == "component" {
        controls.extend(
            [
                "component_exec_live",
                "component_forensics",
                "component_start",
                "component_stop",
                "component_restart",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    CatalogRuntimeEndpoint {
        id: id.into(),
        kind: kind.into(),
        controls,
        limitations: limitations
            .iter()
            .map(|limitation| (*limitation).into())
            .collect(),
    }
}

/// Installed driver registry. Protocol support and runtime controllability are
/// deliberately separate: an image can implement Lightning while its current
/// Proofstorm driver does not yet expose peer/channel controls.
#[allow(
    clippy::too_many_lines,
    reason = "the runtime registry explicitly declares each installed driver's complete control surface"
)]
fn catalog_runtime_endpoints(implementation: &str, amd64: bool) -> Vec<CatalogRuntimeEndpoint> {
    const OBSERVE: &[&str] = &["component_logs", "reachability_oracle"];
    const CDK_MANAGEMENT: &str = "Management RPC is always enabled on pod loopback with per-mint mutual TLS. Native entrypoint: cdk-mint-cli --addr https://127.0.0.1:8086 --work-dir /management-client get-info; use --help for native commands. Client certificates are mounted in /management-client/tls; never copy their contents into arguments or public output. Invoke through component_exec_live, not forensics. Durable RPC changes survive ordinary restarts; a changed authored cell configuration is applied on the next rollout. Mint quote payment override is disabled by the upstream server policy. CLI success is not proof of the intended state: verify the result independently. Management images support Linux amd64 and arm64.";
    const NUTSHELL_MANAGEMENT: &str = "Management RPC is always enabled on pod loopback with per-mint mutual TLS. Native entrypoint: mint-cli --host 127.0.0.1 --port 8086 --ca-cert-path /management-client/tls/ca.pem --client-cert-path /management-client/tls/client.pem --client-key-path /management-client/tls/client.key get-info; use --help for native commands. Invoke through component_exec_live, not forensics. Never copy credentials into arguments or public output. Nutshell 0.20.3 can print RPC errors while exiting zero: verify state independently. Metadata/settings mutations can be process-local and reset from authored configuration on restart; persistent keyset/quote changes follow upstream database semantics. Management images support Linux amd64 and arm64.";
    if let Some(endpoints) = processor::endpoints(implementation) {
        return endpoints;
    }
    let mut endpoints = match implementation {
        "bitcoin-core" => vec![runtime_endpoint(
            "component",
            "bitcoin",
            &[
                "chain_mine",
                "component_logs",
                "node_restart",
                "reachability_oracle",
            ],
            &[
                "live bitcoin-cli requires -regtest -rpcconnect=127.0.0.1 -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only",
            ],
        )],
        "lnd" => vec![runtime_endpoint(
            "component",
            "lightning",
            &["component_logs", "node_restart", "reachability_oracle"],
            &[
                "live lncli uses --network=regtest --lnddir=/home/lnd/.lnd. Native addinvoice --amt=<sat> uses output mode lnd_invoice: validated payment_request/payment_hash plus amount_msat, currency, expires_at_unix; raw output stays private. Pay with payinvoice --force --json and json_fields status,value_sat; lookupinvoice --rhash <payment_hash> with json_fields state,settled. Require successful native exit and projection, verify intended amount/network/expiry before payment; do not grep invoice output.",
            ],
        )],
        "cln" => vec![runtime_endpoint(
            "component",
            "lightning",
            &["component_logs", "node_restart", "reachability_oracle"],
            &["live lightning-cli uses --network=regtest --lightning-dir=/home/cln/.lightning"],
        )],
        "cdk" | "nutshell" => vec![runtime_endpoint(
            "component",
            "mint",
            &["component_logs", "reachability_oracle"],
            &[if implementation == "cdk" {
                CDK_MANAGEMENT
            } else {
                NUTSHELL_MANAGEMENT
            }],
        )],
        "cdk-ldk" => vec![
            runtime_endpoint(
                "component",
                "mint",
                &["component_logs", "reachability_oracle"],
                &[CDK_MANAGEMENT],
            ),
            runtime_endpoint(
                "ldk-node",
                "lightning",
                &[],
                &[
                    "the embedded LDK backend supports payments but its installed driver does not yet expose peer or channel controls",
                ],
            ),
        ],
        "cdk-bdk" => vec![
            runtime_endpoint("component", "mint", OBSERVE, &[CDK_MANAGEMENT]),
            runtime_endpoint(
                "bdk",
                "onchain",
                &[],
                &["the embedded BDK backend has no direct runtime controls"],
            ),
        ],
        "cocod-wallet" => vec![runtime_endpoint(
            "component",
            "wallet",
            &[],
            &[
                "Experimental commit pin; no default version. Native cocod CLI and authenticated loopback HTTP are the mutation surface.",
                "COCOD_URL=http://127.0.0.1:62626 makes clients strictly client-only. Daemon runs in foreground under native exclusive state lease. Never start another daemon in a Job or forensics pod.",
                "HOME=/wallet; private state /wallet/.cocod; credentials/current/client contains the administrative bearer. Initialization/recovery output includes mnemonic: use private execution output.",
                "This pin's initialize CLI/API cannot select a mint. Initialize with a private passphrase (keeps session stopped), configure mintUrl in its native config.json while the protected session is stopped, restart the component, then explicitly start the protected session. Never use its public default mint in a cell.",
                "Read catalog and cocod subcommand help first. Prefer direct private payment invocation and independent recipient settlement plus passive balances. No invented parser defaults; failure is not rollback.",
                "For a passive observation, execute /opt/proofstorm/driver observe cocod-wallet through cell_exec with PROOFSTORM_DATABASE=/wallet/.cocod/coco.db, PROOFSTORM_WALLET=<component>, PROOFSTORM_MINT=<mint-component>, and PROOFSTORM_MINT_URL=<mint-url>. This reads exact mint/sat proof state in a read-only SQLite transaction. balance_sat is unreserved ready proofs; reserved_sat is reserved ready proofs; inflight_sat is a distinct local category. The native /balance endpoint returns ready total, including reservations. Neither is a mint-side proof-state oracle.",
                "Health means process reachability, not initialization or running session. Protected sessions remain stopped across restart. NPC external traffic is blocked by the cell network policy; NPC is outside this checkpoint.",
                "Observe native status directly with argv [cocod,status] and json_fields selecting seedAccess.state, seedAccess.requiresPassphrase, cocoSession.state. Fixed enums/booleans are validated; null seedAccess produces null leaves for an uninitialized wallet. Use argv [cocod,health] with json_fields field status. No raw status/error output or custom status parser is needed. API reference: /opt/coco/packages/cocod/docs/API.md; structured recovery response: private mnemonic field.",
                "Use native cocod receive bolt11 <sat> with output mode bolt11. It validates the entire invoice-only response and exposes payment_request/payment_hash, amount_msat, currency, expires_at_unix; raw streams stay private. Check exit_code 0 and projection_succeeded, then intended amount/network/expiry before relaying payment_request as a separate native payer argument. Do not grep invoice output. This public invoice projection is not for spendable Cashu tokens.",
            ],
        )],
        "cdk-cli-wallet" => vec![runtime_endpoint(
            "component",
            "wallet",
            &["component_logs"],
            &[
                "Native entrypoint: cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive --help. Set --work-dir /wallet/cdk on EVERY native invocation. SQLite and sat are the installed observation contract. Native commands, including balance, may recover incomplete sagas; use cell_exec with /opt/proofstorm/driver observe cdk-cli-wallet for passive wallet-local observations. Set PROOFSTORM_DATABASE=/wallet/cdk/cdk-cli.sqlite, PROOFSTORM_WALLET=<component>, PROOFSTORM_MINT=<mint-component>, and PROOFSTORM_MINT_URL=<mint-url>. Initialize with the native balance command before observing an empty database. A failed melt is not a rollback: completed preparation swaps can charge input fees even when payment fails. Reconcile passive balances and quote/recipient state before a new attempt. Reuse the same operation ID and idempotency key to retrieve an existing execution, not a new CLI mutation. Use native mint/melt commands. In this pinned release, mint-pending checks pending proofs, not paid mint-quote issuance despite its help text. Resume a paid quote with mint <url> --quote-id <id>, then verify the passive balance; command success alone does not prove issuance. Typed wallet mutations, quote recovery and conservation are unavailable. Initial image is Linux arm64 only. Mint compatibility requires live evidence for each claimed combination.",
            ],
        )],
        "nutshell-wallet" => vec![runtime_endpoint(
            "component",
            "wallet",
            &["component_logs"],
            &[
                "Native Nutshell CLI entrypoint: export HOME=/wallet; cd /app; cashu -w wallet --help. Use the default internal name wallet (-w wallet) and set the mint URL explicitly on every command. Proofstorm component IDs such as alice and bob identify separate persistent volumes, not Nutshell wallet names. All native and typed operations use /wallet/.cashu/wallet/wallet.sqlite3. Named wallets are unsupported in this pinned release because receive and balance disagree about their database paths. Existing named-wallet state is not migrated automatically; recover it separately before replacing a validation cell. Use cashu -w wallet -h <mint-url> balance to read the native balance. Wallet-local fee_paid uses legacy accounting, not authoritative Lightning fees; inspect mint/backend evidence. Account separately for input fees, including preparatory swaps",
            ],
        )],
        "keycloak" => vec![runtime_endpoint(
            "component",
            "identity_provider",
            &[
                "authentication_conformance",
                "authentication_protected_spend",
                "authentication_replay",
                "component_logs",
            ],
            &[],
        )],
        "workspace" => vec![runtime_endpoint(
            "component",
            "workspace",
            &[
                "workspace_task",
                "workspace_file",
                "workspace_upload",
                "workspace_capture",
                "component_logs",
            ],
            &[
                "Use workspace_upload to copy local scripts or binary files up to 16 MiB into /workspace without inline contents. Use workspace_file for small edits, reads and listing, and workspace_task for managed background tasks. Each task captures its source directory (default src), survives agent disconnection and never automatically replays after restart. Stop through workspace_task. Optional task control.components grants native command calls on named cell components: invoke $PROOFSTORM_CONTROL workspace call with JSON {call_id,component,command}; command uses the cell_exec contract. Reuse call_id only for an exact retry. Receipts are under output/<task_id>/control/<call_id>.json. Scope defaults to 256 calls and 30 seconds per command. control.lifecycle grants start/stop/restart targets; control.network grants exact partition pairs with max_fault_seconds (default 60, maximum 3600). Typed calls use {call_id,operation:{kind,...}}. Partitions expire and are healed on task exit; inspect task control_cleanup after stopping. Lifecycle effects persist. Use workspace_capture to attach source, inputs, selected output files and control receipts to an open run without stopping the task. Evidence exports include captures; download the resource before cell teardown. Runtime defaults to BusyBox; a pinned runtime_image supplies other languages. An optional service_port exposes one cell-local TCP endpoint.",
            ],
        )],
        "redis" | "postgresql" => {
            vec![runtime_endpoint("component", "service", OBSERVE, &[])]
        }
        _ => vec![runtime_endpoint(
            "component",
            "component",
            OBSERVE,
            &["no specialized runtime driver controls are registered"],
        )],
    };
    if amd64 {
        for endpoint in &mut endpoints {
            for limitation in &mut endpoint.limitations {
                *limitation = limitation.replace(
                    "Initial image is Linux arm64 only.",
                    "Packaged image is Linux amd64.",
                );
            }
        }
    }
    endpoints
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

/// Check new-cell admission separately from resolving historical locked cells.
/// Experimental candidates still require explicit selection through the catalog.
/// # Errors
/// Refuses retired exact versions with the current supported alternatives.
pub fn validate_new_cell_versions(
    cell: &crate::CellSpec,
    catalog: &CatalogResponse,
) -> Result<(), String> {
    for component in &cell.components {
        let entry = validate_catalog_component(component, catalog)?;
        if entry.support_lifecycle == SupportLifecycle::Deprecated {
            let supported = catalog
                .implementations
                .iter()
                .find(|support| support.implementation == entry.id)
                .map(|support| &support.supported_versions);
            return Err(format!(
                "catalog_version_retired: {:?} version {:?} is retained for existing cells; supported versions are {supported:?}",
                entry.id, entry.version
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
    fn cdk_support_floor_rejects_active_seventeen_but_allows_archived_entries() {
        for implementation in ["cdk", "cdk-ldk", "cdk-bdk", "cdk-cli-wallet"] {
            let mut entries = default_catalog().entries.clone();
            let mut historical = entries
                .iter()
                .find(|entry| entry.id == implementation)
                .unwrap()
                .clone();
            historical.version = "0.17.7".into();
            historical.support_lifecycle = SupportLifecycle::Supported;
            entries.push(historical);
            assert!(
                CatalogResponse::try_new(entries.clone())
                    .unwrap_err()
                    .contains("catalog_release_ineligible")
            );
            entries.last_mut().unwrap().support_lifecycle = SupportLifecycle::Deprecated;
            let catalog = CatalogResponse::try_new(entries).unwrap();
            let support = catalog
                .implementations
                .iter()
                .find(|entry| entry.implementation == implementation)
                .unwrap();
            assert_eq!(support.minimum_supported.as_deref(), Some("0.18.1"));
            assert_eq!(
                support.supported_versions,
                BTreeSet::from(["0.18.1".into()])
            );
        }
    }

    #[test]
    fn rolling_support_excludes_retired_patches_and_preserves_locked_cells() {
        let base = default_catalog()
            .entries
            .iter()
            .find(|entry| entry.id == "bitcoin-core")
            .unwrap()
            .clone();
        let entries = [
            ("31.1", SupportLifecycle::Preferred),
            ("30.3", SupportLifecycle::Supported),
            ("29.4", SupportLifecycle::Supported),
        ]
        .into_iter()
        .map(|(version, lifecycle)| {
            let mut entry = base.clone();
            entry.version = version.into();
            entry.support_lifecycle = lifecycle;
            entry
        })
        .collect::<Vec<_>>();
        let catalog = CatalogResponse::try_new(entries).unwrap();
        assert_eq!(
            catalog.implementations[0].minimum_supported.as_deref(),
            Some("29.4")
        );
        let cell = crate::CellSpec {
            api_version: crate::API_VERSION.into(),
            name: "retirement".into(),
            components: vec![crate::ComponentSpec {
                id: "chain".into(),
                kind: ComponentKind::Bitcoin,
                implementation: "bitcoin-core".into(),
                version: Some("29.4".into()),
                config_version: base.config_version.clone(),
                control: ControlClass::Cell,
                config: BTreeMap::new(),
            }],
            links: vec![],
            policy: crate::CellPolicy::default(),
        };
        validate_new_cell_versions(&cell, &catalog).unwrap();
        let before = crate::resolve_lock(&cell, &catalog).unwrap();
        let mut retired = catalog.entries.clone();
        retired[2].support_lifecycle = SupportLifecycle::Deprecated;
        let retired = CatalogResponse::try_new(retired).unwrap();
        assert_eq!(
            retired.implementations[0].minimum_supported.as_deref(),
            Some("30.3")
        );
        assert!(
            !retired.implementations[0]
                .supported_versions
                .contains("29.4")
        );
        assert!(
            validate_new_cell_versions(&cell, &retired)
                .unwrap_err()
                .contains("catalog_version_retired")
        );
        assert_eq!(crate::resolve_lock(&cell, &retired).unwrap(), before);

        for (version, error) in [
            ("31.0", "catalog_release_family_duplicate"),
            ("28.4", "catalog_support_window_exceeded"),
        ] {
            let mut entries = catalog.entries.clone();
            let mut extra = base.clone();
            extra.version = version.into();
            extra.support_lifecycle = SupportLifecycle::Supported;
            entries.push(extra);
            assert!(
                CatalogResponse::try_new(entries)
                    .unwrap_err()
                    .contains(error)
            );
        }
    }

    #[test]
    fn bitcoin_release_provenance_and_publisher_mirrors_are_pinned() {
        use sha2::{Digest, Sha256};
        let catalog = default_catalog();
        let bitcoin = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "bitcoin-core")
            .unwrap();
        let provenance = bitcoin
            .build_provenance
            .as_ref()
            .expect("Bitcoin release provenance");
        assert_eq!(bitcoin.version, "31.1");
        assert_eq!(bitcoin.config_version, "bitcoin-core/31/v1");
        assert_eq!(provenance.platform, "linux/amd64,linux/arm64");
        assert_eq!(
            provenance.recipe_digest,
            format!(
                "sha256:{:x}",
                Sha256::digest(include_bytes!("../../../docker/bitcoin/Dockerfile"))
            )
        );
        for entry in &catalog.entries {
            assert!(
                entry
                    .image
                    .starts_with("proofstorm-registry.localhost:5000/")
            );
            assert!(is_sha256_image(&entry.image));
        }
        assert_eq!(
            mirror_image("docker.io/project/image@sha256:exact"),
            "proofstorm-registry.localhost:5000/upstream/docker.io/project/image@sha256:exact"
        );
        assert_eq!(mirror_image(&bitcoin.image), bitcoin.image);
    }

    #[test]
    fn management_clients_have_matching_build_provenance() {
        use sha2::{Digest, Sha256};
        let recipe = include_bytes!("../../../docker/mint/Dockerfile.cdk-0.18.1");
        for id in ["cdk", "cdk-ldk", "cdk-bdk"] {
            let entry = default_catalog()
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap();
            let provenance = entry.build_provenance.as_ref().unwrap();
            assert_eq!(
                provenance.recipe_digest,
                format!("sha256:{:x}", Sha256::digest(recipe))
            );
            assert!(entry.features.contains(&CatalogFeature::MintManagementRpc));
            assert_eq!(
                provenance.commit_sha,
                "a056e0f0f69e94f431b1aeb90d883f18c61ea4c6"
            );
        }
    }

    #[test]
    fn packaged_wallet_provenance_and_observation_surface_are_explicit() {
        use sha2::{Digest, Sha256};
        let entry = default_catalog()
            .entries
            .iter()
            .find(|entry| entry.id == "cdk-cli-wallet")
            .expect("CDK wallet");
        let provenance = entry.build_provenance.as_ref().expect("release provenance");
        let recipe = include_bytes!("../../../docker/wallet/Dockerfile.cdk-0.18.1");
        assert_eq!(
            provenance.recipe_digest,
            format!("sha256:{:x}", Sha256::digest(recipe))
        );
        assert_eq!(
            provenance.commit_sha,
            "a056e0f0f69e94f431b1aeb90d883f18c61ea4c6"
        );
        assert_eq!(
            provenance.platform,
            if crate::wallet_builds::LINUX_AMD64 {
                "linux/amd64"
            } else {
                "linux/arm64"
            }
        );
        let controls = &entry.runtime_endpoints[0].controls;
        assert!(!controls.contains("wallet_balance"));
        assert!(controls.contains("component_exec_live"));
        assert!(!controls.contains("wallet_fund"));
        assert!(!controls.contains("wallet_pay"));
        assert!(!entry.features.contains(&CatalogFeature::WalletOperations));
        assert!(
            entry.source.is_none(),
            "release provenance must not invent a PR"
        );
    }

    #[test]
    fn cocod_provenance_and_passive_controls_match_the_packaged_recipe() {
        use sha2::{Digest, Sha256};
        let entry = default_catalog()
            .entries
            .iter()
            .find(|entry| entry.id == "cocod-wallet")
            .expect("cocod wallet");
        let provenance = entry.build_provenance.as_ref().expect("source provenance");
        assert_eq!(
            provenance.recipe_digest,
            format!(
                "sha256:{:x}",
                Sha256::digest(include_bytes!(
                    "../../../docker/wallet/Dockerfile.kube-cocod"
                ))
            )
        );
        assert_eq!(
            provenance.commit_sha,
            "44e5101cbea370132af6e68f88e01b47e39431c4"
        );
        assert_eq!(provenance.package_path.as_deref(), Some("packages/cocod"));
        assert_eq!(
            provenance.dependency_lock_digest.as_deref(),
            Some("sha256:8c6bc502e3fa1178e3efbb56f86ef8a92d1e9612952a3f8d7268e2de7e611055")
        );
        assert_eq!(entry.support_lifecycle, SupportLifecycle::Experimental);
        let controls = &entry.runtime_endpoints[0].controls;
        assert!(!controls.contains("wallet_balance"));
        assert!(controls.contains("component_exec_live"));
        assert!(!controls.contains("wallet_initialize"));
        assert!(!controls.contains("wallet_fund"));
        assert!(!controls.contains("wallet_pay"));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one catalog invariant test keeps all fail-closed variants together"
    )]
    fn catalog_support_summary_is_exact_and_invariants_fail_closed() {
        let catalog = default_catalog();
        assert_eq!(catalog.entries.len(), 23);
        assert_eq!(catalog.implementations.len(), 16);
        let lnd = catalog
            .implementations
            .iter()
            .find(|support| support.implementation == "lnd")
            .expect("LND support summary");
        assert_eq!(lnd.minimum_supported.as_deref(), Some("0.20.4-beta"));
        assert_eq!(lnd.preferred_version.as_deref(), Some("0.21.3-beta"));
        assert_eq!(
            lnd.supported_versions,
            BTreeSet::from(["0.20.4-beta".into(), "0.21.3-beta".into()])
        );
        assert!(catalog.implementations.iter().all(|support| {
            matches!(
                support.implementation.as_str(),
                "cocod-wallet" | "ldk-server" | "cdk-ldk-server-processor"
            ) || support.implementation == "lnd"
                || matches!(
                    support.implementation.as_str(),
                    "nutshell" | "nutshell-wallet"
                )
                || support.minimum_supported == support.preferred_version
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
            .find(|entry| entry.id == "cdk" && entry.version == "0.18.0")
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
            .find(|entry| entry.id == "cdk" && entry.version == "0.18.0")
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

    #[test]
    fn promoted_releases_keep_exact_historical_entries_and_current_defaults() {
        for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
            let catalog = catalog_for_platform(platform);
            for id in [
                "cdk",
                "cdk-ldk",
                "cdk-bdk",
                "cdk-cli-wallet",
                "nutshell",
                "nutshell-wallet",
            ] {
                let cdk = id.starts_with("cdk");
                let old_version = if cdk { "0.18.0" } else { "0.20.3" };
                let version = if cdk { "0.18.1" } else { "0.21.0" };
                let support = catalog
                    .implementations
                    .iter()
                    .find(|entry| entry.implementation == id)
                    .unwrap();
                assert_eq!(support.preferred_version.as_deref(), Some(version));
                let old = catalog
                    .entries
                    .iter()
                    .find(|entry| entry.id == id && entry.version == old_version)
                    .unwrap();
                let current = catalog
                    .entries
                    .iter()
                    .find(|entry| entry.id == id && entry.version == version)
                    .unwrap();
                assert_eq!(
                    old.support_lifecycle,
                    if cdk {
                        SupportLifecycle::Deprecated
                    } else {
                        SupportLifecycle::Supported
                    }
                );
                assert_eq!(current.support_lifecycle, SupportLifecycle::Preferred);
                assert_ne!(old.image, current.image);
                assert_ne!(old.source_digest, current.source_digest);
                assert_eq!(old.config_version, current.config_version);
                assert_eq!(
                    current.build_provenance.as_ref().unwrap().commit_sha,
                    if cdk {
                        "a056e0f0f69e94f431b1aeb90d883f18c61ea4c6"
                    } else {
                        "a9749146c6bd7f9ab75375a050e9ba795cee301c"
                    }
                );
                if id == "nutshell" {
                    assert_eq!(
                        current.protocol_action_adapter_version.as_deref(),
                        Some("nutshell-mint/0.21/v1")
                    );
                    assert_eq!(
                        old.protocol_action_adapter_version.as_deref(),
                        Some("0.1.0-alpha.1")
                    );
                }
            }
        }
    }
}
