//! Experimental Bark support with published native AMD64 and ARM64 images.
use super::{
    BTreeSet, BackendContractRegistry, BuildProvenance, CatalogEntry, CatalogFeature,
    CatalogRuntimeEndpoint, ComponentKind, ControlClass, LinkKind, PaymentMethod, ReleaseChannel,
    StorageBackend, SupportLifecycle, catalog_entry_with_lifecycle, dependency, payment_binding,
    runtime_endpoint, support_matrix,
};
use crate::{
    ProcessorProfile,
    processor_ids::{BARK_PROCESSOR, BARK_SERVER, CLN_HOLD},
};
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Images {
    processor: String,
    server: String,
    cln: String,
}

const PROCESSOR_VERSION: &str = "0.1.0-fe468ca";
const SERVER_VERSION: &str = "0.7.0-6188e2d";
const CLN_HOLD_VERSION: &str = "26.06.7-hold.0.3.3";
const QUALIFICATION: &str = "Experimental support on Linux AMD64 and ARM64. Published native images passed managed BOLT11/sat payment/restart qualification with original-quote reconciliation. Ordinary native qualification covers the distributed pins.";

#[allow(
    clippy::too_many_lines,
    reason = "catalog entries explicitly declare complete compatibility contracts"
)]
pub(super) fn extend(
    entries: &mut Vec<CatalogEntry>,
    amd64: bool,
    backends: &BackendContractRegistry,
    adapter_version: &str,
) {
    // The shipped file pins verified public images on both platforms. Candidate
    // qualification stages alternate pins only in its disposable checkout;
    // there is no runtime image override.
    let platform = if amd64 { "linux/amd64" } else { "linux/arm64" };
    let images: BTreeMap<String, Images> =
        serde_json::from_str(include_str!("bark_images.json")).expect("pinned Bark images");
    let Some(images) = images.get(platform) else {
        return;
    };
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        CLN_HOLD,
        backends,
        ComponentKind::Lightning,
        "Core Lightning with hold-invoice plugin for managed Bark payments",
        adapter_version,
        CLN_HOLD_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.cln,
        BTreeSet::from([
            CatalogFeature::NativeCli,
            CatalogFeature::NativeCliEntrypoints,
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
            &["sat", "msat"],
            &[],
            &[],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        BARK_SERVER,
        backends,
        ComponentKind::ArkServer,
        "Bark Ark server for experimental Lightning, on-chain and arkoor payments",
        adapter_version,
        SERVER_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.server,
        BTreeSet::from([
            CatalogFeature::NativeCli,
            CatalogFeature::NativeCliEntrypoints,
            CatalogFeature::Regtest,
            CatalogFeature::PersistentState,
            CatalogFeature::Postgres,
            CatalogFeature::Bolt11,
        ]),
        vec![
            dependency(LinkKind::ChainBackend, "bitcoin-core", &["31.1"]),
            dependency(LinkKind::DatabaseBackend, "postgresql", &["17.11"]),
            dependency(LinkKind::PaymentBackend, CLN_HOLD, &[CLN_HOLD_VERSION]),
        ],
        support_matrix(
            &[StorageBackend::PersistentVolume, StorageBackend::Postgres],
            &[PaymentMethod::Bolt11],
            &[CLN_HOLD],
            &["sat"],
            &[payment_binding(
                PaymentMethod::Bolt11,
                "sat",
                CLN_HOLD,
                &[CLN_HOLD_VERSION],
            )],
            &[],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        BARK_PROCESSOR,
        backends,
        ComponentKind::PaymentProcessor,
        "CDK gRPC payment processor with a persistent Bark wallet",
        adapter_version,
        PROCESSOR_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        &images.processor,
        BTreeSet::from([
            CatalogFeature::Regtest,
            CatalogFeature::PersistentState,
            CatalogFeature::Bolt11,
            CatalogFeature::Onchain,
        ]),
        vec![
            dependency(LinkKind::ChainBackend, "bitcoin-core", &["31.1"]),
            dependency(LinkKind::ArkBackend, BARK_SERVER, &[SERVER_VERSION]),
        ],
        support_matrix(
            &[StorageBackend::PersistentVolume],
            &Vec::from_iter(ProcessorProfile::Bark.supported_methods()),
            &[],
            &["sat"],
            &[],
            &[],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    for entry in entries.iter_mut() {
        let encoded = match entry.id.as_str() {
            BARK_PROCESSOR => include_str!("../../../docker/payment/cdk-bark-provenance.json"),
            BARK_SERVER => include_str!("../../../docker/payment/bark-server-provenance.json"),
            CLN_HOLD => include_str!("../../../docker/payment/cln-hold-provenance.json"),
            _ => continue,
        };
        let mut provenance: BuildProvenance =
            serde_json::from_str(encoded).expect("pinned Bark build provenance");
        provenance.platform = platform.into();
        entry.source_digest =
            crate::digest_json(&(&entry.source_digest, &entry.image, &provenance));
        entry.build_provenance = Some(provenance);
    }
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.id == "cdk" && entry.version == "0.18.1")
    {
        entry
            .support_matrix
            .payment_backends
            .insert(BARK_PROCESSOR.into());
        entry.compatible_dependencies.push(dependency(
            LinkKind::PaymentBackend,
            BARK_PROCESSOR,
            &[PROCESSOR_VERSION],
        ));
        // CDK 0.18.1 registers every method a gRPC processor advertises,
        // including on-chain and custom methods.
        for method in ProcessorProfile::Bark.supported_methods() {
            entry
                .features
                .extend(CatalogFeature::for_payment_method(&method));
            entry.support_matrix.payment_methods.insert(method.clone());
            entry
                .support_matrix
                .payment_bindings
                .insert(payment_binding(
                    method,
                    "sat",
                    BARK_PROCESSOR,
                    &[PROCESSOR_VERSION],
                ));
        }
        entry.source_digest = crate::digest_json(&(
            &entry.source_digest,
            &entry.support_matrix,
            &entry.compatible_dependencies,
        ));
    }
}

pub(super) fn endpoints(implementation: &str) -> Option<Vec<CatalogRuntimeEndpoint>> {
    let (kind, guidance) = match implementation {
        BARK_PROCESSOR => (
            "payment_processor",
            "CDK payment protocol 4.0.0, unit sat. payment_methods selects what the processor advertises from bolt11, onchain and arkoor; the default is all three, as upstream. The CDK 0.18.1 mint registers every advertised method, so bind each one exactly once with a payment_backend link (method, sat) to this component. Another backend of the mint, such as embedded BDK for onchain, can serve a method left out. bolt11 settles through the Ark server's Lightning node. onchain mint quotes return a processor wallet address; after 1 confirmation the deposit is boarded into Ark with the board fee deducted from the minted amount. onchain melts offboard from the processor's Ark balance. arkoor is melt-only: the request is an Ark address on the same server, with zero fee; cdk-cli 0.18.1 cannot create custom-method melts. Managed qualification does not exercise on-chain melts or arkoor payments. Link to one Bark server with ark_backend/regtest and the same indexed Bitcoin regtest node with chain_backend/regtest. event_poll_interval_ms is 1–60000 (default 5000). GetSettings: /opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls cdk-bark-processor METHODS, where METHODS is payment_methods comma-separated (omitted means bolt11,onchain,arkoor). The generated seed, db.sqlite and onchain_state.redb persist together; missing identity or partial state requires explicit recovery. Verify original quote state and independent settlement after interruptions.",
        ),
        BARK_SERVER => (
            "ark_server",
            "Regtest Ark server; Lightning payments route through CLN/hold (bolt11/sat). Requires indexed Bitcoin, primary PostgreSQL and CLN/hold on the same chain. Public Ark RPC uses port 3535; admin/integration APIs remain on loopback. Native entrypoint: captaind --config /usr/local/share/bark/captaind.default.toml rpc. Use component_exec_live for loopback access. Native mnemonic/state and the PostgreSQL identity seal must persist together. Restore missing retained state rather than reinitializing it. No standalone Bark CLI wallet is included; the CDK Bark processor holds the managed Bark wallet.",
        ),
        CLN_HOLD => (
            "lightning",
            "Core Lightning 26.06.7 plus hold 0.3.3. Native entrypoint: lightning-cli --notifications=none --lightning-dir=/data --network=regtest. Separate mutual-TLS APIs: CLN port 9988, hold port 9292. listholdinvoices exposes native unpaid/accepted/paid state and held HTLCs. Preserve the HSM, Lightning and hold databases, and native TLS identities together. Keep private seed/key material out of command arguments and public output.",
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
