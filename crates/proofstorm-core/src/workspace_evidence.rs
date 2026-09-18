//! Immutable task captures, attached explicitly before a run is sealed.
use super::{TaskStart, validate_path, validate_task_id};
use base64::{Engine, engine::general_purpose::STANDARD};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_CAPTURE_BYTES: usize = 40 * 1024 * 1024;
pub const MAX_CAPTURE_FILE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_CAPTURE_FILES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureSelection {
    pub task_id: String,
    /// Exact regular-file paths relative to `output/<task_id>`. No directory expansion.
    #[serde(default)]
    pub output_paths: Vec<String>,
    /// Include the retained stdout/stderr segments. Logs may contain raw private data.
    #[serde(default)]
    pub include_logs: bool,
}

impl CaptureSelection {
    /// # Errors
    /// Refuses unsafe or duplicate output paths and oversized selections.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_task_id(&self.task_id)?;
        if self.output_paths.len() > 64 {
            return Err("capture allows at most 64 selected output files");
        }
        let mut unique = BTreeSet::new();
        for path in &self.output_paths {
            validate_path(path)?;
            validate_path(&format!("output/{}/{path}", self.task_id))?;
            if path == "control" || path.starts_with("control/") || !unique.insert(path) {
                return Err(
                    "duplicate or reserved output path; control records are captured separately",
                );
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| "capture selection encoding failed")?
            .len()
            > 8192
        {
            return Err("capture selection exceeds 8192 bytes");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub capture_id: String,
    pub request_digest: String,
    pub selection: CaptureSelection,
}

impl CaptureRequest {
    /// # Errors
    /// Refuses invalid capture identifiers or selections.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_task_id(&self.capture_id)?;
        validate_digest(&self.request_digest)?;
        self.selection.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapturedFile {
    pub path: String,
    pub executable_bits: u32,
    pub byte_length: u64,
    pub sha256: String,
    /// Standard base64 preserves arbitrary binary output exactly.
    pub content_base64: String,
}

impl CapturedFile {
    #[must_use]
    pub fn from_bytes(path: String, executable_bits: u32, bytes: &[u8]) -> Self {
        Self {
            path,
            executable_bits,
            byte_length: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            content_base64: STANDARD.encode(bytes),
        }
    }

    /// # Errors
    /// Refuses corrupt, oversized or mismatched file bodies.
    pub fn bytes(&self) -> Result<Vec<u8>, &'static str> {
        if self.byte_length > MAX_CAPTURE_FILE_BYTES as u64
            || self.content_base64.len() > MAX_CAPTURE_BYTES
        {
            return Err("captured file exceeds byte limit");
        }
        let bytes = STANDARD
            .decode(&self.content_base64)
            .map_err(|_| "invalid captured base64")?;
        if bytes.len() as u64 != self.byte_length
            || format!("{:x}", Sha256::digest(&bytes)) != self.sha256
        {
            return Err("captured file digest or length mismatch");
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskCapture {
    pub capture_id: String,
    pub capture_request_digest: String,
    pub selection: CaptureSelection,
    pub observed_at_unix: u64,
    pub task: Value,
    pub request: TaskStart,
    /// A sequential observation, not an atomic filesystem or application checkpoint.
    pub files: Vec<CapturedFile>,
}

impl TaskCapture {
    /// # Errors
    /// Refuses incomplete, corrupt or identity-mismatched snapshots.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_task_id(&self.capture_id)?;
        validate_digest(&self.capture_request_digest)?;
        self.selection.validate()?;
        self.request.validate()?;
        if self.request.task_id != self.selection.task_id
            || self.task["task_id"] != self.selection.task_id
            || self.task["request_digest"] != crate::digest_json(&self.request)
            || self.files.len() > MAX_CAPTURE_FILES
        {
            return Err("captured task identity or size mismatch");
        }
        let mut names = BTreeSet::new();
        let mut source = BTreeMap::new();
        let mut outputs = BTreeSet::new();
        let mut bytes = 0;
        for file in &self.files {
            validate_path(&file.path)?;
            if !names.insert(&file.path) || file.executable_bits & !0o111 != 0 {
                return Err("invalid captured file path or mode");
            }
            bytes += file.bytes()?.len();
            if bytes > MAX_CAPTURE_FILE_BYTES {
                return Err("capture exceeds 24 MiB of file content");
            }
            if let Some(path) = file.path.strip_prefix("source/") {
                source.insert(
                    path,
                    serde_json::json!({"sha256":file.sha256,"mode":file.executable_bits}),
                );
            } else if let Some(path) = file.path.strip_prefix("output/") {
                outputs.insert(path.to_owned());
            } else if !(file.path.starts_with("control/")
                || self.selection.include_logs && file.path.starts_with("logs/"))
            {
                return Err("unexpected captured file group");
            }
        }
        if crate::digest_json(&source) != self.task["source_digest"]
            || outputs != self.selection.output_paths.iter().cloned().collect()
        {
            return Err("source changed since task start or selected outputs are incomplete");
        }
        Ok(())
    }
}

fn validate_digest(digest: &str) -> Result<(), &'static str> {
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("capture request digest must be SHA-256");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEvidenceContent {
    pub capture_id: String,
    pub run_id: String,
    pub principal_id: String,
    pub instance_id: String,
    pub instance_key: String,
    pub revision_digest: String,
    pub component: String,
    pub workspace_pod_uid: String,
    pub snapshot: TaskCapture,
    /// Later controller observation; snapshots never claim a distributed atomic checkpoint.
    pub controller_observed_at_unix: i64,
    pub controller_actions: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEvidence {
    pub digest: String,
    pub byte_length: u32,
    pub content: WorkspaceEvidenceContent,
}

impl WorkspaceEvidence {
    /// # Errors
    /// Refuses oversized or corrupt captures before they enter the evidence store.
    pub fn new(content: WorkspaceEvidenceContent) -> Result<Self, &'static str> {
        content.snapshot.validate()?;
        if content.capture_id != content.snapshot.capture_id {
            return Err("capture identity mismatch");
        }
        let bytes = serde_json::to_vec(&content).map_err(|_| "capture serialization failed")?;
        if bytes.len() > MAX_CAPTURE_BYTES {
            return Err("workspace evidence exceeds 40 MiB");
        }
        Ok(Self {
            digest: crate::digest_json(&content),
            byte_length: u32::try_from(bytes.len()).map_err(|_| "capture length overflow")?,
            content,
        })
    }

    /// # Errors
    /// Refuses modified evidence envelopes or content.
    pub fn validate(&self) -> Result<(), &'static str> {
        if &Self::new(self.content.clone())? != self {
            return Err("workspace evidence digest mismatch");
        }
        Ok(())
    }
}
