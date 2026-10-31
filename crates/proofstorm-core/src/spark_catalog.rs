//! Experimental Spark stack. Entries exist only for platforms whose images are
//! pinned in `spark_images.json`, which stays empty until the images are
//! published and a managed gate has qualified them.
use super::{
    BTreeSet, BackendContractRegistry, BuildProvenance, CatalogEntry, CatalogFeature,
    CatalogRuntimeEndpoint, ComponentKind, ControlClass, LinkKind, PaymentMethod, ReleaseChannel,
    StorageBackend, SupportLifecycle, catalog_entry_with_lifecycle, dependency, payment_binding,
    runtime_endpoint, support_matrix,
};
use crate::{
    ProcessorProfile,
    processor_ids::{OPEN_SSP, SPARK_OPERATORS, SPARK_PROCESSOR},
};
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Images {
    operators: String,
    ssp: String,
    processor: String,
    ldk_server: String,
}

pub(super) const PROCESSOR_VERSION: &str = "0.1.0-d7244e2";
const OPERATORS_VERSION: &str = "0.0.0-5e0c879";
const SSP_VERSION: &str = "0.1.0-9c25f69";
/// open-ssp's LDK client authenticates with macaroons, which the API-key-era
/// `ldk-server` that the CDK LDK Server processor needs does not issue.
const LDK_SERVER_VERSION: &str = "0.1.0-762bc47";
const QUALIFICATION: &str = "Experimental. A Docker Compose probe on native ARM64 passed BOLT11/sat mint, melt, an unreachable melt and restarts through this stack; no managed qualification has run yet.";
const LDK_SERVER_GUIDANCE: &str = "Pinned upstream revision 762bc473342c8f945475785ec6743a9b26f56aff for open-ssp. It authenticates with macaroons, so the CDK LDK Server processor, which needs an API key, uses 0.1.0-50fe752 instead. Native entrypoint: ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536; the CLI reads the admin macaroon and TLS certificate from /data through that config. Use amounts with explicit sat/msat suffixes. Channel recipe for an SSP node: onchain-receive, send to it from bitcoin-core and mine 6 blocks, take the peer's node_id from its get-node-info, then open-channel <peer node_id> <peer component>:9735 2000000sat --push-to-counterparty 1000000sat --announce-channel so both directions have liquidity, mine 6 blocks and wait until list-channels shows the channel is_channel_ready and is_usable. Keep credentials out of command arguments and public output.";

fn platform(amd64: bool) -> &'static str {
    if amd64 { "linux/amd64" } else { "linux/arm64" }
}

/// The images pinned for this platform, if any.
pub(super) fn pinned(amd64: bool) -> Option<Images> {
    let mut images: BTreeMap<String, Images> =
        serde_json::from_str(include_str!("spark_images.json")).expect("pinned Spark images");
    images.remove(platform(amd64))
}

