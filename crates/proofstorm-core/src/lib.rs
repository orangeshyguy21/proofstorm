//! Domain contracts shared by every Proofstorm interface.

mod backend;
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
mod validation;

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
pub use catalog::{
    AuthenticationMode, CatalogDependencySupport, CatalogEntry, CatalogFeature,
    CatalogImplementationSupport, CatalogPaymentBindingSupport, CatalogResponse,
    CatalogSupportMatrix, CatalogVersionSupport, ReleaseChannel, StorageBackend, SupportLifecycle,
    default_catalog, validate_catalog_component, validate_component_config,
};
pub use coverage::{
    CONFIGURATION_COVERAGE_API_VERSION, ConfigurationCoverageEntry, ConfigurationCoverageManifest,
    ConfigurationFieldCoverage, configuration_coverage_manifest,
};
pub use evidence::{
    EVIDENCE_API_VERSION, EVIDENCE_MEDIA_TYPE, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance,
};
pub use experiment::{Experiment, ExperimentLease, ExperimentPhase, LeasePhase};
pub use instance::{
    ComponentCondition, ComponentStatus, InstancePhase, InventoryEntry, LabInstance,
    LabInstanceStatus, MAX_COMPONENT_CONDITIONS, MAX_CONDITION_MESSAGE_BYTES, TeardownReceipt,
};
pub use model::{
    API_VERSION, AuthenticationProtocol, BitcoinNetwork, Capability, ComponentKind, ComponentSpec,
    ControlClass, DatabaseRole, DependencyBinding, LabLimits, LabPolicy, LabSpec, LinkKind,
    LinkSpec, PaymentMethod, ValidateLabRequest,
};
pub use mutation::{DraftMutation, apply_draft_mutation};
pub use network::{
    MAX_NETWORK_DELAY_MS, MAX_NETWORK_JITTER_MS, MAX_NETWORK_LOSS_BASIS_POINTS,
    NetworkFaultBackend, NetworkFaultBounds, NetworkFaultDirection, NetworkFaultFeature,
    network_policy_fault_backend,
};
pub use operation::{LabOperation, OperationArtifact, OperationKind, OperationPhase};
pub use publication::{
    EFFECTIVE_CONFIG_DIGEST_VERSION, LOCK_API_VERSION, LockEntry, PublishedRevision,
    ROLLOUT_DIGEST_VERSION, ResolvedLock, digest_json, publication_digest, resolve_effective_lab,
    resolve_lock,
};
pub use quote::{
    WalletQuoteDirection, WalletQuoteObservation, WalletQuoteObservationInput,
    WalletQuoteObservationRole, wallet_quote_observations_from_artifact,
};
pub use schema::schema_documents;
pub use validation::{ValidationIssue, ValidationReport, validate_lab};
