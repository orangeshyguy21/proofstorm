//! Exact upstream revisions for the first CDK gRPC processor integration.
use super::{
    BTreeSet, BackendContractRegistry, BuildProvenance, CatalogEntry, CatalogFeature,
    CatalogRuntimeEndpoint, ComponentKind, ControlClass, LinkKind, PaymentMethod, ReleaseChannel,
    StorageBackend, SupportLifecycle, catalog_entry_with_lifecycle, dependency, payment_binding,
    runtime_endpoint, support_matrix,
};

pub const LDK_SERVER_VERSION: &str = "0.1.0-50fe752";
pub const LDK_PROCESSOR_VERSION: &str = "0.1.0-fe468ca";

#[allow(
    clippy::too_many_lines,
    reason = "catalog entries explicitly declare their complete compatibility contracts"
)]
pub(super) fn extend(
    entries: &mut Vec<CatalogEntry>,
    amd64: bool,
    backends: &BackendContractRegistry,
    adapter_version: &str,
) {
    let node_image = if amd64 {
        "proofstorm-registry.localhost:5000/ldk-server@sha256:1a21ea3bdd9a73ce04e3e8159ebcd66fcfec25a649b71798d1f9bb3c454d94a3"
    } else {
        "proofstorm-registry.localhost:5000/ldk-server@sha256:15e5a5ea093a320ecb8ffdf74ecd39ab5c92dc65b2371d47bc3e1c920c2b22cc"
    };
    let processor_image = if amd64 {
        "proofstorm-registry.localhost:5000/cdk-ldk-server-processor@sha256:1fcd8620a90f221432e960e29c46e5c11380ed661ff215d8acdad62ab42976c3"
    } else {
        "proofstorm-registry.localhost:5000/cdk-ldk-server-processor@sha256:ee21cdf4056d6f228d17f2e6739f98f11f84f94812762d1b8d6e7d873fc9451e"
    };
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        "ldk-server",
        backends,
        ComponentKind::Lightning,
        "Standalone LDK Server regtest node with native CLI",
        adapter_version,
        LDK_SERVER_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        node_image,
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
    ));
    entries.push(catalog_entry_with_lifecycle(
        amd64,
        "cdk-ldk-server-processor",
        backends,
        ComponentKind::PaymentProcessor,
        "CDK gRPC payment processor backed by LDK Server",
        adapter_version,
        LDK_PROCESSOR_VERSION,
        ReleaseChannel::Prerelease,
        SupportLifecycle::Experimental,
        processor_image,
        BTreeSet::from([
            CatalogFeature::Regtest,
            CatalogFeature::Bolt11,
            CatalogFeature::Bolt12,
        ]),
        vec![dependency(
            LinkKind::PaymentBackend,
            "ldk-server",
            &[LDK_SERVER_VERSION],
        )],
        support_matrix(
            &[StorageBackend::Ephemeral],
            &[PaymentMethod::Bolt11, PaymentMethod::Bolt12],
            &["ldk-server"],
            &["sat"],
            &[
                payment_binding(
                    PaymentMethod::Bolt11,
                    "sat",
                    "ldk-server",
                    &[LDK_SERVER_VERSION],
                ),
                payment_binding(
                    PaymentMethod::Bolt12,
                    "sat",
                    "ldk-server",
                    &[LDK_SERVER_VERSION],
                ),
            ],
            &[],
            vec![],
        ),
        vec![ControlClass::Cell, ControlClass::Target],
    ));
    for entry in entries.iter_mut() {
        let encoded = match entry.id.as_str() {
            "ldk-server" => include_str!("../../../docker/payment/ldk-server-provenance.json"),
            "cdk-ldk-server-processor" => {
                include_str!("../../../docker/payment/cdk-ldk-server-provenance.json")
            }
            _ => continue,
        };
        let mut provenance: BuildProvenance =
            serde_json::from_str(encoded).expect("pinned payment backend build provenance");
        if amd64 {
            provenance.platform = "linux/amd64".into();
        }
        entry.source_digest = crate::digest_json(&(&entry.source_digest, &provenance));
        entry.build_provenance = Some(provenance);
    }
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.id == "cdk" && entry.version == "0.18.1")
    {
        entry.features.insert(CatalogFeature::Bolt12);
        entry
            .support_matrix
            .payment_methods
            .insert(PaymentMethod::Bolt12);
        entry
            .support_matrix
            .payment_backends
            .insert("cdk-ldk-server-processor".into());
        entry.compatible_dependencies.push(dependency(
            LinkKind::PaymentBackend,
            "cdk-ldk-server-processor",
            &[LDK_PROCESSOR_VERSION],
        ));
        for method in [PaymentMethod::Bolt11, PaymentMethod::Bolt12] {
            entry
                .support_matrix
                .payment_bindings
                .insert(payment_binding(
                    method,
                    "sat",
                    "cdk-ldk-server-processor",
                    &[LDK_PROCESSOR_VERSION],
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
    match implementation {
        "ldk-server" => Some(vec![runtime_endpoint(
            "component",
            "lightning",
            &["component_logs", "reachability_oracle"],
            &[
                "Pinned upstream revision 50fe7523be3529d86bfee0dfc35df9a52aca7310. Native entrypoint: ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536. The CLI reads its API key and TLS certificate from private /data storage. Use amounts with explicit sat/msat suffixes. Native CLI controls include funding, peers, channels, BOLT11/BOLT12, payment status, and held invoices. Verify asynchronous payments against recipient state. Keep credentials out of command arguments and public output.",
            ],
        )]),
        "cdk-ldk-server-processor" => Some(vec![runtime_endpoint(
            "component",
            "payment_processor",
            &["component_logs", "reachability_oracle"],
            &[
                "Pinned upstream revision fe468cad486157683eddbc0df4ff87ba71b6c0a3, CDK payment protocol 4.0.0. Requires bolt11/sat and bolt12/sat links to one LDK Server; CDK mints use the same two bindings to this component. The backend reports msat internally; the supported mint contract is sat with CDK conversion. The linked LDK node owns durable payment history; the live event stream does not replay payments missed during an outage. After reconnect, check original Cashu mint and melt quotes to reconcile them against durable LDK payment history. GetSettings: /opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls. Mint/processor transport requires mutual TLS. No MPP support.",
            ],
        )]),
        _ => None,
    }
}