#[cfg(any(test, feature = "test-catalogs"))]
pub(super) fn test_images() -> Images {
    let image = |name: &str| {
        format!(
            "proofstorm-registry.localhost:5000/{name}@sha256:{}",
            "0".repeat(64)
        )
    };
    Images {
        operators: image("spark-operator"),
        ssp: image("open-ssp"),
        processor: image("cdk-spark-processor"),
        ldk_server: image("ldk-server"),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "catalog entries explicitly declare complete compatibility contracts"
)]
pub(super) fn extend(
    entries: &mut Vec<CatalogEntry>,
    amd64: bool,
    backends: &BackendContractRegistry,
    adapter_version: &str,
    images: &Images,
) {
    let mut ldk_server = catalog_entry_with_lifecycle(
        amd64,
        "ldk-server",
        backends,
        ComponentKind::Lightning,
        "Standalone LDK Server regtest node with native CLI, macaroon authentication for open-ssp",
        adapter_version,
        LDK_SERVER_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.ldk_server,
        BTreeSet::from([
            CatalogFeature::NativeCli,
            CatalogFeature::NativeCliEntrypoints,
            CatalogFeature::Regtest,
            CatalogFeature::PersistentState,
            CatalogFeature::Bolt11,
            CatalogFeature::Bolt12,
        ]),
        vec![dependency(
            LinkKind::ChainBackend,
            "bitcoin-core",
            &["31.1"],
        )],
        support_matrix(
            &[StorageBackend::PersistentVolume],
            &[PaymentMethod::Bolt11, PaymentMethod::Bolt12],
            &[],
            &["sat", "msat"],
            &[],
            &[],
            vec![],
        ),
        vec![
            ControlClass::Cell,
            ControlClass::Target,
            ControlClass::Workspace,
        ],
    );
    // Guidance is per implementation; this revision authenticates differently.
    ldk_server.runtime_endpoints = vec![runtime_endpoint(
        "component",
        "lightning",
        &["component_logs", "reachability_oracle"],
        &[LDK_SERVER_GUIDANCE],
    )];
    entries.push(ldk_server);
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        SPARK_OPERATORS,
        backends,
        ComponentKind::SparkServer,
        "Spark operator federation (FROST signers) for a local regtest Spark network",
        adapter_version,
        OPERATORS_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.operators,
        BTreeSet::from([CatalogFeature::Regtest, CatalogFeature::Postgres]),
        vec![
            dependency(LinkKind::ChainBackend, "bitcoin-core", &["31.1"]),
            dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
        ],
        support_matrix(&[StorageBackend::Postgres], &[], &[], &[], &[], &[], vec![]),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        OPEN_SSP,
        backends,
        ComponentKind::SparkServer,
        "open-ssp Spark Service Provider settling Lightning through LDK Server",
        adapter_version,
        SSP_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.ssp,
        BTreeSet::from([
            CatalogFeature::Regtest,
            CatalogFeature::PersistentState,
            CatalogFeature::Bolt11,
        ]),
        vec![
            dependency(
                LinkKind::SparkBackend,
                SPARK_OPERATORS,
                &[OPERATORS_VERSION],
            ),
            dependency(
                LinkKind::PaymentBackend,
                "ldk-server",
                &[LDK_SERVER_VERSION],
            ),
        ],
        support_matrix(
            &[StorageBackend::PersistentVolume],
            &[PaymentMethod::Bolt11],
            &["ldk-server"],
            &["sat"],
            &[payment_binding(
                PaymentMethod::Bolt11,
                "sat",
                "ldk-server",
                &[LDK_SERVER_VERSION],
            )],
            &[],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        SPARK_PROCESSOR,
        backends,
        ComponentKind::PaymentProcessor,
        "CDK gRPC payment processor with a persistent Spark wallet",
        adapter_version,
        PROCESSOR_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.processor,
        BTreeSet::from([
            CatalogFeature::Regtest,
            CatalogFeature::PersistentState,
            CatalogFeature::Bolt11,
        ]),
        vec![dependency(LinkKind::SparkBackend, OPEN_SSP, &[SSP_VERSION])],
        support_matrix(
            &[StorageBackend::PersistentVolume],
            &Vec::from_iter(ProcessorProfile::Spark.supported_methods()),
            &[],
            &["sat"],
            &[],
            &[],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    for entry in entries.iter_mut() {
        let encoded = match (entry.id.as_str(), entry.version.as_str()) {
            (SPARK_OPERATORS, _) => {
                include_str!("../../../docker/payment/spark-operator-provenance.json")
            }
            (OPEN_SSP, _) => include_str!("../../../docker/payment/open-ssp-provenance.json"),
            (SPARK_PROCESSOR, _) => {
                include_str!("../../../docker/payment/cdk-spark-provenance.json")
            }
            ("ldk-server", LDK_SERVER_VERSION) => {
                include_str!("../../../docker/payment/ldk-server-762bc47-provenance.json")
            }
            _ => continue,
        };
        let mut provenance: BuildProvenance =
            serde_json::from_str(encoded).expect("pinned Spark build provenance");
        provenance.platform = platform(amd64).into();
        entry.source_digest =
            crate::digest_json(&(&entry.source_digest, &entry.image, &provenance));
        entry.build_provenance = Some(provenance);
    }
    // CDK 0.18 registers every method a gRPC processor advertises.
    for entry in entries.iter_mut().filter(|entry| entry.id == "cdk") {
        super::bind_processor(entry, ProcessorProfile::Spark);
    }
}

pub(super) fn endpoints(implementation: &str) -> Option<Vec<CatalogRuntimeEndpoint>> {
    let (kind, guidance) = match implementation {
        SPARK_PROCESSOR => (
            "payment_processor",
            "CDK payment protocol 4.0.0, bolt11 in sat only: no amountless invoices, descriptions or MPP. Bind the mint with exactly one payment_backend link (bolt11, sat) to this component. Link to one open-ssp with spark_backend/regtest; the processor reaches the operators through that SSP's own link. Its Spark wallet starts empty and is funded only by paid mint quotes, which the SSP settles from its own Spark liquidity, so fund the SSP first. Melts carry no payment preimage. A failed Lightning send can leave the processor's Spark funds locked in a pending swap for up to 16 days. The generated mnemonic and the quote mapping database persist together. GetSettings: /opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls cdk-spark-processor. Verify original quote state and independent settlement after interruptions.",
        ),
        OPEN_SSP => (
            "spark_server",
            "Regtest Spark Service Provider. Requires exactly one spark_backend/regtest link to spark-operators and one payment_backend (bolt11, sat) link to ldk-server 0.1.0-762bc47 on the operators' Bitcoin node. Readiness checks identities only; upstream always reports ldk_mode live. Its admin API is loopback-only: run curl with cell_exec on this component and send the token as a header file, never in arguments, e.g. curl -sS --fail-with-body -H @/run/open-ssp/admin.header http://127.0.0.1:5000/status (spark.available_sats is its Spark liquidity). Funding is explicit and never part of readiness: (1) curl -sS --fail-with-body -X POST -H @/run/open-ssp/admin.header http://127.0.0.1:5000/admin/spark/deposit-address returns {address}; it fails until the operators finish key generation, about 16 s after they are ready, so retry. (2) On bitcoin-core, sendtoaddress that address (0.005 BTC is enough to start) and mine 3 blocks. (3) Take the output index paying that address from getrawtransaction <txid> true and the hex from getrawtransaction <txid> false. (4) curl -sS --fail-with-body -H @/run/open-ssp/admin.header -H 'Content-Type: application/json' -d '{\"transaction_hex\":\"<hex>\",\"vout\":<n>}' http://127.0.0.1:5000/admin/spark/claim-deposit returns leaf_values. (5) Recheck /status until available_sats rises. Lightning liquidity is separate: open a channel from the linked ldk-server to a peer (see that node's guidance). The Spark mnemonic and SSP state persist together; restore missing state rather than reinitializing it.",
        ),
        SPARK_OPERATORS => (
            "spark_server",
            "Regtest Spark operator federation: operators pods (default 3) sign with a FROST threshold (default 2). Requires one chain_backend/regtest link to bitcoin-core and one primary database_backend link to postgresql; each operator creates its own main and ephemeral databases. Ready means each operator serves gRPC with its databases; key generation finishes about 16 s after every operator is ready, and SSP deposit addresses fail until it has. Identity keys and TLS certificates are generated per component and never leave it; consumers get only public keys and the CA.",
        ),
        _ => return None,
    };
    Some(vec![runtime_endpoint(
        "component",
        kind,
        &["component_logs", "reachability_oracle"],
        &[QUALIFICATION, guidance],
    )])
}

#[cfg(test)]
mod tests {
    use crate::{CatalogPlatform, catalog_for_platform, processor_ids::SPARK_PROCESSOR};

    #[test]
    fn spark_entries_ship_only_for_pinned_platforms() {
        for (platform, amd64) in [
            (CatalogPlatform::LinuxAmd64, true),
            (CatalogPlatform::LinuxArm64, false),
        ] {
            let catalog = catalog_for_platform(platform);
            let shipped = |id: &str| catalog.entries.iter().any(|entry| entry.id == id);
            let pinned = super::pinned(amd64).is_some();
            for id in [super::SPARK_OPERATORS, super::OPEN_SSP, SPARK_PROCESSOR] {
                assert_eq!(shipped(id), pinned, "{id} on {platform:?}");
            }
            // The CDK mint binds the processor only alongside it.
            let cdk = catalog
                .entries
                .iter()
                .find(|entry| entry.id == "cdk")
                .unwrap();
            assert_eq!(
                cdk.support_matrix
                    .payment_backends
                    .contains(SPARK_PROCESSOR),
                pinned
            );
        }
    }

    #[test]
    fn a_pinned_cdk_mint_binds_the_processor_in_bolt11_sat() {
        let catalog = super::super::catalog_with_spark_test_images(true);
        let cdk = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "cdk")
            .unwrap();
        assert!(cdk.support_matrix.payment_bindings.iter().any(|binding| {
            binding.backend.implementation == SPARK_PROCESSOR
                && binding.method == crate::PaymentMethod::Bolt11
                && binding.unit == "sat"
        }));
    }

    fn guidance(id: &str, version: &str) -> String {
        let catalog = super::super::catalog_with_spark_test_images(false);
        catalog
            .entries
            .iter()
            .find(|entry| entry.id == id && entry.version == version)
            .unwrap()
            .runtime_endpoints
            .iter()
            .flat_map(|endpoint| endpoint.limitations.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn each_ldk_server_revision_describes_its_own_authentication() {
        let macaroon = guidance("ldk-server", super::LDK_SERVER_VERSION);
        assert!(macaroon.contains("762bc473342c8f945475785ec6743a9b26f56aff"));
        assert!(macaroon.contains("macaroon"));
        assert!(macaroon.contains("open-channel"));
        let api_key = guidance("ldk-server", "0.1.0-50fe752");
        assert!(api_key.contains("50fe7523be3529d86bfee0dfc35df9a52aca7310"));
        assert!(!api_key.contains("macaroon"));
    }

    #[test]
    fn ssp_funding_recipes_send_the_admin_token_from_a_file() {
        let ssp = guidance(super::OPEN_SSP, super::SSP_VERSION);
        for path in [
            "/admin/spark/deposit-address",
            "/admin/spark/claim-deposit",
            "/status",
        ] {
            assert!(ssp.contains(path), "{path}");
        }
        // The driver writes this header file; no recipe carries the token itself.
        assert_eq!(ssp.matches("-H @/run/open-ssp/admin.header").count(), 3);
        assert!(!ssp.contains("Bearer"));
        assert!(!ssp.contains("admin-token"));
    }
}
