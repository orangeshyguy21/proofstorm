//! Domain contracts shared by every Proofstorm interface.

mod backend;
mod candidate;
mod catalog;
mod coverage;
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
mod update;
mod validation;
mod wallet_builds;
pub use update::{CellChanges, CellUpdatePlan, CellUpdateTarget};

pub use backend::{
    BackendContractRegistry, BitcoinCoreConfig, CdkMintConfig, ClnConfig, ComponentBackendContract,
    ComponentConditionReason, ComponentConditionState, ComponentConditionType,
    ComponentPlanContract, ComponentPlanInput, ConditionAggregationContract, ConfigDefault,
    ConfigFieldContract, ConfigRule, ConfigSettingClass, ConfigValueKind,
    CredentialObservationContract, EffectiveComponentConfig, ExecutionContextContract,
    ExecutionMountContract, ExecutionMountTemplateContract, ExecutionStorageSource,
    ExecutionStorageTemplateSource, KeycloakConfig, LinkedStateObservationContract, LndConfig,
    NutshellMintConfig, OperationAdmissionContract, OperationClass, PostgresConfig,
    ProtocolProbeContract, ProtocolProbePlan, ReadinessPrerequisite, RedisConfig,
    StorageObservationContract, StorageRequirementTemplate, TargetDescriptorContract,
    WorkloadControllerKind, WorkloadObservationContract, default_backend_registry,
};
pub use candidate::{
    CANDIDATE_BUILD_API_VERSION, CandidateBuild, CandidateBuildPhase, CandidateSource,
    candidate_catalog_entry, effective_catalog,
};
pub use catalog::{
    AuthenticationMode, BuildProvenance, CatalogDependencySupport, CatalogEntry, CatalogFeature,
    CatalogImplementationSupport, CatalogPaymentBindingSupport, CatalogPlatform, CatalogResponse,
    CatalogRuntimeEndpoint, CatalogSupportMatrix, CatalogVersionSupport, ReleaseChannel,
    StorageBackend, SupportLifecycle, catalog_for_platform, default_catalog,
    validate_catalog_component, validate_component_config,
};
pub use coverage::{
    CONFIGURATION_COVERAGE_API_VERSION, ConfigurationCoverageEntry, ConfigurationCoverageManifest,
    ConfigurationFieldCoverage, configuration_coverage_manifest,
};
pub use evidence::{
    EVIDENCE_API_VERSION, EVIDENCE_MEDIA_TYPE, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance,
};
pub use experiment::{
    Experiment, ExperimentPhase, PrivateAccessGrant, PrivateReceiveCommand, PrivateTransferScope,
    Session, SessionPhase,
};
pub use instance::{
    CellInstance, CellInstanceStatus, ComponentCondition, ComponentStatus, InstancePhase,
    InventoryEntry, MAX_COMPONENT_CONDITIONS, MAX_CONDITION_MESSAGE_BYTES, TeardownReceipt,
};
pub use model::{
    API_VERSION, AuthenticationProtocol, BitcoinNetwork, Capability, CellLimits, CellPolicy,
    CellSpec, ComponentKind, ComponentSpec, ControlClass, DatabaseRole, DependencyBinding,
    LinkKind, LinkSpec, PaymentMethod, ValidateCellRequest,
};
pub use mutation::{DraftMutation, apply_draft_mutation};
pub use network::{
    MAX_NETWORK_DELAY_MS, MAX_NETWORK_JITTER_MS, MAX_NETWORK_LOSS_BASIS_POINTS,
    NetworkFaultBackend, NetworkFaultBounds, NetworkFaultDirection, NetworkFaultFeature,
    network_policy_fault_backend,
};
pub use operation::{CellOperation, OperationArtifact, OperationKind, OperationPhase};
pub use publication::{
    EFFECTIVE_CONFIG_DIGEST_VERSION, LOCK_API_VERSION, LockEntry, PublishedRevision,
    ROLLOUT_DIGEST_VERSION, ResolvedLock, digest_json, publication_digest, resolve_effective_cell,
    resolve_lock,
};
pub use quote::{
    WalletQuoteDirection, WalletQuoteObservation, WalletQuoteObservationInput,
    WalletQuoteObservationRole, wallet_quote_observations_from_artifact,
};
pub use schema::schema_documents;
pub use validation::{ValidationIssue, ValidationReport, validate_cell};
pub mod native;

pub mod private_io;
