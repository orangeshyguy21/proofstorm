//! Deterministic evidence exports, bounded section reads and immutable resources.
use std::collections::BTreeSet;

use proofstorm_core::{
    Capability, EVIDENCE_API_VERSION, EvidenceAction, EvidenceArtifact, EvidenceBundle,
    EvidenceBundleContent, EvidenceInstance, ExperimentPhase, OperationKind, OperationPhase,
};
use rmcp::{
    ErrorData, Json,
    model::{
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    MAX_AGENT_RESPONSE_BYTES, ProofstormMcp, bounded_json_response, coded_invalid_request,
    read_query, store_error,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceExportRequest {
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    /// Include full artifact bodies for conservation and reachability oracles.
    #[serde(default = "default_true")]
    pub include_oracle_artifacts: bool,
    /// Optional full bodies for additional operations. Do not enumerate
    /// the experiment: every action and artifact descriptor is always in the journal.
    #[serde(default)]
    pub artifact_operation_ids: Vec<String>,
    /// Internal full-bundle assembly; MCP clients read the returned immutable resource.
    #[serde(skip)]
    pub include_content: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceExportResponse {
    pub media_type: String,
    pub digest: String,
    pub byte_length: u32,
    pub workspace_id: String,
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    pub revision_digest: String,
    pub lock_digest: String,
    pub journal_count: u32,
    pub artifact_count: u32,
    pub workspace_capture_count: u32,
    /// Always true: every experiment action and its artifact descriptor is in the journal.
    pub journal_complete: bool,
    /// Artifact bodies are optional enrichments; their count need not equal `journal_count`.
    pub artifact_bodies_optional: bool,
    pub guidance: String,
    /// Stable MCP resource URI for reading the complete deterministic bundle.
    pub resource_uri: String,
    pub content_included: bool,
    /// Deliberately schema-opaque bulk content, present only after explicit opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSection {
    Revision,
    Lock,
    Journal,
    Artifact,
    WorkspaceCapture,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSectionReadRequest {
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    /// Must match the selection used for the evidence manifest.
    #[serde(default = "default_true")]
    pub include_oracle_artifacts: bool,
    /// Must match the selection used for the evidence manifest.
    #[serde(default)]
    pub artifact_operation_ids: Vec<String>,
    pub section: EvidenceSection,
    /// RFC 6901 pointer within the section. Artifact data lives under `/artifact/content`
    /// (e.g. `/artifact/content/exit_code`); `/artifact/digest` reads its digest. Empty reads the whole section.
    #[serde(default)]
    pub pointer: String,
    /// Required for artifact reads and ignored for other sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Required for `workspace_capture` section reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    /// Journal sequence boundary; ignored for other sections.
    #[serde(default)]
    pub after_sequence: u64,
    /// Journal page size; ignored for other sections.
    #[serde(default = "default_evidence_section_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSectionReadResponse {
    pub evidence_digest: String,
    pub section: EvidenceSection,
    pub data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_sequence: Option<u64>,
}

const fn default_true() -> bool {
    true
}

const EVIDENCE_ACTION_PAGE_SIZE: u32 = 100;

const fn default_evidence_section_limit() -> u32 {
    20
}

impl ProofstormMcp {
    pub(super) fn export_evidence(
        &self,
        request: &EvidenceExportRequest,
    ) -> Result<Json<EvidenceExportResponse>, ErrorData> {
        let bundle = self.build_evidence_bundle(request)?;
        let resource_uri = evidence_resource_uri(request, &bundle.digest);
        Ok(Json(evidence_export_response(
            bundle,
            resource_uri,
            request.include_content,
        )))
    }

    pub(super) fn read_evidence_section(
        &self,
        request: EvidenceSectionReadRequest,
    ) -> Result<Json<EvidenceSectionReadResponse>, ErrorData> {
        let export_request = EvidenceExportRequest {
            experiment_id: request.experiment_id,
            include_oracle_artifacts: request.include_oracle_artifacts,
            artifact_operation_ids: request.artifact_operation_ids,
            include_content: false,
        };
        let bundle = self.build_evidence_bundle(&export_request)?;
        if matches!(request.section, EvidenceSection::Journal) {
            if !(1..=50).contains(&request.limit) {
                return Err(coded_invalid_request(
                    "evidence_section_limit_invalid",
                    "journal limit must be between 1 and 50",
                ));
            }
            let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
            let candidates = bundle
                .content
                .journal
                .iter()
                .filter(|action| action.sequence > request.after_sequence)
                .take(limit + 1)
                .cloned()
                .collect::<Vec<_>>();
            let source_has_more = candidates.len() > limit;
            let page_len = candidates.len().min(limit);
            let mut end = page_len;
            loop {
                let has_more = source_has_more || end < page_len;
                let response = EvidenceSectionReadResponse {
                    evidence_digest: bundle.digest.clone(),
                    section: request.section,
                    data: evidence_json(&candidates[..end])?,
                    next_after_sequence: (has_more && end > 0)
                        .then(|| candidates[end - 1].sequence),
                };
                if read_query::wire_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
                    return Ok(Json(response));
                }
                if end <= 1 {
                    return Err(coded_invalid_request(
                        "evidence_action_too_large",
                        "one evidence journal action exceeds the agent response budget",
                    ));
                }
                end -= 1;
            }
        }
        let data = match request.section {
            EvidenceSection::Revision => evidence_pointer(
                evidence_json(&bundle.content.revision)?,
                &request.pointer,
                "revision",
            )?,
            EvidenceSection::Lock => evidence_pointer(
                evidence_json(&bundle.content.revision.lock)?,
                &request.pointer,
                "lock",
            )?,
            EvidenceSection::Artifact => {
                let operation_id = request.operation_id.as_deref().ok_or_else(|| {
                    coded_invalid_request(
                        "evidence_operation_id_required",
                        "operation_id is required for an artifact section read",
                    )
                })?;
                let artifact = bundle
                    .content
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.operation_id == operation_id)
                    .ok_or_else(|| {
                        coded_invalid_request(
                            "evidence_artifact_not_selected",
                            "operation_id is not present in the selected evidence artifacts",
                        )
                    })?;
                evidence_pointer(evidence_json(artifact)?, &request.pointer, "artifact")?
            }
            EvidenceSection::WorkspaceCapture => {
                evidence_capture_section(&bundle, request.capture_id.as_deref(), &request.pointer)?
            }
            EvidenceSection::Journal => unreachable!("journal returned above"),
        };
        bounded_json_response(EvidenceSectionReadResponse {
            evidence_digest: bundle.digest,
            section: request.section,
            data,
            next_after_sequence: None,
        })
        .map(Json)
    }

    pub(super) fn read_evidence_resource(
        &self,
        request: ReadResourceRequestParams,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (export_request, expected_digest) = parse_evidence_resource_uri(&request.uri)?;
        let bundle = self.build_evidence_bundle(&export_request)?;
        if bundle.digest != expected_digest {
            return Err(ErrorData::resource_not_found(
                "evidence resource digest does not match current durable content",
                Some(serde_json::json!({"code": "evidence_digest_mismatch"})),
            ));
        }
        let text = serde_json::to_string(&bundle).map_err(|error| {
            ErrorData::internal_error(
                format!("failed to serialize evidence resource: {error}"),
                Some(serde_json::json!({"code": "evidence_serialization_failed"})),
            )
        })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri)
                .with_mime_type("application/vnd.proofstorm.evidence.v1alpha1+json"),
        ])
        .into())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "evidence admission, selection, and final size checks stay visibly atomic"
    )]
    pub(super) fn build_evidence_bundle(
        &self,
        request: &EvidenceExportRequest,
    ) -> Result<EvidenceBundle, ErrorData> {
        self.authorize_all(&[Capability::ExperimentRead, Capability::ArtifactRead])?;
        let explicit = request
            .artifact_operation_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if explicit.len() != request.artifact_operation_ids.len() {
            return Err(coded_invalid_request(
                "evidence_artifact_duplicate",
                "artifact operation IDs must be unique",
            ));
        }
        let experiment = self
            .store
            .experiment(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        if experiment.phase != ExperimentPhase::Closed {
            return Err(coded_invalid_request(
                "evidence_experiment_active",
                "evidence export requires a closed experiment",
            ));
        }
        let (instance, revision) = self
            .store
            .sealed_run_context(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        let mut actions = Vec::new();
        let mut after = 0;
        loop {
            let page = self
                .store
                .actions(
                    &self.workspace,
                    &self.principal,
                    &request.experiment_id,
                    after,
                    EVIDENCE_ACTION_PAGE_SIZE,
                )
                .map_err(store_error)?;
            let Some(last) = page.last() else {
                break;
            };
            after = last.sequence;
            let complete = page.len() < EVIDENCE_ACTION_PAGE_SIZE as usize;
            actions.extend(page);
            if complete {
                break;
            }
        }
        if actions.iter().any(|action| {
            matches!(
                action.phase,
                OperationPhase::Pending | OperationPhase::Running
            )
        }) {
            return Err(coded_invalid_request(
                "evidence_journal_incomplete",
                "all experiment actions must be terminal before evidence export",
            ));
        }
        let known_ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(unknown) = explicit.iter().find(|id| !known_ids.contains(id.as_str())) {
            return Err(coded_invalid_request(
                "evidence_artifact_unknown",
                format!("operation {unknown:?} is not in the experiment journal"),
            ));
        }
        let selected = actions
            .iter()
            .filter(|action| {
                explicit.contains(&action.id)
                    || request.include_oracle_artifacts
                        && matches!(
                            action.kind,
                            OperationKind::ConservationOracle | OperationKind::ReachabilityOracle
                        )
            })
            .collect::<Vec<_>>();
        let mut artifacts = Vec::with_capacity(selected.len());
        for action in selected {
            let artifact = action.artifact.clone().ok_or_else(|| {
                coded_invalid_request(
                    "evidence_artifact_missing",
                    format!("operation {:?} has no terminal artifact", action.id),
                )
            })?;
            artifacts.push(EvidenceArtifact {
                operation_id: action.id.clone(),
                sequence: action.sequence,
                kind: action.kind,
                artifact,
            });
        }
        let workspace_captures = self
            .store
            .workspace_captures(&self.workspace, &self.principal, &request.experiment_id)
            .map_err(store_error)?;
        let revisions = actions
            .iter()
            .map(|a| a.revision_digest.as_str())
            .chain(
                workspace_captures
                    .iter()
                    .map(|capture| capture.content.revision_digest.as_str()),
            )
            .filter(|d| !d.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|digest| {
                self.store
                    .revision_for_evidence(&self.workspace, &self.principal, digest)
                    .map_err(store_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let content = EvidenceBundleContent {
            workspace_captures,
            revisions,
            api_version: EVIDENCE_API_VERSION.to_owned(),
            workspace_id: self.workspace.clone(),
            experiment,
            instance: EvidenceInstance {
                id: instance.id,
                revision_digest: instance.revision_digest,
                lock_digest: instance.lock_digest,
            },
            revision,
            journal: actions.iter().map(EvidenceAction::from).collect(),
            artifacts,
        };
        Ok(EvidenceBundle::from_content(content))
    }
}

fn evidence_capture_section(
    bundle: &EvidenceBundle,
    capture_id: Option<&str>,
    pointer: &str,
) -> Result<serde_json::Value, ErrorData> {
    let id = capture_id.ok_or_else(|| {
        coded_invalid_request(
            "evidence_capture_id_required",
            "capture_id is required for workspace capture reads",
        )
    })?;
    let capture = bundle
        .content
        .workspace_captures
        .iter()
        .find(|capture| capture.content.capture_id == id)
        .ok_or_else(|| {
            coded_invalid_request(
                "evidence_capture_unknown",
                "capture_id is not attached to this run",
            )
        })?;
    evidence_pointer(evidence_json(capture)?, pointer, "workspace_capture")
}

fn evidence_export_response(
    bundle: EvidenceBundle,
    resource_uri: String,
    include_content: bool,
) -> EvidenceExportResponse {
    EvidenceExportResponse {
        media_type: bundle.media_type,
        digest: bundle.digest,
        byte_length: bundle.byte_length,
        workspace_id: bundle.content.workspace_id.clone(),
        experiment_id: bundle.content.experiment.id.clone(),
        revision_digest: bundle.content.instance.revision_digest.clone(),
        lock_digest: bundle.content.instance.lock_digest.clone(),
        journal_count: u32::try_from(bundle.content.journal.len()).unwrap_or(u32::MAX),
        artifact_count: u32::try_from(bundle.content.artifacts.len()).unwrap_or(u32::MAX),
        workspace_capture_count: u32::try_from(bundle.content.workspace_captures.len()).unwrap_or(u32::MAX),
        journal_complete: true,
        artifact_bodies_optional: true,
        guidance: "Evidence is complete: the journal covers every action and artifact descriptor. Do not retry merely to make artifact_count equal journal_count; explicit artifact IDs only embed optional full bodies."
            .into(),
        resource_uri,
        content_included: include_content,
        content: include_content
            .then(|| serde_json::to_value(bundle.content).expect("typed evidence serializes")),
    }
}

fn evidence_resource_uri(request: &EvidenceExportRequest, digest: &str) -> String {
    let mut artifact_ids = request.artifact_operation_ids.clone();
    artifact_ids.sort();
    format!(
        "proofstorm://evidence/{}/{}?oracles={}&artifacts={}",
        request.experiment_id,
        digest,
        u8::from(request.include_oracle_artifacts),
        artifact_ids.join(",")
    )
}

fn parse_evidence_resource_uri(uri: &str) -> Result<(EvidenceExportRequest, String), ErrorData> {
    let remainder = uri
        .strip_prefix("proofstorm://evidence/")
        .ok_or_else(|| ErrorData::resource_not_found("unknown Proofstorm resource URI", None))?;
    let (path, query) = remainder
        .split_once('?')
        .ok_or_else(|| ErrorData::resource_not_found("invalid evidence resource URI", None))?;
    let (experiment_id, digest) = path
        .split_once('/')
        .filter(|(experiment_id, digest)| {
            !experiment_id.is_empty() && !digest.is_empty() && !digest.contains('/')
        })
        .ok_or_else(|| ErrorData::resource_not_found("invalid evidence resource URI", None))?;
    let mut oracles = None;
    let mut artifacts = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| ErrorData::resource_not_found("invalid evidence resource URI", None))?;
        match key {
            "oracles" if oracles.is_none() => {
                oracles = Some(match value {
                    "0" => false,
                    "1" => true,
                    _ => {
                        return Err(ErrorData::resource_not_found(
                            "invalid evidence resource URI",
                            None,
                        ));
                    }
                });
            }
            "artifacts" if artifacts.is_none() => {
                artifacts = Some(if value.is_empty() {
                    Vec::new()
                } else {
                    value.split(',').map(str::to_owned).collect()
                });
            }
            _ => {
                return Err(ErrorData::resource_not_found(
                    "invalid evidence resource URI",
                    None,
                ));
            }
        }
    }
    Ok((
        EvidenceExportRequest {
            experiment_id: experiment_id.to_owned(),
            include_oracle_artifacts: oracles.ok_or_else(|| {
                ErrorData::resource_not_found("invalid evidence resource URI", None)
            })?,
            artifact_operation_ids: artifacts.ok_or_else(|| {
                ErrorData::resource_not_found("invalid evidence resource URI", None)
            })?,
            include_content: false,
        },
        digest.to_owned(),
    ))
}

fn evidence_json<T: Serialize + ?Sized>(value: &T) -> Result<serde_json::Value, ErrorData> {
    serde_json::to_value(value).map_err(|error| {
        ErrorData::internal_error(
            format!("failed to serialize evidence section: {error}"),
            Some(serde_json::json!({"code": "evidence_serialization_failed"})),
        )
    })
}

fn evidence_pointer(
    value: serde_json::Value,
    pointer: &str,
    section: &str,
) -> Result<serde_json::Value, ErrorData> {
    if pointer.is_empty() {
        return Ok(value);
    }
    if !pointer.starts_with('/') {
        return Err(coded_invalid_request(
            "evidence_pointer_invalid",
            "JSON Pointer must be empty or start with '/'",
        ));
    }
    value.pointer(pointer).cloned().ok_or_else(|| {
        let mut parent = pointer;
        while value.pointer(parent).is_none() {
            parent = parent.rsplit_once('/').map_or("", |(prefix, _)| prefix);
        }
        let available = value.pointer(parent).expect("existing pointer ancestor")
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .take(16)
                    .map(|key| format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1")))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|keys| !keys.is_empty())
            .unwrap_or_else(|| "the empty pointer for the whole section".to_owned());
        coded_invalid_request(
            "evidence_pointer_not_found",
            format!(
                "[evidence_pointer_not_found] JSON Pointer {pointer:?} does not exist in the {section} section; no changes were made. Recovery: choose one of the available pointers beneath {parent:?} ({available}), or use an empty pointer to read the whole section"
            ),
        )
    })
}
