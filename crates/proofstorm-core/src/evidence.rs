use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Capability, Experiment, LabOperation, OperationArtifact, OperationKind, OperationPhase,
    PublishedRevision, digest_json,
};

pub const EVIDENCE_API_VERSION: &str = "proofstorm/evidence/v1alpha1";
pub const EVIDENCE_MEDIA_TYPE: &str = "application/vnd.proofstorm.evidence.v1alpha1+json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundleContent {
    pub api_version: String,
    pub workspace_id: String,
    pub experiment: Experiment,
    pub instance: EvidenceInstance,
    pub revision: PublishedRevision,
    pub journal: Vec<EvidenceAction>,
    pub artifacts: Vec<EvidenceArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceInstance {
    pub id: String,
    pub revision_digest: String,
    pub lock_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAction {
    pub id: String,
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub principal_id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub capability: Capability,
    pub request_digest: String,
    pub request: Value,
    pub phase: OperationPhase,
    pub accepted_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_byte_length: Option<u32>,
}

impl From<&LabOperation> for EvidenceAction {
    fn from(operation: &LabOperation) -> Self {
        Self {
            id: operation.id.clone(),
            instance_id: operation.instance_id.clone(),
            experiment_id: operation.experiment_id.clone(),
            lease_id: operation.lease_id.clone(),
            principal_id: operation.principal_id.clone(),
            sequence: operation.sequence,
            kind: operation.kind,
            capability: operation.capability,
            request_digest: operation.request_digest.clone(),
            request: operation.request.clone(),
            phase: operation.phase,
            accepted_at_unix: operation.accepted_at_unix,
            started_at_unix: operation.started_at_unix,
            completed_at_unix: operation.completed_at_unix,
            artifact_digest: operation
                .artifact
                .as_ref()
                .map(|artifact| artifact.digest.clone()),
            artifact_media_type: operation
                .artifact
                .as_ref()
                .map(|artifact| artifact.media_type.clone()),
            artifact_byte_length: operation
                .artifact
                .as_ref()
                .map(|artifact| artifact.byte_length),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceArtifact {
    pub operation_id: String,
    pub sequence: u64,
    pub kind: OperationKind,
    pub artifact: OperationArtifact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundle {
    pub media_type: String,
    pub digest: String,
    pub byte_length: u32,
    pub content: EvidenceBundleContent,
}

impl EvidenceBundle {
    #[must_use]
    /// Wrap typed evidence content with its deterministic digest and byte length.
    ///
    /// # Panics
    ///
    /// Panics only if typed evidence content cannot be represented as JSON.
    pub fn from_content(content: EvidenceBundleContent) -> Self {
        let encoded = serde_json::to_vec(&content).expect("typed evidence content serializes");
        Self {
            media_type: EVIDENCE_MEDIA_TYPE.to_owned(),
            digest: digest_json(&content),
            byte_length: u32::try_from(encoded.len()).unwrap_or(u32::MAX),
            content,
        }
    }
}
