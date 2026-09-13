//! Shared compatibility inputs for an installed client and its controller.
use kube::CustomResourceExt;
use serde_json::{Value, json};

/// Deliberately includes all shipped schemas, catalog entries, and helper pins.
#[must_use]
pub fn contract() -> Value {
    json!({
        "format_version": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "catalog": proofstorm_core::default_catalog(),
        "helpers": crate::images::HELPER_IMAGES,
        "driver_version": proofstorm_driver::VERSION,
        "prober": {"protocol": proofstorm_prober::PROTOCOL_VERSION,
            "request": schemars::schema_for!(proofstorm_prober::Request),
            "response": schemars::schema_for!(proofstorm_prober::Response)},
        "crds": [crate::ProofstormCell::crd(), crate::ProofstormCellAction::crd(),
                 crate::ProofstormCandidateBuild::crd()]
    })
}
