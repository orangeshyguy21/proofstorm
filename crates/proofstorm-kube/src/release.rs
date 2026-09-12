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
        "crds": [crate::ProofstormCell::crd(), crate::ProofstormCellAction::crd(),
                 crate::ProofstormCandidateBuild::crd()]
    })
}
