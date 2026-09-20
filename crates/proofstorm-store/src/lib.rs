#![allow(
    clippy::missing_errors_doc,
    reason = "all public store operations return the documented StoreError contract"
)]

mod environment;
pub use environment::{EnvironmentEntry, PendingObservationPage};
mod delegation;
mod onboarding;
mod previews;
mod runs;
mod workspace_evidence;
pub use previews::CellPreview;
mod session_directory;
#[cfg(test)]
mod session_tests;
mod sessions;
pub use session_directory::{SessionFilters, SessionWindow};
pub use sessions::SessionPage;
mod cells;
mod lifecycle;
pub use lifecycle::{LifecycleGuard, RuntimeBinding};
mod updates;
pub use cells::{CellHandle, CellHandlePhase};
pub use updates::CellUpdateState;

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use proofstorm_core::{
    CandidateBuild, CandidateBuildPhase, Capability, CatalogResponse, CellInstance, CellOperation,
    CellSpec, Experiment, ExperimentPhase, OperationArtifact, OperationKind, OperationPhase,
    PublishedRevision, default_catalog, effective_catalog, resolve_effective_cell, resolve_lock,
    validate_cell,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const MAX_ARTIFACT_BYTES: usize = 32 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("{code}: {message}")]
    CellUpdate { code: &'static str, message: String },
    #[error("filesystem failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("store failure: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization failure: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("{resource} {id:?} was not found")]
    NotFound { resource: &'static str, id: String },
    #[error("principal {principal:?} lacks {capability:?} in workspace {workspace:?}")]
    AccessDenied {
        workspace: String,
        principal: String,
        capability: Capability,
    },
    #[error("draft {draft:?} expected version {expected}, current version is {actual}")]
    StaleDraft {
        draft: String,
        expected: u64,
        actual: u64,
    },
    #[error("idempotency key {key:?} was reused with a different request")]
    IdempotencyConflict { key: String },
    #[error("{resource} {id:?} already exists with different immutable identity")]
    Conflict { resource: &'static str, id: String },
    #[error("cell validation failed: {0}")]
    Validation(String),
    #[error("catalog resolution failed: {0}")]
    Catalog(String),
    #[error("store mutex was poisoned")]
    Poisoned,
    #[error("version {0} cannot be represented by SQLite")]
    VersionOverflow(u64),
    #[error("SQLite contained invalid negative version {0}")]
    InvalidStoredVersion(i64),
    #[error("operation artifact is {actual} bytes; maximum is {maximum}")]
    ArtifactTooLarge { actual: usize, maximum: usize },
    #[error("operation {operation:?} belongs to principal {owner:?}, not {principal:?}")]
    OperationOwnerMismatch {
        operation: String,
        owner: String,
        principal: String,
    },
}

impl StoreError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CellUpdate { code, .. } => code,
            Self::Io(_)
            | Self::Database(_)
            | Self::Serialization(_)
            | Self::Poisoned
            | Self::VersionOverflow(_)
            | Self::InvalidStoredVersion(_) => "store_failure",
            Self::NotFound { .. } => "not_found",
            Self::AccessDenied { .. } => "access_denied",
            Self::StaleDraft { .. } => "stale_draft",
            Self::IdempotencyConflict { .. } => "idempotency_conflict",
            Self::Conflict { .. } => "conflict",
            Self::Validation(_) => "validation_failed",
            Self::Catalog(_) => "catalog_resolution_failed",
            Self::ArtifactTooLarge { .. } => "artifact_too_large",
            Self::OperationOwnerMismatch { .. } => "operation_owner_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub id: String,
    pub workspace_id: String,
    pub version: u64,
    pub cell: CellSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftDiff {
    pub from_version: u64,
    pub to_version: u64,
    pub added_components: Vec<String>,
    pub removed_components: Vec<String>,
    pub links_changed: bool,
    pub policy_changed: bool,
}

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    context_id: Arc<String>,
    context_sessions: Arc<Mutex<BTreeSet<(String, String)>>>,
    lifecycle_busy: Arc<std::sync::atomic::AtomicBool>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    pub fn memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete SQLite schema is intentionally visible as one atomic initialization contract"
    )]
    fn from_connection(connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS workspaces (
               id TEXT PRIMARY KEY, name TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS cell_handles (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               name TEXT NOT NULL, generation INTEGER NOT NULL,
               owner TEXT NOT NULL, config_digest TEXT NOT NULL,
               phase TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               PRIMARY KEY(workspace_id, name)
             );
             CREATE TABLE IF NOT EXISTS principals (
               id TEXT PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS grants (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               principal_id TEXT NOT NULL REFERENCES principals(id),
               capability TEXT NOT NULL,
               PRIMARY KEY (workspace_id, principal_id, capability)
             );
             CREATE TABLE IF NOT EXISTS drafts (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               version INTEGER NOT NULL,
               cell_json TEXT NOT NULL,
               PRIMARY KEY (workspace_id, id)
             );
             CREATE TABLE IF NOT EXISTS candidate_builds (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               principal_id TEXT NOT NULL REFERENCES principals(id),
               resource_name TEXT NOT NULL UNIQUE,
               request_digest TEXT NOT NULL,
               build_json TEXT NOT NULL,
               accepted_at INTEGER NOT NULL,
               PRIMARY KEY (workspace_id, id)
             );
             CREATE TABLE IF NOT EXISTS candidate_directory_versions (workspace_id TEXT PRIMARY KEY, generation INTEGER NOT NULL);
             CREATE TRIGGER IF NOT EXISTS candidate_directory_insert AFTER INSERT ON candidate_builds BEGIN
               INSERT INTO candidate_directory_versions VALUES (NEW.workspace_id,1) ON CONFLICT(workspace_id) DO UPDATE SET generation=generation+1;
             END;
             CREATE TRIGGER IF NOT EXISTS candidate_directory_update AFTER UPDATE ON candidate_builds BEGIN
               INSERT INTO candidate_directory_versions VALUES (NEW.workspace_id,1) ON CONFLICT(workspace_id) DO UPDATE SET generation=generation+1;
             END;
             CREATE TABLE IF NOT EXISTS revisions (
               digest TEXT PRIMARY KEY,
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               draft_id TEXT NOT NULL,
               draft_version INTEGER NOT NULL,
               revision_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS instances (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               revision_digest TEXT NOT NULL REFERENCES revisions(digest),
               lock_digest TEXT NOT NULL,
               instance_key TEXT NOT NULL UNIQUE,
               resource_name TEXT NOT NULL UNIQUE,
               PRIMARY KEY (workspace_id, id)
             );
             CREATE TABLE IF NOT EXISTS experiments (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               owner_principal_id TEXT NOT NULL REFERENCES principals(id),
               phase_json TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               closed_at INTEGER,
               PRIMARY KEY (workspace_id, id),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id)
             );
             CREATE TABLE IF NOT EXISTS sessions (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               principal_id TEXT NOT NULL REFERENCES principals(id),
               phase_json TEXT NOT NULL,
               started_at INTEGER NOT NULL,
               last_activity_at INTEGER NOT NULL,
               finished_at INTEGER,
               PRIMARY KEY (workspace_id, id),
               FOREIGN KEY (workspace_id, experiment_id) REFERENCES experiments(workspace_id, id),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id)
             );
             CREATE TABLE IF NOT EXISTS actions (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               session_id TEXT NOT NULL,
               principal_id TEXT NOT NULL,
               sequence INTEGER NOT NULL,
               kind_json TEXT NOT NULL,
               capability_json TEXT NOT NULL,
               resource_name TEXT NOT NULL UNIQUE,
               request_digest TEXT NOT NULL,
               request_json TEXT NOT NULL,
               phase_json TEXT NOT NULL,
               artifact_json TEXT,
               accepted_at INTEGER NOT NULL,
               started_at INTEGER,
               completed_at INTEGER,
               PRIMARY KEY (workspace_id, id),
               UNIQUE (workspace_id, experiment_id, sequence),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id),
               FOREIGN KEY (workspace_id, experiment_id) REFERENCES experiments(workspace_id, id),
               FOREIGN KEY (workspace_id, session_id) REFERENCES sessions(workspace_id, id)
             );
             CREATE INDEX IF NOT EXISTS actions_by_instance_activity
               ON actions(workspace_id, instance_id, accepted_at DESC, id DESC);
             CREATE INDEX IF NOT EXISTS sessions_by_instance
               ON sessions(workspace_id, instance_id, id);
             CREATE TABLE IF NOT EXISTS private_access_grants (
               workspace_id TEXT NOT NULL, id TEXT NOT NULL, grant_json TEXT NOT NULL,
               PRIMARY KEY(workspace_id,id)
             );
             CREATE TABLE IF NOT EXISTS idempotency (
               workspace_id TEXT NOT NULL,
               principal_id TEXT NOT NULL,
               key TEXT NOT NULL,
               operation TEXT NOT NULL,
               request_hash TEXT NOT NULL,
               response_json TEXT NOT NULL,
               PRIMARY KEY (workspace_id, principal_id, key)
             );",
        )?;
        updates::initialize_schema(&connection)?;
        previews::initialize_schema(&connection)?;
        runs::initialize_schema(&connection)?;
        workspace_evidence::initialize_schema(&connection)?;
        lifecycle::initialize_schema(&connection)?;
        Ok(Self {
            lifecycle_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            context_sessions: Arc::new(Mutex::new(BTreeSet::new())),
            context_id: Arc::new(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn put_workspace(&self, workspace: &Workspace) -> Result<(), StoreError> {
        self.lock()?.execute(
            "INSERT INTO workspaces(id, name) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![workspace.id, workspace.name],
        )?;
        Ok(())
    }

    pub fn put_principal(&self, principal: &str) -> Result<(), StoreError> {
        self.lock()?.execute(
            "INSERT OR IGNORE INTO principals(id) VALUES (?1)",
            [principal],
        )?;
        Ok(())
    }

    pub fn grant(
        &self,
        workspace: &str,
        principal: &str,
        capability: Capability,
    ) -> Result<(), StoreError> {
        self.lock()?.execute(
            "INSERT OR IGNORE INTO grants(workspace_id, principal_id, capability) VALUES (?1, ?2, ?3)",
            params![workspace, principal, capability_name(capability)?],
        )?;
        Ok(())
    }

    pub fn revoke(
        &self,
        workspace: &str,
        principal: &str,
        capability: Capability,
    ) -> Result<(), StoreError> {
        self.lock()?.execute(
            "DELETE FROM grants WHERE workspace_id = ?1 AND principal_id = ?2 AND capability = ?3",
            params![workspace, principal, capability_name(capability)?],
        )?;
        Ok(())
    }

    pub fn replace_grants(
        &self,
        workspace: &str,
        principal: &str,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Result<(), StoreError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM grants WHERE workspace_id = ?1 AND principal_id = ?2",
            params![workspace, principal],
        )?;
        for capability in capabilities {
            transaction.execute(
                "INSERT OR IGNORE INTO grants(workspace_id, principal_id, capability) VALUES (?1, ?2, ?3)",
                params![workspace, principal, capability_name(capability)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn capabilities(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<BTreeSet<Capability>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT capability FROM grants WHERE workspace_id = ?1 AND principal_id = ?2 ORDER BY capability",
        )?;
        statement
            .query_map(params![workspace, principal], |row| row.get::<_, String>(0))?
            .map(|value| {
                let value = value?;
                serde_json::from_value(serde_json::Value::String(value)).map_err(StoreError::from)
            })
            .collect()
    }

    pub fn authorize(
        &self,
        workspace: &str,
        principal: &str,
        capability: Capability,
    ) -> Result<(), StoreError> {
        if self
            .capabilities(workspace, principal)?
            .contains(&capability)
        {
            Ok(())
        } else {
            Err(StoreError::AccessDenied {
                workspace: workspace.to_owned(),
                principal: principal.to_owned(),
                capability,
            })
        }
    }

    pub fn workspace(&self, workspace: &str, principal: &str) -> Result<Workspace, StoreError> {
        self.authorize(workspace, principal, Capability::CellRead)?;
        self.lock()?
            .query_row(
                "SELECT id, name FROM workspaces WHERE id = ?1",
                [workspace],
                |row| {
                    Ok(Workspace {
                        id: row.get(0)?,
                        name: row.get(1)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "workspace",
                id: workspace.to_owned(),
            })
    }

    pub fn create_candidate_build(
        &self,
        workspace: &str,
        principal: &str,
        candidate: &CandidateBuild,
        idempotency_key: &str,
    ) -> Result<CandidateBuild, StoreError> {
        self.authorize(workspace, principal, Capability::CandidateBuild)?;
        validate_candidate_build(workspace, principal, candidate)?;
        let mut connection = self.lock()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let request = candidate_request_identity(candidate);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT build_json FROM candidate_builds WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, candidate.id],
                |row| row.get(0),
            )
            .optional()?;
        let accepted = if let Some(existing) = existing {
            let existing = decode_candidate_build(&existing)?;
            if existing.principal_id != principal
                || candidate_request_identity(&existing) != request
            {
                return Err(StoreError::Conflict {
                    resource: "candidate build",
                    id: candidate.id.clone(),
                });
            }
            existing
        } else {
            transaction.execute(
                "INSERT INTO candidate_builds(workspace_id,id,principal_id,resource_name,request_digest,build_json,accepted_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![workspace,candidate.id,principal,candidate.resource_name,candidate.request_digest,serde_json::to_string(candidate)?,candidate.accepted_at_unix],
            )?;
            candidate.clone()
        };
        bind_candidate_request(
            &transaction,
            workspace,
            principal,
            idempotency_key,
            &request,
            &accepted,
        )?;
        transaction.commit()?;
        Ok(accepted)
    }

    /// Validate a request replay before performing any source resolution.
    pub fn candidate_request_replay(
        &self,
        workspace: &str,
        principal: &str,
        key: &str,
        input_digest: &str,
    ) -> Result<Option<CandidateBuild>, StoreError> {
        self.authorize(workspace, principal, Capability::CandidateRead)?;
        let row: Option<(String, String, String)> = self.lock()?.query_row(
            "SELECT operation,request_hash,response_json FROM idempotency WHERE workspace_id=?1 AND principal_id=?2 AND key=?3",
            params![workspace,principal,key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        let Some((operation, hash, response)) = row else {
            return Ok(None);
        };
        if operation != "candidate.build" {
            return Err(StoreError::IdempotencyConflict { key: key.into() });
        }
        let recorded = decode_candidate_build(&response)?;
        let legacy_source = proofstorm_core::CandidateInput::PullRequest {
            url: recorded.pull_request_url.clone(),
        };
        let legacy_source = proofstorm_core::candidate_build_profile(&recorded.implementation)
            .and_then(|profile| legacy_source.normalized(&profile.repository).ok())
            .unwrap_or(legacy_source);
        let legacy_input = proofstorm_core::digest_json(&(
            &recorded.id,
            &recorded.implementation,
            legacy_source,
            None::<String>,
            None::<String>,
        ));
        if hash != proofstorm_core::digest_json(&input_digest)
            && !(recorded.provenance.is_none() && legacy_input == input_digest)
        {
            return Err(StoreError::IdempotencyConflict { key: key.into() });
        }
        self.candidate_build(workspace, principal, &recorded.id)
            .map(Some)
    }

    pub fn record_candidate_request(
        &self,
        workspace: &str,
        principal: &str,
        key: &str,
        input_digest: &str,
        candidate: &CandidateBuild,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::CandidateBuild)?;
        let mut connection = self.lock()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        bind_candidate_request(
            &transaction,
            workspace,
            principal,
            key,
            &serde_json::json!(input_digest),
            candidate,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn candidate_build(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<CandidateBuild, StoreError> {
        self.authorize(workspace, principal, Capability::CandidateRead)?;
        self.candidate_build_unchecked(workspace, id)
    }

    pub fn candidate_builds(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<Vec<CandidateBuild>, StoreError> {
        self.authorize(workspace, principal, Capability::CandidateRead)?;
        self.candidate_builds_unchecked(workspace)
    }

    pub fn candidate_directory_generation(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<i64, StoreError> {
        self.authorize(workspace, principal, Capability::CandidateRead)?;
        Ok(self
            .lock()?
            .query_row(
                "SELECT generation FROM candidate_directory_versions WHERE workspace_id=?1",
                [workspace],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Stable bounded storage scan; active observation does not load terminal history.
    pub fn candidate_build_page(
        &self,
        workspace: &str,
        principal: &str,
        after: &str,
        active_only: bool,
        limit: u32,
    ) -> Result<Vec<CandidateBuild>, StoreError> {
        self.authorize(workspace, principal, Capability::CandidateRead)?;
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT build_json FROM candidate_builds WHERE workspace_id=?1 AND id>?2 AND (?3=0 OR json_extract(build_json,'$.phase') NOT IN ('succeeded','failed','cancelled')) ORDER BY id LIMIT ?4")?;
        let records = statement.query_map(
            params![workspace, after, active_only, limit.clamp(1, 50)],
            |row| row.get::<_, String>(0),
        )?;
        records.map(|row| decode_candidate_build(&row?)).collect()
    }

    pub fn update_candidate_build(
        &self,
        workspace: &str,
        candidate: &CandidateBuild,
    ) -> Result<CandidateBuild, StoreError> {
        let current = self.candidate_build_unchecked(workspace, &candidate.id)?;
        validate_candidate_update(&current, candidate)?;
        let updated = self.lock()?.execute(
            "UPDATE candidate_builds SET build_json = ?1
             WHERE workspace_id = ?2 AND id = ?3 AND build_json = ?4",
            params![
                serde_json::to_string(candidate)?,
                workspace,
                candidate.id,
                serde_json::to_string(&current)?
            ],
        )?;
        if updated == 0 {
            return self.candidate_build_unchecked(workspace, &candidate.id);
        }
        Ok(candidate.clone())
    }

    pub fn effective_catalog(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<CatalogResponse, StoreError> {
        self.authorize(workspace, principal, Capability::CatalogRead)?;
        if self
            .authorize(workspace, principal, Capability::CandidateRead)
            .is_err()
        {
            return Ok(default_catalog().clone());
        }
        self.effective_catalog_unchecked(workspace)
    }

    pub fn create_draft(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        cell: &CellSpec,
        idempotency_key: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::CellCreate)?;
        let request = serde_json::json!({"id": id, "cell": cell});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "cell.create",
            &request,
        )? {
            return Ok(response);
        }
        let draft = Draft {
            id: id.to_owned(),
            workspace_id: workspace.to_owned(),
            version: 1,
            cell: cell.clone(),
        };
        let inserted = self.lock()?.execute(
            "INSERT INTO drafts(workspace_id, id, version, cell_json) VALUES (?1, ?2, 1, ?3) ON CONFLICT(workspace_id, id) DO NOTHING",
            params![workspace, id, serde_json::to_string(cell)?],
        )?;
        if inserted == 0 {
            return Err(StoreError::Conflict {
                resource: "draft",
                id: id.to_owned(),
            });
        }
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "cell.create",
            &request,
            &draft,
        )?;
        Ok(draft)
    }

    pub fn read_draft(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::CellRead)?;
        self.read_draft_unchecked(workspace, id)
    }

    pub fn edit_draft(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        expected_version: u64,
        cell: &CellSpec,
        idempotency_key: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::CellEdit)?;
        let request =
            serde_json::json!({"id": id, "expectedVersion": expected_version, "cell": cell});
        if let Some(response) =
            self.idempotent_response(workspace, principal, idempotency_key, "cell.edit", &request)?
        {
            return Ok(response);
        }
        let changed = self.lock()?.execute(
            "UPDATE drafts SET version = version + 1, cell_json = ?1
             WHERE workspace_id = ?2 AND id = ?3 AND version = ?4",
            params![
                serde_json::to_string(cell)?,
                workspace,
                id,
                sql_version(expected_version)?
            ],
        )?;
        if changed == 0 {
            let current = self.read_draft_unchecked(workspace, id)?;
            return Err(StoreError::StaleDraft {
                draft: id.to_owned(),
                expected: expected_version,
                actual: current.version,
            });
        }
        let draft = self.read_draft_unchecked(workspace, id)?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "cell.edit",
            &request,
            &draft,
        )?;
        Ok(draft)
    }

    pub fn clone_draft(
        &self,
        workspace: &str,
        principal: &str,
        source: &str,
        target: &str,
        idempotency_key: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::CellClone)?;
        let request = serde_json::json!({"source": source, "target": target});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "cell.clone",
            &request,
        )? {
            return Ok(response);
        }
        let source = self.read_draft_unchecked(workspace, source)?;
        let draft = Draft {
            id: target.to_owned(),
            workspace_id: workspace.to_owned(),
            version: 1,
            cell: source.cell,
        };
        self.lock()?.execute(
            "INSERT INTO drafts(workspace_id, id, version, cell_json) VALUES (?1, ?2, 1, ?3)",
            params![workspace, target, serde_json::to_string(&draft.cell)?],
        )?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "cell.clone",
            &request,
            &draft,
        )?;
        Ok(draft)
    }

    pub fn diff_drafts(
        &self,
        workspace: &str,
        principal: &str,
        from: &str,
        to: &str,
    ) -> Result<DraftDiff, StoreError> {
        let from = self.read_draft(workspace, principal, from)?;
        let to = self.read_draft(workspace, principal, to)?;
        let from_ids = from
            .cell
            .components
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        let to_ids = to
            .cell
            .components
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        Ok(DraftDiff {
            from_version: from.version,
            to_version: to.version,
            added_components: to_ids.difference(&from_ids).cloned().collect(),
            removed_components: from_ids.difference(&to_ids).cloned().collect(),
            links_changed: from.cell.links != to.cell.links,
            policy_changed: from.cell.policy != to.cell.policy,
        })
    }

    pub fn publish(
        &self,
        workspace: &str,
        principal: &str,
        draft_id: &str,
        expected_version: u64,
        idempotency_key: &str,
    ) -> Result<PublishedRevision, StoreError> {
        self.authorize(workspace, principal, Capability::CellPublish)?;
        let request = serde_json::json!({"draftId": draft_id, "expectedVersion": expected_version});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "cell.publish",
            &request,
        )? {
            return Ok(response);
        }
        let draft = self.read_draft_unchecked(workspace, draft_id)?;
        if draft.version != expected_version {
            return Err(StoreError::StaleDraft {
                draft: draft_id.to_owned(),
                expected: expected_version,
                actual: draft.version,
            });
        }
        let report = validate_cell(&draft.cell);
        if !report.valid {
            return Err(StoreError::Validation(serde_json::to_string(
                &report.issues,
            )?));
        }
        let catalog = self.effective_catalog_unchecked(workspace)?;
        let effective_cell =
            resolve_effective_cell(&draft.cell, &catalog).map_err(StoreError::Catalog)?;
        let lock = resolve_lock(&effective_cell, &catalog).map_err(StoreError::Catalog)?;
        let digest = proofstorm_core::publication_digest(workspace, &effective_cell, &lock);
        let revision = PublishedRevision {
            workspace_id: workspace.to_owned(),
            digest: digest.clone(),
            cell: effective_cell,
            lock,
        };
        self.lock()?.execute(
            "INSERT INTO revisions(digest, workspace_id, draft_id, draft_version, revision_json)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(digest) DO UPDATE SET draft_id=excluded.draft_id,draft_version=excluded.draft_version WHERE NOT EXISTS(SELECT 1 FROM drafts WHERE workspace_id=revisions.workspace_id AND id=revisions.draft_id)",
            params![digest, workspace, draft_id, sql_version(draft.version)?, serde_json::to_string(&revision)?],
        )?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "cell.publish",
            &request,
            &revision,
        )?;
        Ok(revision)
    }

    pub fn revision(
        &self,
        workspace: &str,
        principal: &str,
        digest: &str,
    ) -> Result<PublishedRevision, StoreError> {
        self.authorize(workspace, principal, Capability::CellRead)?;
        let encoded = self
            .lock()?
            .query_row(
                "SELECT revision_json FROM revisions WHERE workspace_id = ?1 AND digest = ?2",
                params![workspace, digest],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "revision",
                id: digest.to_owned(),
            })
    }

    pub fn materialize(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        revision_digest: &str,
        idempotency_key: &str,
    ) -> Result<CellInstance, StoreError> {
        self.authorize(workspace, principal, Capability::CellMaterialize)?;
        if !is_slug(instance_id) {
            return Err(StoreError::Validation(
                "instance id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        if updates::state(&*self.lock()?, workspace, instance_id)?.closing {
            return Err(StoreError::CellUpdate { code: "cell_closing", message: "Closed cell cannot be recreated by replaying materialize; start a new cell incarnation".into() });
        }
        let request =
            serde_json::json!({"instanceId": instance_id, "revisionDigest": revision_digest});
        if let Some(_response) = self.idempotent_response::<CellInstance, _>(
            workspace,
            principal,
            idempotency_key,
            "cell.materialize",
            &request,
        )? {
            return self.instance_unchecked(workspace, instance_id);
        }
        if let Ok(existing) = self.instance_unchecked(workspace, instance_id) {
            if existing.revision_digest != revision_digest {
                return Err(StoreError::Conflict {
                    resource: "instance",
                    id: instance_id.into(),
                });
            }
            return Ok(existing);
        }
        let revision = self.revision_unchecked(workspace, revision_digest)?;
        // Deleted cells consume their plans. A stale low-level materialize request
        // must not resurrect one through a shared immutable revision.
        let has_plan: bool = self.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM revisions r JOIN drafts d ON d.workspace_id=r.workspace_id AND d.id=r.draft_id WHERE r.workspace_id=?1 AND r.digest=?2)", params![workspace, revision_digest], |r|r.get(0))?;
        if !has_plan {
            return Err(StoreError::NotFound {
                resource: "creation plan; create a fresh plan",
                id: revision_digest.into(),
            });
        }
        // Existing instances returned above use their immutable lock even after
        // retirement. New materializations must pass the current support policy,
        // including plans published before a release was retired.
        let catalog = self.effective_catalog_unchecked(workspace)?;
        let mut locked_cell = revision.cell.clone();
        for component in &mut locked_cell.components {
            let locked = revision
                .lock
                .entries
                .iter()
                .find(|entry| entry.component_id == component.id)
                .ok_or_else(|| StoreError::Catalog("component lock missing".into()))?;
            component.version = Some(locked.version.clone());
        }
        proofstorm_core::validate_new_cell_versions(&locked_cell, &catalog)
            .map_err(StoreError::Catalog)?;
        let nonce: String = self
            .lock()?
            .query_row("SELECT hex(randomblob(16))", [], |r| r.get(0))?;
        let identity = proofstorm_core::digest_json(&(workspace, instance_id, nonce));
        let instance_key = format!("i{}", &identity[7..26]);
        let instance = CellInstance {
            generation: 1,
            id: instance_id.to_owned(),
            workspace_id: workspace.to_owned(),
            revision_digest: revision_digest.to_owned(),
            lock_digest: revision.lock.digest,
            resource_name: format!("cell-{instance_key}"),
            instance_key,
        };
        let inserted = self.lock()?.execute(
            "INSERT OR IGNORE INTO instances(workspace_id, id, revision_digest, lock_digest, instance_key, resource_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance.workspace_id,
                instance.id,
                instance.revision_digest,
                instance.lock_digest,
                instance.instance_key,
                instance.resource_name
            ],
        )?;
        if inserted == 0 {
            let existing = self.instance_unchecked(workspace, instance_id)?;
            if existing != instance {
                return Err(StoreError::Conflict {
                    resource: "instance",
                    id: instance_id.to_owned(),
                });
            }
        }
        self.record_plan_use(&instance, None)?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "cell.materialize",
            &request,
            &instance,
        )?;
        Ok(instance)
    }

    pub fn instance(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<CellInstance, StoreError> {
        self.authorize(workspace, principal, Capability::CellStatus)?;
        self.instance_unchecked(workspace, id)
    }

    pub fn instance_for_close(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<CellInstance, StoreError> {
        self.authorize(workspace, principal, Capability::CellClose)?;
        self.instance_unchecked(workspace, id)
    }

    pub fn create_experiment(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        instance_id: &str,
        idempotency_key: &str,
    ) -> Result<Experiment, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentCreate)?;
        if !is_slug(experiment_id) {
            return Err(StoreError::Validation(
                "experiment id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        self.instance_unchecked(workspace, instance_id)?;
        let request = serde_json::json!({"experimentId": experiment_id, "instanceId": instance_id});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "experiment.create",
            &request,
        )? {
            return Ok(response);
        }
        let experiment = Experiment {
            id: experiment_id.to_owned(),
            workspace_id: workspace.to_owned(),
            instance_id: instance_id.to_owned(),
            owner_principal_id: principal.to_owned(),
            phase: ExperimentPhase::Active,
            created_at_unix: now_unix(),
            closed_at_unix: None,
        };
        let inserted = self.lock()?.execute(
            "INSERT OR IGNORE INTO experiments(workspace_id, id, instance_id, owner_principal_id, phase_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![workspace, experiment_id, instance_id, principal,
                serde_json::to_string(&experiment.phase)?, experiment.created_at_unix],
        )?;
        if inserted == 0 {
            let existing = self.experiment_unchecked(workspace, experiment_id)?;
            if existing.instance_id != instance_id || existing.owner_principal_id != principal {
                return Err(StoreError::Conflict {
                    resource: "experiment",
                    id: experiment_id.to_owned(),
                });
            }
            self.record_idempotency(
                workspace,
                principal,
                idempotency_key,
                "experiment.create",
                &request,
                &existing,
            )?;
            return Ok(existing);
        }
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "experiment.create",
            &request,
            &experiment,
        )?;
        Ok(experiment)
    }

    pub fn experiment(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
    ) -> Result<Experiment, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.experiment_unchecked(workspace, experiment_id)
    }

    pub fn experiment_for_session(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
    ) -> Result<Experiment, StoreError> {
        self.authorize(workspace, principal, Capability::CellOperate)?;
        self.experiment_unchecked(workspace, experiment_id)
    }

    pub fn close_experiment(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        idempotency_key: &str,
    ) -> Result<Experiment, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentClose)?;
        let request = serde_json::json!({"experimentId": experiment_id});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "experiment.close",
            &request,
        )? {
            return Ok(response);
        }
        self.experiment_unchecked(workspace, experiment_id)?;
        let now = now_unix();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM actions WHERE workspace_id=?1 AND experiment_id=?2 AND phase_json IN ('\"pending\"','\"running\"'))",params![workspace,experiment_id],|row|row.get(0))?;
        if active {
            return Err(StoreError::Validation(
                "run has active operations; wait before finishing".into(),
            ));
        }
        // Seal configuration in the same transaction as closure. Later cell edits cannot change this export.
        let instance_id: String = transaction.query_row(
            "SELECT instance_id FROM experiments WHERE workspace_id=?1 AND id=?2",
            params![workspace, experiment_id],
            |row| row.get(0),
        )?;
        let instance=transaction.query_row("SELECT revision_digest,lock_digest,instance_key,resource_name,COALESCE((SELECT generation FROM cell_update_state s WHERE s.workspace_id=instances.workspace_id AND s.instance_id=instances.id),1) FROM instances WHERE workspace_id=?1 AND id=?2",params![workspace,instance_id],|row|Ok(CellInstance {id:instance_id.clone(),workspace_id:workspace.into(),revision_digest:row.get(0)?,lock_digest:row.get(1)?,instance_key:row.get(2)?,resource_name:row.get(3)?,generation:updates::generation_column(row,4)?}))?;
        let revision: String = transaction.query_row(
            "SELECT revision_json FROM revisions WHERE workspace_id=?1 AND digest=?2",
            params![workspace, instance.revision_digest],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO run_seals VALUES(?1,?2,?3,?4)",
            params![
                workspace,
                experiment_id,
                serde_json::to_string(&instance)?,
                revision
            ],
        )?;
        transaction.execute(
            "UPDATE experiments SET phase_json = ?1, closed_at = COALESCE(closed_at, ?2)
             WHERE workspace_id = ?3 AND id = ?4",
            params![
                serde_json::to_string(&ExperimentPhase::Closed)?,
                now,
                workspace,
                experiment_id
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        let experiment = self.experiment_unchecked(workspace, experiment_id)?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "experiment.close",
            &request,
            &experiment,
        )?;
        Ok(experiment)
    }

    pub fn revision_for_materialize(
        &self,
        workspace: &str,
        principal: &str,
        digest: &str,
    ) -> Result<PublishedRevision, StoreError> {
        self.authorize(workspace, principal, Capability::CellMaterialize)?;
        self.revision_unchecked(workspace, digest)
    }

    pub fn revision_for_evidence(
        &self,
        workspace: &str,
        principal: &str,
        digest: &str,
    ) -> Result<PublishedRevision, StoreError> {
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        self.revision_unchecked(workspace, digest)
    }

    pub fn operation_context(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        capability: Capability,
    ) -> Result<(CellInstance, PublishedRevision), StoreError> {
        self.authorize(workspace, principal, capability)?;
        let instance = self.instance_unchecked(workspace, instance_id)?;
        let revision = self.revision_unchecked(workspace, &instance.revision_digest)?;
        Ok((instance, revision))
    }

    /// A replay validates and renders against the configuration admitted with the operation.
    pub fn operation_context_for(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        operation_id: &str,
        capability: Capability,
    ) -> Result<(CellInstance, PublishedRevision), StoreError> {
        let (mut instance, current) =
            self.operation_context(workspace, principal, instance_id, capability)?;
        match self.operation_unchecked(workspace, operation_id) {
            Ok(operation) => {
                if operation.instance_id != instance_id || operation.principal_id != principal {
                    return Err(StoreError::Conflict {
                        resource: "operation",
                        id: operation_id.into(),
                    });
                }
                let revision = self.revision_unchecked(workspace, &operation.revision_digest)?;
                instance.revision_digest.clone_from(&revision.digest);
                instance.lock_digest.clone_from(&revision.lock.digest);
                Ok((instance, revision))
            }
            Err(StoreError::NotFound { .. }) => Ok((instance, current)),
            Err(error) => Err(error),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "action identity and the session, sequence, concurrency, and insert checks remain one atomic admission transaction"
    )]
    pub fn create_operation(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        experiment_id: &str,
        session_id: &str,
        operation_id: &str,
        kind: OperationKind,
        request: &serde_json::Value,
        idempotency_key: &str,
        capability: Capability,
    ) -> Result<CellOperation, StoreError> {
        self.create_operation_inner(
            workspace,
            principal,
            instance_id,
            experiment_id,
            session_id,
            operation_id,
            kind,
            request,
            idempotency_key,
            capability,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_operation_at_revision(
        &self,
        expected_revision: &str,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        experiment_id: &str,
        session_id: &str,
        operation_id: &str,
        kind: OperationKind,
        request: &serde_json::Value,
        idempotency_key: &str,
        capability: Capability,
    ) -> Result<CellOperation, StoreError> {
        self.create_operation_inner(
            workspace,
            principal,
            instance_id,
            experiment_id,
            session_id,
            operation_id,
            kind,
            request,
            idempotency_key,
            capability,
            Some(expected_revision),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "action identity, session, quota, and revision checks are one atomic admission transaction"
    )]
    fn create_operation_inner(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        experiment_id: &str,
        session_id: &str,
        operation_id: &str,
        kind: OperationKind,
        request: &serde_json::Value,
        idempotency_key: &str,
        capability: Capability,
        expected_revision: Option<&str>,
    ) -> Result<CellOperation, StoreError> {
        self.authorize(workspace, principal, capability)?;
        if !is_slug(operation_id) {
            return Err(StoreError::Validation(
                "operation id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        self.instance_unchecked(workspace, instance_id)?;
        let implicit = experiment_id.is_empty();
        let resolved_run = if implicit {
            // Replays retain their original sealed run; new work rolls into the next run.
            match self.operation_unchecked(workspace, operation_id) {
                Ok(previous)
                    if previous.principal_id == principal
                        && previous.instance_id == instance_id =>
                {
                    previous.experiment_id
                }
                Ok(_) | Err(StoreError::NotFound { .. }) => {
                    self.implicit_run_id(workspace, principal, instance_id)?
                }
                Err(error) => return Err(error),
            }
        } else {
            experiment_id.to_owned()
        };
        let experiment_id = resolved_run.as_str();
        let mut normalized = request.clone();
        if let Some(fields) = normalized.as_object_mut() {
            for field in ["experiment_id", "run_id"] {
                if fields.contains_key(field) {
                    fields.insert(
                        field.into(),
                        serde_json::Value::String(resolved_run.clone()),
                    );
                }
            }
        }
        let request = &normalized;
        let envelope = serde_json::json!({
            "instanceId": instance_id, "experimentId": experiment_id,
            "sessionId": session_id, "operationId": operation_id,
            "kind": kind, "request": request
        });
        if let Some(response) = self.idempotent_response::<CellOperation, _>(
            workspace,
            principal,
            idempotency_key,
            "cell.operation.create",
            &envelope,
        )? {
            return self.operation_unchecked(workspace, &response.id);
        }
        self.authorize_operation_access(workspace, principal, instance_id, kind, request)?;
        if implicit {
            self.ensure_implicit_run(workspace, principal, instance_id, experiment_id)?;
        }
        let run = self.experiment_unchecked(workspace, experiment_id)?;
        if run.instance_id != instance_id || run.phase != ExperimentPhase::Active {
            return Err(StoreError::Validation(
                "action run must be open and belong to this cell".into(),
            ));
        }
        let automatic_session = session_id.is_empty();
        let session = self.track_session(workspace, principal, experiment_id, session_id)?;
        let session_id = session.id.as_str();
        let digest = proofstorm_core::digest_json(&(
            workspace,
            instance_id,
            session_id,
            operation_id,
            &kind,
            request,
        ));
        let accepted_at = now_unix();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Admission and its retry record are one transaction. Another connection may
        // have admitted this request after our optimistic lookup and session tracking.
        if let Some(previous) = Self::idempotent_response_from::<CellOperation, _>(
            &transaction,
            workspace,
            principal,
            idempotency_key,
            "cell.operation.create",
            &envelope,
        )? {
            return Self::operation_from(&transaction, workspace, &previous.id);
        }
        match Self::operation_from(&transaction, workspace, operation_id) {
            Ok(existing) => {
                if existing.instance_id != instance_id
                    || existing.principal_id != principal
                    || existing.experiment_id != experiment_id
                    || existing.request_digest != proofstorm_core::digest_json(request)
                    || existing.kind != kind
                    || (!automatic_session && existing.session_id != session_id)
                {
                    return Err(StoreError::Conflict {
                        resource: "operation",
                        id: operation_id.into(),
                    });
                }
                Self::record_idempotency_in(
                    &transaction,
                    workspace,
                    principal,
                    idempotency_key,
                    "cell.operation.create",
                    &envelope,
                    &existing,
                )?;
                transaction.commit()?;
                return Ok(existing);
            }
            Err(StoreError::NotFound { .. }) => {}
            Err(error) => return Err(error),
        }
        let run_open:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM experiments WHERE workspace_id=?1 AND id=?2 AND instance_id=?3 AND phase_json='\"active\"')",params![workspace,experiment_id,instance_id],|row|row.get(0))?;
        if !run_open {
            return Err(StoreError::Validation(
                "run finished during operation admission; retry the same request".into(),
            ));
        }
        let handle_phase: Option<String> = transaction
            .query_row(
                "SELECT phase FROM cell_handles WHERE workspace_id=?1 AND instance_id=?2",
                params![workspace, instance_id],
                |row| row.get(0),
            )
            .optional()?;
        if handle_phase.is_some_and(|phase| phase != "\"open\"") {
            return Err(StoreError::Validation(
                "cell is closing; new actions are not admitted".into(),
            ));
        }
        transaction.execute("UPDATE sessions SET last_activity_at=MAX(last_activity_at,?1) WHERE workspace_id=?2 AND id=?3",params![accepted_at,workspace,session_id])?;
        let last_sequence = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM actions
             WHERE workspace_id = ?1 AND instance_id = ?2",
            params![workspace, instance_id],
            |row| row.get::<_, i64>(0),
        )?;
        let sequence = u64::try_from(last_sequence + 1)
            .map_err(|_| StoreError::InvalidStoredVersion(last_sequence))?;
        let revision_digest = updates::admit_operation(
            &transaction,
            workspace,
            instance_id,
            operation_id,
            request,
            kind,
        )?;
        if expected_revision.is_some_and(|expected| expected != revision_digest) {
            return Err(StoreError::CellUpdate {code:"cell_update_conflict", message:"Configuration changed during operation admission; retry against current configuration".into()});
        }
        let operation = CellOperation {
            revision_digest,
            id: operation_id.to_owned(),
            workspace_id: workspace.to_owned(),
            instance_id: instance_id.to_owned(),
            experiment_id: experiment_id.to_owned(),
            session_id: session_id.to_owned(),
            principal_id: principal.to_owned(),
            sequence,
            kind,
            capability,
            resource_name: format!("op-{}", &digest[7..26]),
            request_digest: proofstorm_core::digest_json(request),
            request: request.clone(),
            phase: OperationPhase::Pending,
            accepted_at_unix: accepted_at,
            started_at_unix: None,
            completed_at_unix: None,
            artifact: None,
        };
        transaction.execute(
            "INSERT INTO actions(workspace_id, id, instance_id, experiment_id, session_id,
             principal_id, sequence, kind_json, capability_json, resource_name, request_digest,
             request_json, phase_json, accepted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                workspace,
                operation_id,
                instance_id,
                experiment_id,
                session_id,
                principal,
                sql_version(sequence)?,
                serde_json::to_string(&kind)?,
                serde_json::to_string(&capability)?,
                operation.resource_name,
                operation.request_digest,
                serde_json::to_string(request)?,
                serde_json::to_string(&operation.phase)?,
                accepted_at
            ],
        )?;
        Self::record_idempotency_in(
            &transaction,
            workspace,
            principal,
            idempotency_key,
            "cell.operation.create",
            &envelope,
            &operation,
        )?;
        transaction.commit()?;
        Ok(operation)
    }

    pub fn operation(
        &self,
        workspace: &str,
        principal: &str,
        operation_id: &str,
    ) -> Result<CellOperation, StoreError> {
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        self.operation_unchecked(workspace, operation_id)
    }

    pub fn operation_for_cancel(
        &self,
        workspace: &str,
        principal: &str,
        operation_id: &str,
    ) -> Result<CellOperation, StoreError> {
        self.authorize(workspace, principal, Capability::ActionCancel)?;
        let operation = self.operation_unchecked(workspace, operation_id)?;
        if operation.principal_id != principal {
            return Err(StoreError::OperationOwnerMismatch {
                operation: operation_id.to_owned(),
                owner: operation.principal_id,
                principal: principal.to_owned(),
            });
        }
        Ok(operation)
    }

    /// Recheck an actor's own admitted request immediately before runtime submission.
    /// This does not grant artifact-read access to other actors' operations.
    pub fn operation_for_submission(
        &self,
        workspace: &str,
        principal: &str,
        operation_id: &str,
    ) -> Result<CellOperation, StoreError> {
        let operation = self.operation_unchecked(workspace, operation_id)?;
        self.authorize(workspace, principal, operation.capability)?;
        if operation.principal_id != principal {
            return Err(StoreError::OperationOwnerMismatch {
                operation: operation_id.to_owned(),
                owner: operation.principal_id,
                principal: principal.to_owned(),
            });
        }
        if operation.phase == OperationPhase::Pending {
            self.authorize_operation_access(
                workspace,
                principal,
                &operation.instance_id,
                operation.kind,
                &operation.request,
            )?;
        }
        Ok(operation)
    }

    pub fn actions(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<CellOperation>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.experiment_unchecked(workspace, experiment_id)?;
        if !(1..=100).contains(&limit) {
            return Err(StoreError::Validation(
                "action list limit must be between 1 and 100".into(),
            ));
        }
        let ids = {
            let connection = self.lock()?;
            let mut statement = connection.prepare(
                "SELECT id FROM actions
                 WHERE workspace_id = ?1 AND experiment_id = ?2 AND sequence > ?3
                 ORDER BY sequence ASC LIMIT ?4",
            )?;
            statement
                .query_map(
                    params![
                        workspace,
                        experiment_id,
                        sql_version(after_sequence)?,
                        limit
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        ids.into_iter()
            .map(|id| self.operation_unchecked(workspace, &id))
            .collect()
    }

    /// Every pending or running operation recorded for one cell instance, in
    /// journal order. The store is the ledger of record, so cell close uses this
    /// to finalize operations whose runtime resources are about to disappear.
    pub fn active_operations(
        &self,
        workspace: &str,
        instance_id: &str,
    ) -> Result<Vec<CellOperation>, StoreError> {
        let ids = {
            let connection = self.lock()?;
            let mut statement = connection.prepare(
                "SELECT id FROM actions
                 WHERE workspace_id = ?1 AND instance_id = ?2
                   AND phase_json IN ('\"pending\"', '\"running\"')
                 ORDER BY experiment_id ASC, sequence ASC",
            )?;
            statement
                .query_map(params![workspace, instance_id], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        ids.into_iter()
            .map(|id| self.operation_unchecked(workspace, &id))
            .collect()
    }

    pub fn record_operation_result(
        &self,
        workspace: &str,
        operation_id: &str,
        phase: OperationPhase,
        content: serde_json::Value,
    ) -> Result<CellOperation, StoreError> {
        if matches!(phase, OperationPhase::Pending | OperationPhase::Running) {
            return Err(StoreError::Validation(
                "operation result phase must be terminal".into(),
            ));
        }
        let existing = self.operation_unchecked(workspace, operation_id)?;
        if matches!(
            existing.phase,
            OperationPhase::Succeeded | OperationPhase::Failed | OperationPhase::Cancelled
        ) {
            return Ok(existing);
        }
        let encoded = serde_json::to_vec(&content)?;
        if encoded.len() > MAX_ARTIFACT_BYTES {
            return Err(StoreError::ArtifactTooLarge {
                actual: encoded.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let artifact = OperationArtifact {
            media_type: "application/json".into(),
            digest: proofstorm_core::digest_json(&content),
            byte_length: u32::try_from(encoded.len()).map_err(|_| {
                StoreError::ArtifactTooLarge {
                    actual: encoded.len(),
                    maximum: MAX_ARTIFACT_BYTES,
                }
            })?,
            content,
        };
        let completed_at = now_unix();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE actions SET phase_json = ?1, artifact_json = ?2, completed_at = ?3
             WHERE workspace_id = ?4 AND id = ?5
               AND phase_json IN ('\"pending\"', '\"running\"')",
            params![
                serde_json::to_string(&phase)?,
                serde_json::to_string(&artifact)?,
                completed_at,
                workspace,
                operation_id
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            drop(connection);
            return self.operation_unchecked(workspace, operation_id);
        }
        transaction.execute("UPDATE sessions SET last_activity_at=MAX(last_activity_at,?1) WHERE workspace_id=?2 AND id=?3",params![completed_at,workspace,existing.session_id])?;
        transaction.commit()?;
        drop(connection);
        self.operation_unchecked(workspace, operation_id)
    }

    pub fn update_operation_phase(
        &self,
        workspace: &str,
        operation_id: &str,
        phase: OperationPhase,
    ) -> Result<CellOperation, StoreError> {
        if phase != OperationPhase::Running {
            return Err(StoreError::Validation(
                "operation phase update only accepts running".into(),
            ));
        }
        self.lock()?.execute(
            "UPDATE actions SET phase_json = ?1, started_at = COALESCE(started_at, ?2)
             WHERE workspace_id = ?3 AND id = ?4 AND phase_json = '\"pending\"'",
            params![
                serde_json::to_string(&phase)?,
                now_unix(),
                workspace,
                operation_id
            ],
        )?;
        self.operation_unchecked(workspace, operation_id)
    }

    fn candidate_build_unchecked(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<CandidateBuild, StoreError> {
        self.lock()?
            .query_row(
                "SELECT build_json FROM candidate_builds
                 WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|encoded| decode_candidate_build(&encoded))
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "candidate build",
                id: id.to_owned(),
            })
    }

    fn candidate_builds_unchecked(
        &self,
        workspace: &str,
    ) -> Result<Vec<CandidateBuild>, StoreError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT build_json FROM candidate_builds
             WHERE workspace_id = ?1 ORDER BY accepted_at DESC, id ASC",
        )?;
        statement
            .query_map([workspace], |row| row.get::<_, String>(0))?
            .map(|encoded| decode_candidate_build(&encoded.map_err(StoreError::from)?))
            .collect()
    }

    fn effective_catalog_unchecked(&self, workspace: &str) -> Result<CatalogResponse, StoreError> {
        // Image discovery needs successful source contracts, never build diagnostics
        // or the potentially much larger history of unsuccessful attempts.
        let candidates = {
            let connection = self.lock()?;
            let mut statement = connection.prepare("SELECT json_remove(build_json, '$.diagnostics') FROM candidate_builds WHERE workspace_id=?1 AND json_extract(build_json, '$.phase')='succeeded' ORDER BY id")?;
            statement
                .query_map([workspace], |row| row.get::<_, String>(0))?
                .map(|encoded| decode_candidate_build(&encoded?))
                .collect::<Result<Vec<_>, StoreError>>()?
        };
        effective_catalog(default_catalog(), &candidates).map_err(StoreError::Catalog)
    }

    fn read_draft_unchecked(&self, workspace: &str, id: &str) -> Result<Draft, StoreError> {
        let record = self
            .lock()?
            .query_row(
                "SELECT version, cell_json FROM drafts WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let (version, cell_json) = record.ok_or_else(|| StoreError::NotFound {
            resource: "draft",
            id: id.to_owned(),
        })?;
        let version =
            u64::try_from(version).map_err(|_| StoreError::InvalidStoredVersion(version))?;
        Ok(Draft {
            id: id.to_owned(),
            workspace_id: workspace.to_owned(),
            version,
            cell: serde_json::from_str(&cell_json)?,
        })
    }

    fn revision_unchecked(
        &self,
        workspace: &str,
        digest: &str,
    ) -> Result<PublishedRevision, StoreError> {
        let encoded = self
            .lock()?
            .query_row(
                "SELECT revision_json FROM revisions WHERE workspace_id = ?1 AND digest = ?2",
                params![workspace, digest],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "revision",
                id: digest.to_owned(),
            })
    }

    fn instance_unchecked(&self, workspace: &str, id: &str) -> Result<CellInstance, StoreError> {
        self.lock()?
            .query_row(
                "SELECT revision_digest, lock_digest, instance_key, resource_name, COALESCE((SELECT generation FROM cell_update_state s WHERE s.workspace_id=instances.workspace_id AND s.instance_id=instances.id),1)
                 FROM instances WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| {
                    Ok(CellInstance {
                        generation: updates::generation_column(row, 4)?,
                        id: id.to_owned(),
                        workspace_id: workspace.to_owned(),
                        revision_digest: row.get(0)?,
                        lock_digest: row.get(1)?,
                        instance_key: row.get(2)?,
                        resource_name: row.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "instance",
                id: id.to_owned(),
            })
    }

    fn operation_unchecked(&self, workspace: &str, id: &str) -> Result<CellOperation, StoreError> {
        Self::operation_from(&*self.lock()?, workspace, id)
    }

    fn operation_from(
        connection: &Connection,
        workspace: &str,
        id: &str,
    ) -> Result<CellOperation, StoreError> {
        connection
            .query_row(
                "SELECT instance_id, experiment_id, session_id, principal_id, sequence, kind_json,
                        capability_json, resource_name, request_digest, request_json, phase_json,
                        accepted_at, started_at, completed_at, artifact_json,
                        (SELECT revision_digest FROM operation_revisions r WHERE r.workspace_id=actions.workspace_id AND r.operation_id=actions.id)
                 FROM actions WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, Option<i64>>(12)?,
                        row.get::<_, Option<i64>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, String>(15)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    instance_id,
                    experiment_id,
                    session_id,
                    principal_id,
                    sequence,
                    kind,
                    capability,
                    resource_name,
                    request_digest,
                    request,
                    phase,
                    accepted_at_unix,
                    started_at_unix,
                    completed_at_unix,
                    artifact,
                    revision_digest,
                )| {
                    let sequence = u64::try_from(sequence)
                        .map_err(|_| StoreError::InvalidStoredVersion(sequence))?;
                    Ok::<CellOperation, StoreError>(CellOperation {
                        revision_digest,
                        id: id.to_owned(),
                        workspace_id: workspace.to_owned(),
                        instance_id,
                        experiment_id,
                        session_id,
                        principal_id,
                        sequence,
                        kind: serde_json::from_str(&kind)?,
                        capability: serde_json::from_str(&capability)?,
                        resource_name,
                        request_digest,
                        request: serde_json::from_str(&request)?,
                        phase: serde_json::from_str(&phase)?,
                        accepted_at_unix,
                        started_at_unix,
                        completed_at_unix,
                        artifact: artifact
                            .map(|value| serde_json::from_str(&value))
                            .transpose()?,
                    })
                },
            )
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "operation",
                id: id.to_owned(),
            })
    }

    fn experiment_unchecked(&self, workspace: &str, id: &str) -> Result<Experiment, StoreError> {
        self.lock()?
            .query_row(
                "SELECT instance_id, owner_principal_id, phase_json, created_at, closed_at
                 FROM experiments WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(instance_id, owner_principal_id, phase, created_at_unix, closed_at_unix)| {
                    Ok::<Experiment, StoreError>(Experiment {
                        id: id.to_owned(),
                        workspace_id: workspace.to_owned(),
                        instance_id,
                        owner_principal_id,
                        phase: serde_json::from_str(&phase)?,
                        created_at_unix,
                        closed_at_unix,
                    })
                },
            )
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "experiment",
                id: id.to_owned(),
            })
    }

    fn idempotent_response<T: DeserializeOwned, R: Serialize>(
        &self,
        workspace: &str,
        principal: &str,
        key: &str,
        operation: &str,
        request: &R,
    ) -> Result<Option<T>, StoreError> {
        Self::idempotent_response_from(
            &*self.lock()?,
            workspace,
            principal,
            key,
            operation,
            request,
        )
    }

    fn idempotent_response_from<T: DeserializeOwned, R: Serialize>(
        connection: &Connection,
        workspace: &str,
        principal: &str,
        key: &str,
        operation: &str,
        request: &R,
    ) -> Result<Option<T>, StoreError> {
        let found = connection
            .query_row(
                "SELECT operation, request_hash, response_json FROM idempotency
             WHERE workspace_id = ?1 AND principal_id = ?2 AND key = ?3",
                params![workspace, principal, key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored_operation, stored_hash, response)) = found else {
            return Ok(None);
        };
        let request_hash = proofstorm_core::digest_json(request);
        if stored_operation != operation || stored_hash != request_hash {
            return Err(StoreError::IdempotencyConflict {
                key: key.to_owned(),
            });
        }
        Ok(Some(serde_json::from_str(&response)?))
    }

    fn record_idempotency<T: Serialize, R: Serialize>(
        &self,
        workspace: &str,
        principal: &str,
        key: &str,
        operation: &str,
        request: &R,
        response: &T,
    ) -> Result<(), StoreError> {
        Self::record_idempotency_in(
            &*self.lock()?,
            workspace,
            principal,
            key,
            operation,
            request,
            response,
        )
    }

    fn record_idempotency_in<T: Serialize, R: Serialize>(
        connection: &Connection,
        workspace: &str,
        principal: &str,
        key: &str,
        operation: &str,
        request: &R,
        response: &T,
    ) -> Result<(), StoreError> {
        connection.execute(
            "INSERT INTO idempotency(workspace_id, principal_id, key, operation, request_hash, response_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![workspace, principal, key, operation, proofstorm_core::digest_json(request), serde_json::to_string(response)?],
        )?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }
}

fn capability_name(capability: Capability) -> Result<String, StoreError> {
    let serde_json::Value::String(name) = serde_json::to_value(capability)? else {
        unreachable!("Capability serializes as a string")
    };
    Ok(name)
}

fn sql_version(version: u64) -> Result<i64, StoreError> {
    i64::try_from(version).map_err(|_| StoreError::VersionOverflow(version))
}

fn now_unix() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn validate_session_request(session_id: &str) -> Result<(), StoreError> {
    if !is_slug(session_id) {
        return Err(StoreError::Validation(
            "session id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
        ));
    }
    Ok(())
}

fn decode_candidate_build(encoded: &str) -> Result<CandidateBuild, StoreError> {
    let candidate: CandidateBuild = serde_json::from_str(encoded)?;
    if candidate.api_version != proofstorm_core::CANDIDATE_BUILD_API_VERSION
        || candidate
            .provenance
            .as_ref()
            .is_some_and(|p| p.validate().is_err())
    {
        return Err(StoreError::Validation("candidate_record_unsupported: stored candidate evidence is inconsistent or uses an unsupported version; the record was not changed".into()));
    }
    Ok(candidate)
}

fn validate_candidate_build(
    workspace: &str,
    principal: &str,
    candidate: &CandidateBuild,
) -> Result<(), StoreError> {
    let resolution_failed = candidate.provenance.is_some()
        && candidate.phase == CandidateBuildPhase::Failed
        && candidate.commit_sha.is_none()
        && candidate.repository.is_none()
        && candidate.error_code.as_deref() == Some("candidate_source_resolution_failed")
        && candidate.error_message.is_some()
        && candidate.completed_at_unix.is_some();
    if candidate.api_version != proofstorm_core::CANDIDATE_BUILD_API_VERSION
        || candidate.workspace_id != workspace
        || candidate.principal_id != principal
        || !is_slug(&candidate.id)
        || !is_slug(&candidate.implementation)
        || candidate.base_version.is_empty()
        || (!resolution_failed && candidate.phase != CandidateBuildPhase::Pending)
        || (!resolution_failed && candidate.repository.as_deref().is_none_or(str::is_empty))
        || candidate.version.as_deref().is_none_or(str::is_empty)
        || candidate.image.is_some()
        || (!resolution_failed
            && (candidate.error_code.is_some() || candidate.error_message.is_some()))
    {
        return Err(StoreError::Validation(
            "candidate build has an invalid immutable identity or initial state".into(),
        ));
    }
    if candidate.provenance.is_none()
        && !candidate
            .pull_request_url
            .starts_with("https://github.com/")
    {
        return Err(StoreError::Validation(
            "candidate pull request must be a public https://github.com URL".into(),
        ));
    }
    let commit_sha = candidate.commit_sha.as_deref().unwrap_or_default();
    if !resolution_failed
        && (commit_sha.len() != 40 || !commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(StoreError::Validation(
            "candidate commit_sha must be a full 40-character Git SHA".into(),
        ));
    }
    let base = default_catalog()
        .entries
        .iter()
        .find(|entry| {
            entry.id == candidate.implementation && entry.version == candidate.base_version
        })
        .ok_or_else(|| {
            StoreError::Catalog(format!(
                "candidate base {} {} is not installed",
                candidate.implementation, candidate.base_version
            ))
        })?;
    if base.source.is_some() {
        return Err(StoreError::Validation(
            "candidate builds must derive from a built-in release".into(),
        ));
    }
    if let Some(provenance) = &candidate.provenance {
        if provenance.validate().is_err()
            || provenance.baseline_digest != proofstorm_core::digest_json(base)
            || !provenance.profile.platforms.contains(&provenance.platform)
            || candidate.build_features != provenance.profile.features
            || provenance
                .requested_source
                .normalized(&provenance.profile.repository)
                .as_ref()
                != Ok(&provenance.requested_source)
        {
            return Err(StoreError::Validation(
                "candidate provenance is inconsistent or unsupported".into(),
            ));
        }
    }
    Ok(())
}

fn validate_candidate_update(
    current: &CandidateBuild,
    candidate: &CandidateBuild,
) -> Result<(), StoreError> {
    let immutable_matches = current.api_version == candidate.api_version
        && current.id == candidate.id
        && current.workspace_id == candidate.workspace_id
        && current.principal_id == candidate.principal_id
        && current.implementation == candidate.implementation
        && current.base_version == candidate.base_version
        && current.pull_request_url == candidate.pull_request_url
        && current.resource_name == candidate.resource_name
        && current.request_digest == candidate.request_digest
        && current.provenance == candidate.provenance
        && current.build_features == candidate.build_features
        && current.accepted_at_unix == candidate.accepted_at_unix
        && current.repository == candidate.repository
        && current.commit_sha == candidate.commit_sha
        && current.version == candidate.version;
    if !immutable_matches {
        return Err(StoreError::Conflict {
            resource: "candidate build",
            id: candidate.id.clone(),
        });
    }
    if current.phase.terminal() && current != candidate {
        return Err(StoreError::Validation(
            "terminal candidate build state is immutable".into(),
        ));
    }
    let valid_transition = current.phase == candidate.phase
        || matches!(
            (current.phase, candidate.phase),
            (
                CandidateBuildPhase::Pending,
                CandidateBuildPhase::Resolving
                    | CandidateBuildPhase::Building
                    | CandidateBuildPhase::Succeeded
                    | CandidateBuildPhase::Failed
                    | CandidateBuildPhase::Cancelled
            ) | (
                CandidateBuildPhase::Resolving,
                CandidateBuildPhase::Building
                    | CandidateBuildPhase::Failed
                    | CandidateBuildPhase::Cancelled
            ) | (
                CandidateBuildPhase::Building,
                CandidateBuildPhase::Pushing
                    | CandidateBuildPhase::Succeeded
                    | CandidateBuildPhase::Failed
                    | CandidateBuildPhase::Cancelled
            ) | (
                CandidateBuildPhase::Pushing,
                CandidateBuildPhase::Succeeded
                    | CandidateBuildPhase::Failed
                    | CandidateBuildPhase::Cancelled
            )
        );
    if !valid_transition {
        return Err(StoreError::Validation(format!(
            "invalid candidate build transition from {:?} to {:?}",
            current.phase, candidate.phase
        )));
    }
    if candidate.phase == CandidateBuildPhase::Succeeded
        && candidate.image.as_deref().is_none_or(|image| {
            let Some((_, digest)) = image.rsplit_once("@sha256:") else {
                return true;
            };
            digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(StoreError::Validation(
            "successful candidate build must have an immutable sha256 image".into(),
        ));
    }
    Ok(())
}

fn is_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !value.contains("--")
}

fn candidate_request_identity(candidate: &CandidateBuild) -> serde_json::Value {
    candidate.provenance.as_ref().map_or_else(|| serde_json::json!({"candidateId":candidate.id,"implementation":candidate.implementation,"baseVersion":candidate.base_version,"pullRequestUrl":candidate.pull_request_url}), |p| serde_json::json!(p.input_digest))
}

fn bind_candidate_request(
    connection: &Connection,
    workspace: &str,
    principal: &str,
    key: &str,
    request: &serde_json::Value,
    candidate: &CandidateBuild,
) -> Result<(), StoreError> {
    let expected = proofstorm_core::digest_json(request);
    let row: Option<(String,String)> = connection.query_row("SELECT operation,request_hash FROM idempotency WHERE workspace_id=?1 AND principal_id=?2 AND key=?3",params![workspace,principal,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((operation, hash)) = row {
        if operation != "candidate.build" || hash != expected {
            return Err(StoreError::IdempotencyConflict { key: key.into() });
        }
    } else {
        connection.execute("INSERT INTO idempotency(workspace_id,principal_id,key,operation,request_hash,response_json) VALUES (?1,?2,?3,'candidate.build',?4,?5)", params![workspace,principal,key,expected,serde_json::to_string(candidate)?])?;
    }
    Ok(())
}
