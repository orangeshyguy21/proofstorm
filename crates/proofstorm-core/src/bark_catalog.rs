//! Native ARM64 Bark preview. Public distribution and full release qualification
//! remain pending; development installations must seed the verified images.
use super::{
    BTreeSet, BackendContractRegistry, BuildProvenance, CatalogEntry, CatalogFeature,
    CatalogRuntimeEndpoint, ComponentKind, ControlClass, LinkKind, PaymentMethod, ReleaseChannel,
    StorageBackend, SupportLifecycle, catalog_entry_with_lifecycle, dependency, payment_binding,
    runtime_endpoint, support_matrix,
};
use crate::processor_ids::{BARK_PROCESSOR, BARK_SERVER, CLN_HOLD};
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
const PREVIEW: &str = "Experimental Linux ARM64 local preview. Local managed payment/restart qualification passed with original-quote reconciliation. Native AMD64 qualification and public image distribution are pending. Requires explicitly seeded development images.";

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
    // Qualification stages verified native candidates in this embedded file in
    // its disposable checkout. There is no runtime image override. The shipped
    // file deliberately omits platforms that are not yet available.
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
        "Core Lightning with hold-invoice plugin for the local Bark preview",
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
        "Bark Ark server for the local BOLT11/sat preview",
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
        ]),
        vec![
            dependency(LinkKind::ChainBackend, "bitcoin-core", &["31.1"]),
            dependency(LinkKind::ArkBackend, BARK_SERVER, &[SERVER_VERSION]),
        ],
        support_matrix(
            &[StorageBackend::PersistentVolume],
            &[PaymentMethod::Bolt11],
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
        entry
            .support_matrix
            .payment_bindings
            .insert(payment_binding(
                PaymentMethod::Bolt11,
                "sat",
                BARK_PROCESSOR,
                &[PROCESSOR_VERSION],
            ));
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
            "CDK payment protocol 4.0.0, BOLT11/sat only. Link to one Bark server with ark_backend/regtest and the same indexed Bitcoin regtest node with chain_backend/regtest. CDK 0.18.1 mints bind bolt11/sat to this processor. event_poll_interval_ms is 1–60000 (default 5000). GetSettings: /opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls cdk-bark-processor. The generated seed, db.sqlite and onchain_state.redb persist together; missing identity or partial state requires explicit recovery. Verify original quote state and independent Lightning settlement after interruptions.",
        ),
        BARK_SERVER => (
            "ark_server",
            "BOLT11/sat regtest profile. Requires indexed Bitcoin, primary PostgreSQL and CLN/hold on the same chain. Public Ark RPC uses port 3535; admin/integration APIs remain on loopback. Native entrypoint: captaind --config /usr/local/share/bark/captaind.default.toml rpc. Use component_exec_live for loopback access. Native mnemonic/state and the PostgreSQL identity seal must persist together. Restore missing retained state rather than reinitializing it. No standalone Bark CLI wallet or on-chain/boarding payment rail is exposed.",
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
        &[PREVIEW, guidance],
    )])
}
