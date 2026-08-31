//! Domain contracts shared by every Proofstorm interface.

mod catalog;
mod evidence;
mod experiment;
mod instance;
mod model;
mod mutation;
mod network;
mod operation;
mod publication;
mod quote;
mod schema;
mod validation;

pub use catalog::{
    CatalogEntry, CatalogResponse, default_catalog, validate_catalog_component,
    validate_component_config,
};
pub use evidence::{
    EVIDENCE_API_VERSION, EVIDENCE_MEDIA_TYPE, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance,
};
pub use experiment::{Experiment, ExperimentLease, ExperimentPhase, LeasePhase};
pub use instance::{
    ComponentStatus, InstancePhase, InventoryEntry, LabInstance, LabInstanceStatus, TeardownReceipt,
};
pub use model::{
    API_VERSION, Capability, ComponentKind, ComponentSpec, ControlClass, LabLimits, LabPolicy,
    LabSpec, LinkKind, LinkSpec, ValidateLabRequest,
};
pub use mutation::{DraftMutation, apply_draft_mutation};
pub use network::{
    MAX_NETWORK_DELAY_MS, MAX_NETWORK_JITTER_MS, MAX_NETWORK_LOSS_BASIS_POINTS,
    NetworkFaultBackend, NetworkFaultBounds, NetworkFaultDirection, NetworkFaultFeature,
    network_policy_fault_backend,
};
pub use operation::{LabOperation, OperationArtifact, OperationKind, OperationPhase};
pub use publication::{
    LockEntry, PublishedRevision, ResolvedLock, digest_json, publication_digest, resolve_lock,
};
pub use quote::{WalletQuote, WalletQuoteDirection, WalletQuotePhase};
pub use schema::schema_documents;
pub use validation::{ValidationIssue, ValidationReport, validate_lab};
