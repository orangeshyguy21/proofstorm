//! Managed workspace tasks. These outlive the bounded command used to control them.
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[path = "workspace_control.rs"]
pub mod control;
#[path = "workspace_evidence.rs"]
pub mod evidence;
#[path = "workspace_upload.rs"]
pub mod upload;
#[path = "workspace_wire.rs"]
pub mod wire;

pub const WORKSPACE_PATH: &str = "/workspace";
pub const WORKSPACE_RUNNER: &str = "/opt/proofstorm/workspace";
pub const MAX_TASKS: usize = 128;
pub const MAX_ACTIVE_TASKS: usize = 16;
pub const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_SOURCE_FILES: usize = 512;
pub const MAX_FILE_WRITE_BYTES: usize = 8192;
pub const LOG_SEGMENT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskStart {
    /// Optional authority for calls through the controller. Fixed for the lifetime of this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<control::ControlScope>,
    /// Stable ID for this task. Exact retries return the original task; changed input is refused.
    pub task_id: String,
    /// Snapshot this workspace-relative directory before starting. Defaults to src.
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub argv: Vec<String>,
    /// Shell script instead of argv. Runs in the captured source directory.
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Omit to run until stopped. A deadline is measured from process start.
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
}

fn default_source() -> String {
    "src".into()
}

impl TaskStart {
    /// # Errors
    /// Rejects invalid commands, IDs, paths, deadlines or environment bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(scope) = &self.control {
            scope.validate()?;
        }
        validate_task_id(&self.task_id)?;
        validate_path(&self.source)?;
        crate::native::NativeCommand {
            private_io: None,
            script: self.script.clone(),
            argv: self.argv.clone(),
            timeout_seconds: 300,
            output: crate::native::NativeOutput::default(),
        }
        .validate()?;
        if self.timeout_seconds == Some(0) {
            return Err("task timeout must be positive or omitted");
        }
        if self.env.len() > 64
            || self.env.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 128
                    || key.starts_with("PROOFSTORM_")
                    || !key.bytes().enumerate().all(|(i, b)| {
                        b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                    })
                    || value.contains('\0')
                    || value.len() > 4096
            })
        {
            return Err("invalid task environment; PROOFSTORM_ variables are reserved");
        }
        if self
            .env
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            > 8192
        {
            return Err("task environment exceeds 8192 bytes");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskRequest {
    Start(TaskStart),
    Status {
        task_id: String,
    },
    Stop {
        task_id: String,
    },
    /// Pages are ordered by task ID. Pass `next_after` as after.
    List {
        #[serde(default)]
        after: Option<String>,
    },
    /// Explicitly read raw task output; it can contain secrets. Returns the recent bounded tail.
    Logs {
        task_id: String,
        #[serde(default)]
        stream: LogStream,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    #[default]
    Stdout,
    Stderr,
}

impl LogStream {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRequest {
    /// Write UTF-8 text atomically. Parent directories are created. Files under .proofstorm are private to the supervisor.
    Write { path: String, content: String },
    /// Read a bounded byte slice as UTF-8 text (invalid byte sequences are replaced).
    Read {
        path: String,
        #[serde(default)]
        offset: u64,
    },
    /// List a directory. Pass `next_after` as after for more entries.
    List {
        /// Omit or leave empty to list the workspace root.
        #[serde(default)]
        path: String,
        #[serde(default)]
        after: Option<String>,
    },
    /// Remove one regular file. Does not remove directories or follow symlinks.
    Remove { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WorkspaceRequest {
    Task(TaskRequest),
    File(FileRequest),
    Upload(upload::UploadRequest),
    Ping,
    Capture(evidence::CaptureRequest),
    ReleaseCapture { capture_id: String },
}

impl WorkspaceRequest {
    /// # Errors
    /// Rejects invalid task input, unsafe paths or oversized file writes.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Upload(request) => request.validate(),
            Self::Capture(request) => request.validate(),
            Self::ReleaseCapture { capture_id } => validate_task_id(capture_id),
            Self::Task(TaskRequest::Start(start)) => start.validate(),
            Self::Task(
                TaskRequest::Status { task_id }
                | TaskRequest::Stop { task_id }
                | TaskRequest::Logs { task_id, .. },
            ) => validate_task_id(task_id),
            Self::File(FileRequest::Write { path, content }) => {
                validate_path(path)?;
                if content.len() > MAX_FILE_WRITE_BYTES {
                    return Err("file write exceeds 8192 bytes");
                }
                Ok(())
            }
            Self::File(FileRequest::List { path, .. }) if path.is_empty() => Ok(()),
            Self::File(
                FileRequest::Read { path, .. }
                | FileRequest::List { path, .. }
                | FileRequest::Remove { path },
            ) => validate_path(path),
            _ => Ok(()),
        }
    }
}

/// # Errors
/// Rejects IDs that cannot be used as single task directory names.
pub fn validate_task_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("task_id must contain 1..=64 ASCII letters, digits, '_' or '-'");
    }
    Ok(())
}

/// # Errors
/// Rejects absolute paths, traversal, empty segments and reserved supervisor paths.
pub fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty()
        || path.len() > 512
        || path.contains('\0')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s == ".proofstorm")
    {
        return Err("path must be relative, without empty, '.', '..' or '.proofstorm' segments");
    }
    Ok(())
}

/// Validate a fully qualified custom workspace runtime image.
#[must_use]
pub fn is_runtime_image(image: &str) -> bool {
    let Some((repository, digest)) = image.split_once("@sha256:") else {
        return false;
    };
    let registry = repository.split('/').next().unwrap_or_default();
    (registry.contains('.') || registry.contains(':') || registry == "localhost")
        && image.len() <= 512
        && repository.contains('/')
        && !repository.starts_with(['/', '-'])
        && repository
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"./:_-".contains(&b))
        && digest.len() == 64
        && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_allow_unbounded_time_without_changing_native_execution() {
        let mut start: TaskStart =
            serde_json::from_value(serde_json::json!({"task_id":"miner", "script":"sleep 301"}))
                .unwrap();
        start.validate().unwrap();
        start.timeout_seconds = Some(3600);
        start.validate().unwrap();
        start.timeout_seconds = Some(0);
        assert!(start.validate().is_err());
        let native = crate::native::NativeCommand {
            private_io: None,
            script: "sleep 301".into(),
            argv: vec![],
            timeout_seconds: 301,
            output: crate::native::NativeOutput::default(),
        };
        assert!(native.validate().is_err());
    }

    #[test]
    fn paths_and_reserved_environment_are_rejected_before_execution() {
        for path in [
            "/etc/passwd",
            "../src",
            "src/../state",
            ".proofstorm/tasks",
            "src//a",
            "src/./a",
        ] {
            assert!(validate_path(path).is_err(), "{path}");
        }
        validate_path("src/helpers/mining.sh").unwrap();
        let start: TaskStart = serde_json::from_value(serde_json::json!({"task_id":"miner", "script":"true", "env":{"PROOFSTORM_OUTPUT":"elsewhere"}})).unwrap();
        assert!(start.validate().is_err());
    }
}
