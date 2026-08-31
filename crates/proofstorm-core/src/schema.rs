use schemars::schema_for;
use serde_json::Value;

use crate::{
    Capability, CatalogResponse, EvidenceBundle, Experiment, ExperimentLease, LabOperation,
    LabSpec, NetworkFaultBackend, OperationArtifact, PublishedRevision, ResolvedLock,
    ValidationReport, WalletQuote,
};

#[must_use]
/// Generate the public JSON Schema documents from the Rust contract types.
///
/// # Panics
///
/// Panics only if a `schemars` schema cannot be represented as JSON, which
/// indicates a programming error in a contract type.
pub fn schema_documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "lab.schema.json",
            serde_json::to_value(schema_for!(LabSpec)).expect("LabSpec schema serializes"),
        ),
        (
            "capability.schema.json",
            serde_json::to_value(schema_for!(Capability)).expect("Capability schema serializes"),
        ),
        (
            "catalog.schema.json",
            serde_json::to_value(schema_for!(CatalogResponse)).expect("catalog schema serializes"),
        ),
        (
            "network-fault-backend.schema.json",
            serde_json::to_value(schema_for!(NetworkFaultBackend))
                .expect("network fault backend schema serializes"),
        ),
        (
            "validation-report.schema.json",
            serde_json::to_value(schema_for!(ValidationReport))
                .expect("validation report schema serializes"),
        ),
        (
            "resolved-lock.schema.json",
            serde_json::to_value(schema_for!(ResolvedLock))
                .expect("resolved lock schema serializes"),
        ),
        (
            "published-revision.schema.json",
            serde_json::to_value(schema_for!(PublishedRevision))
                .expect("published revision schema serializes"),
        ),
        (
            "lab-operation.schema.json",
            serde_json::to_value(schema_for!(LabOperation))
                .expect("lab operation schema serializes"),
        ),
        (
            "operation-artifact.schema.json",
            serde_json::to_value(schema_for!(OperationArtifact))
                .expect("operation artifact schema serializes"),
        ),
        (
            "wallet-quote.schema.json",
            serde_json::to_value(schema_for!(WalletQuote)).expect("wallet quote schema serializes"),
        ),
        (
            "experiment.schema.json",
            serde_json::to_value(schema_for!(Experiment)).expect("experiment schema serializes"),
        ),
        (
            "experiment-lease.schema.json",
            serde_json::to_value(schema_for!(ExperimentLease))
                .expect("experiment lease schema serializes"),
        ),
        (
            "evidence-bundle.schema.json",
            serde_json::to_value(schema_for!(EvidenceBundle))
                .expect("evidence bundle schema serializes"),
        ),
    ]
}
