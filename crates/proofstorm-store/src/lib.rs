#![allow(
    clippy::missing_errors_doc,
    reason = "all public store operations return the documented StoreError contract"
)]

mod environment;
pub use environment::EnvironmentEntry;
mod delegation;
#[cfg(test)]
mod session_tests;
mod sessions;
pub use sessions::SessionPage;
mod labs;
mod migration;
pub use labs::{LabHandle, LabHandlePhase};

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use proofstorm_core::{
    CandidateBuild, CandidateBuildPhase, Capability, CatalogResponse, DraftMutation, Experiment,
    ExperimentPhase, LabInstance, LabOperation, LabSpec, OperationArtifact, OperationKind,
    OperationPhase, PublishedRevision, WalletQuoteDirection, WalletQuoteObservation,
    WalletQuoteObservationInput, WalletQuoteObservationRole, apply_draft_mutation, default_catalog,
    effective_catalog, resolve_effective_lab, resolve_lock, validate_lab,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

/// Per-instance admission bound for non-terminal runtime actions. Eight allows
/// one useful agent batch (for example policies, payer funding, and invoices
/// for a bidirectional treatment) without permitting unbounded fan-out.
pub const MAX_ACTIVE_OPERATIONS: u32 = 8;
const MAX_ARTIFACT_BYTES: usize = 32 * 1024;

struct PaymentClaimInput<'a> {
    recipient_wallet: &'a str,
    recipient_mint: &'a str,
    mint_quote: &'a str,
    payer_wallet: &'a str,
    payer_mint: &'a str,
}

#[derive(Debug, Error)]
pub enum StoreError {
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
    #[error("lab validation failed: {0}")]
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
    #[error(
        "lab instance {instance:?} already has {active} active operations; maximum is {maximum}"
    )]
    OperationLimit {
        instance: String,
        active: u32,
        maximum: u32,
    },
    #[error("operation {operation:?} belongs to principal {owner:?}, not {principal:?}")]
    OperationOwnerMismatch {
        operation: String,
        owner: String,
        principal: String,
    },
    #[error("wallet quote {quote:?} belongs to principal {owner:?}, not {principal:?}")]
    QuoteOwnerMismatch {
        quote: String,
        owner: String,
        principal: String,
    },
    #[error("wallet mint quote {quote:?} already has payment operation {operation:?}")]
    QuotePaymentAlreadyClaimed { quote: String, operation: String },
}

impl StoreError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
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
            Self::OperationLimit { .. } => "operation_limit",
            Self::OperationOwnerMismatch { .. } => "operation_owner_mismatch",
            Self::QuoteOwnerMismatch { .. } => "quote_owner_mismatch",
            Self::QuotePaymentAlreadyClaimed { .. } => "quote_payment_already_claimed",
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
    pub lab: LabSpec,
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

struct WalletQuoteObservationRow {
    observation_sequence: i64,
    workspace_id: String,
    instance_id: String,
    experiment_id: String,
    session_id: String,
    principal_id: String,
    operation_id: String,
    observation_role_json: String,
    wallet_id: String,
    mint_id: String,
    direction_json: String,
    quote_id: String,
    amount_sat: i64,
    state: String,
    wallet_created_at_unix: Option<i64>,
    wallet_paid_at_unix: Option<i64>,
    wallet_expires_at_unix: Option<i64>,
    fee_reserve_sat: Option<i64>,
    fee_paid_sat: Option<i64>,
    observed_at_unix: i64,
}

impl TryFrom<WalletQuoteObservationRow> for WalletQuoteObservation {
    type Error = StoreError;

    fn try_from(row: WalletQuoteObservationRow) -> Result<Self, Self::Error> {
        Ok(Self {
            observation_sequence: u64::try_from(row.observation_sequence)
                .map_err(|_| StoreError::InvalidStoredVersion(row.observation_sequence))?,
            workspace_id: row.workspace_id,
            instance_id: row.instance_id,
            experiment_id: row.experiment_id,
            session_id: row.session_id,
            principal_id: row.principal_id,
            observed_by_operation: row.operation_id,
            role: serde_json::from_str(&row.observation_role_json)?,
            wallet_id: row.wallet_id,
            mint_id: row.mint_id,
            direction: serde_json::from_str(&row.direction_json)?,
            quote_id: row.quote_id,
            amount_sat: u64::try_from(row.amount_sat)
                .map_err(|_| StoreError::InvalidStoredVersion(row.amount_sat))?,
            state: row.state,
            wallet_created_at_unix: row.wallet_created_at_unix,
            wallet_paid_at_unix: row.wallet_paid_at_unix,
            wallet_expires_at_unix: row.wallet_expires_at_unix,
            fee_reserve_sat: optional_sql_u64(row.fee_reserve_sat)?,
            fee_paid_sat: optional_sql_u64(row.fee_paid_sat)?,
            observed_at_unix: row.observed_at_unix,
        })
    }
}

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    context_id: Arc<String>,
    context_sessions: Arc<Mutex<BTreeSet<(String, String)>>>,
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
    fn from_connection(mut connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        migration::prepare(&mut connection)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS workspaces (
               id TEXT PRIMARY KEY, name TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS lab_handles (
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
               lab_json TEXT NOT NULL,
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
             CREATE TABLE IF NOT EXISTS operations (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               kind_json TEXT NOT NULL,
               resource_name TEXT NOT NULL UNIQUE,
               request_digest TEXT NOT NULL,
               phase_json TEXT NOT NULL,
               artifact_json TEXT,
               created_at INTEGER NOT NULL DEFAULT (unixepoch()),
               PRIMARY KEY (workspace_id, id),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id)
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
             CREATE TABLE IF NOT EXISTS wallet_quote_observations (
               observation_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               instance_id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               session_id TEXT NOT NULL,
               principal_id TEXT NOT NULL REFERENCES principals(id),
               operation_id TEXT NOT NULL,
               observation_role_json TEXT NOT NULL,
               wallet_id TEXT NOT NULL,
               mint_id TEXT NOT NULL,
               direction_json TEXT NOT NULL,
               quote_id TEXT NOT NULL,
               amount_sat INTEGER NOT NULL,
               state TEXT NOT NULL,
               wallet_created_at INTEGER,
               wallet_paid_at INTEGER,
               wallet_expires_at INTEGER,
               fee_reserve_sat INTEGER,
               fee_paid_sat INTEGER,
               observed_at INTEGER NOT NULL,
               UNIQUE (workspace_id, operation_id, observation_role_json),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id),
               FOREIGN KEY (workspace_id, experiment_id) REFERENCES experiments(workspace_id, id),
               FOREIGN KEY (workspace_id, session_id) REFERENCES sessions(workspace_id, id),
               FOREIGN KEY (workspace_id, operation_id) REFERENCES actions(workspace_id, id)
             );
             CREATE INDEX IF NOT EXISTS actions_by_instance_activity
               ON actions(workspace_id, instance_id, accepted_at DESC, id DESC);
             CREATE INDEX IF NOT EXISTS sessions_by_instance
               ON sessions(workspace_id, instance_id, id);
             CREATE INDEX IF NOT EXISTS wallet_quote_observations_latest
               ON wallet_quote_observations(
                 workspace_id, instance_id, wallet_id, mint_id, direction_json,
                 quote_id, observation_sequence DESC
               );
             CREATE INDEX IF NOT EXISTS wallet_quote_observations_by_experiment
               ON wallet_quote_observations(
                 workspace_id, experiment_id, principal_id, observation_sequence
               );
             CREATE TABLE IF NOT EXISTS wallet_payment_claims (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               instance_id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               session_id TEXT NOT NULL,
               principal_id TEXT NOT NULL REFERENCES principals(id),
               operation_id TEXT NOT NULL,
               recipient_wallet_id TEXT NOT NULL,
               recipient_mint_id TEXT NOT NULL,
               mint_quote_id TEXT NOT NULL,
               payer_wallet_id TEXT NOT NULL,
               payer_mint_id TEXT NOT NULL,
               admitted_at INTEGER NOT NULL,
               PRIMARY KEY (
                 workspace_id, instance_id, recipient_wallet_id,
                 recipient_mint_id, mint_quote_id
               ),
               UNIQUE (workspace_id, operation_id),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id),
               FOREIGN KEY (workspace_id, experiment_id) REFERENCES experiments(workspace_id, id),
               FOREIGN KEY (workspace_id, session_id) REFERENCES sessions(workspace_id, id),
               FOREIGN KEY (workspace_id, operation_id) REFERENCES actions(workspace_id, id)
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
        migration::upgrade(&mut connection)?;
        Ok(Self {
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
        self.authorize(workspace, principal, Capability::LabRead)?;
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
        let request = serde_json::json!({
            "candidateId": candidate.id,
            "implementation": candidate.implementation,
            "baseVersion": candidate.base_version,
            "pullRequestUrl": candidate.pull_request_url,
        });
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "candidate.build",
            &request,
        )? {
            return Ok(response);
        }
        let inserted = self.lock()?.execute(
            "INSERT OR IGNORE INTO candidate_builds(
               workspace_id, id, principal_id, resource_name, request_digest,
               build_json, accepted_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                workspace,
                candidate.id,
                principal,
                candidate.resource_name,
                candidate.request_digest,
                serde_json::to_string(candidate)?,
                candidate.accepted_at_unix,
            ],
        )?;
        if inserted == 0 {
            let existing = self.candidate_build_unchecked(workspace, &candidate.id)?;
            if existing != *candidate {
                return Err(StoreError::Conflict {
                    resource: "candidate build",
                    id: candidate.id.clone(),
                });
            }
        }
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "candidate.build",
            &request,
            candidate,
        )?;
        Ok(candidate.clone())
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

    pub fn update_candidate_build(
        &self,
        workspace: &str,
        candidate: &CandidateBuild,
    ) -> Result<CandidateBuild, StoreError> {
        let current = self.candidate_build_unchecked(workspace, &candidate.id)?;
        validate_candidate_update(&current, candidate)?;
        self.lock()?.execute(
            "UPDATE candidate_builds SET build_json = ?1
             WHERE workspace_id = ?2 AND id = ?3",
            params![serde_json::to_string(candidate)?, workspace, candidate.id],
        )?;
        Ok(candidate.clone())
    }

    pub fn effective_catalog(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<CatalogResponse, StoreError> {
        self.authorize(workspace, principal, Capability::CatalogRead)?;
        self.effective_catalog_unchecked(workspace)
    }

    pub fn create_draft(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        lab: &LabSpec,
        idempotency_key: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::LabCreate)?;
        let request = serde_json::json!({"id": id, "lab": lab});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lab.create",
            &request,
        )? {
            return Ok(response);
        }
        let draft = Draft {
            id: id.to_owned(),
            workspace_id: workspace.to_owned(),
            version: 1,
            lab: lab.clone(),
        };
        let inserted = self.lock()?.execute(
            "INSERT INTO drafts(workspace_id, id, version, lab_json) VALUES (?1, ?2, 1, ?3) ON CONFLICT(workspace_id, id) DO NOTHING",
            params![workspace, id, serde_json::to_string(lab)?],
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
            "lab.create",
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
        self.authorize(workspace, principal, Capability::LabRead)?;
        self.read_draft_unchecked(workspace, id)
    }

    pub fn edit_draft(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        expected_version: u64,
        lab: &LabSpec,
        idempotency_key: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::LabEdit)?;
        let request =
            serde_json::json!({"id": id, "expectedVersion": expected_version, "lab": lab});
        if let Some(response) =
            self.idempotent_response(workspace, principal, idempotency_key, "lab.edit", &request)?
        {
            return Ok(response);
        }
        let changed = self.lock()?.execute(
            "UPDATE drafts SET version = version + 1, lab_json = ?1
             WHERE workspace_id = ?2 AND id = ?3 AND version = ?4",
            params![
                serde_json::to_string(lab)?,
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
            "lab.edit",
            &request,
            &draft,
        )?;
        Ok(draft)
    }

    pub fn mutate_draft(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        expected_version: u64,
        mutation: &DraftMutation,
        idempotency_key: &str,
    ) -> Result<Draft, StoreError> {
        self.authorize(workspace, principal, Capability::LabEdit)?;
        let request = serde_json::json!({
            "id": id,
            "expectedVersion": expected_version,
            "mutation": mutation,
        });
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lab.mutate",
            &request,
        )? {
            return Ok(response);
        }
        let current = self.read_draft_unchecked(workspace, id)?;
        if current.version != expected_version {
            return Err(StoreError::StaleDraft {
                draft: id.to_owned(),
                expected: expected_version,
                actual: current.version,
            });
        }
        let mut lab = current.lab.clone();
        let catalog = self.effective_catalog_unchecked(workspace)?;
        apply_draft_mutation(&mut lab, mutation, &catalog).map_err(StoreError::Validation)?;
        if lab == current.lab {
            self.record_idempotency(
                workspace,
                principal,
                idempotency_key,
                "lab.mutate",
                &request,
                &current,
            )?;
            return Ok(current);
        }
        let changed = self.lock()?.execute(
            "UPDATE drafts SET version = version + 1, lab_json = ?1
             WHERE workspace_id = ?2 AND id = ?3 AND version = ?4",
            params![
                serde_json::to_string(&lab)?,
                workspace,
                id,
                sql_version(expected_version)?
            ],
        )?;
        if changed == 0 {
            let latest = self.read_draft_unchecked(workspace, id)?;
            return Err(StoreError::StaleDraft {
                draft: id.to_owned(),
                expected: expected_version,
                actual: latest.version,
            });
        }
        let draft = self.read_draft_unchecked(workspace, id)?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lab.mutate",
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
        self.authorize(workspace, principal, Capability::LabClone)?;
        let request = serde_json::json!({"source": source, "target": target});
        if let Some(response) =
            self.idempotent_response(workspace, principal, idempotency_key, "lab.clone", &request)?
        {
            return Ok(response);
        }
        let source = self.read_draft_unchecked(workspace, source)?;
        let draft = Draft {
            id: target.to_owned(),
            workspace_id: workspace.to_owned(),
            version: 1,
            lab: source.lab,
        };
        self.lock()?.execute(
            "INSERT INTO drafts(workspace_id, id, version, lab_json) VALUES (?1, ?2, 1, ?3)",
            params![workspace, target, serde_json::to_string(&draft.lab)?],
        )?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lab.clone",
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
            .lab
            .components
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        let to_ids = to
            .lab
            .components
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        Ok(DraftDiff {
            from_version: from.version,
            to_version: to.version,
            added_components: to_ids.difference(&from_ids).cloned().collect(),
            removed_components: from_ids.difference(&to_ids).cloned().collect(),
            links_changed: from.lab.links != to.lab.links,
            policy_changed: from.lab.policy != to.lab.policy,
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
        self.authorize(workspace, principal, Capability::LabPublish)?;
        let request = serde_json::json!({"draftId": draft_id, "expectedVersion": expected_version});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lab.publish",
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
        let report = validate_lab(&draft.lab);
        if !report.valid {
            return Err(StoreError::Validation(serde_json::to_string(
                &report.issues,
            )?));
        }
        let catalog = self.effective_catalog_unchecked(workspace)?;
        let effective_lab =
            resolve_effective_lab(&draft.lab, &catalog).map_err(StoreError::Catalog)?;
        let lock = resolve_lock(&effective_lab, &catalog).map_err(StoreError::Catalog)?;
        let digest = proofstorm_core::publication_digest(workspace, &effective_lab, &lock);
        let revision = PublishedRevision {
            workspace_id: workspace.to_owned(),
            digest: digest.clone(),
            lab: effective_lab,
            lock,
        };
        self.lock()?.execute(
            "INSERT OR IGNORE INTO revisions(digest, workspace_id, draft_id, draft_version, revision_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![digest, workspace, draft_id, sql_version(draft.version)?, serde_json::to_string(&revision)?],
        )?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lab.publish",
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
        self.authorize(workspace, principal, Capability::LabRead)?;
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
    ) -> Result<LabInstance, StoreError> {
        self.authorize(workspace, principal, Capability::LabMaterialize)?;
        if !is_slug(instance_id) {
            return Err(StoreError::Validation(
                "instance id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        let request =
            serde_json::json!({"instanceId": instance_id, "revisionDigest": revision_digest});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lab.materialize",
            &request,
        )? {
            return Ok(response);
        }
        let revision = self.revision_unchecked(workspace, revision_digest)?;
        let identity = proofstorm_core::digest_json(&(workspace, instance_id, revision_digest));
        let instance_key = format!("i{}", &identity[7..26]);
        let instance = LabInstance {
            id: instance_id.to_owned(),
            workspace_id: workspace.to_owned(),
            revision_digest: revision_digest.to_owned(),
            lock_digest: revision.lock.digest,
            resource_name: format!("lab-{instance_key}"),
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
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lab.materialize",
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
    ) -> Result<LabInstance, StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        self.instance_unchecked(workspace, id)
    }

    pub fn instance_for_close(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<LabInstance, StoreError> {
        self.authorize(workspace, principal, Capability::LabClose)?;
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
        self.authorize(workspace, principal, Capability::LabOperate)?;
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
        self.authorize(workspace, principal, Capability::LabMaterialize)?;
        self.revision_unchecked(workspace, digest)
    }

    pub fn operation_context(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        capability: Capability,
    ) -> Result<(LabInstance, PublishedRevision), StoreError> {
        self.authorize(workspace, principal, capability)?;
        let instance = self.instance_unchecked(workspace, instance_id)?;
        let revision = self.revision_unchecked(workspace, &instance.revision_digest)?;
        Ok((instance, revision))
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
    ) -> Result<LabOperation, StoreError> {
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
    pub fn create_wallet_pay_operation(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        experiment_id: &str,
        session_id: &str,
        operation_id: &str,
        request: &serde_json::Value,
        idempotency_key: &str,
        recipient_wallet_id: &str,
        recipient_mint_id: &str,
        mint_quote_id: &str,
        payer_wallet_id: &str,
        payer_mint_id: &str,
    ) -> Result<LabOperation, StoreError> {
        validate_quote_observation_identity(recipient_wallet_id, recipient_mint_id, mint_quote_id)?;
        if !is_slug(payer_wallet_id) || !is_slug(payer_mint_id) {
            return Err(StoreError::Validation(
                "payer wallet and mint ids must be lowercase kebab-case identifiers of 1..=63 bytes"
                    .into(),
            ));
        }
        self.create_operation_inner(
            workspace,
            principal,
            instance_id,
            experiment_id,
            session_id,
            operation_id,
            OperationKind::WalletPay,
            request,
            idempotency_key,
            Capability::WalletControl,
            Some(PaymentClaimInput {
                recipient_wallet: recipient_wallet_id,
                recipient_mint: recipient_mint_id,
                mint_quote: mint_quote_id,
                payer_wallet: payer_wallet_id,
                payer_mint: payer_mint_id,
            }),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "action identity, session, quota, operation, and optional payment claim are one atomic admission transaction"
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
        payment_claim: Option<PaymentClaimInput<'_>>,
    ) -> Result<LabOperation, StoreError> {
        self.authorize(workspace, principal, capability)?;
        if !is_slug(operation_id) {
            return Err(StoreError::Validation(
                "operation id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        self.instance_unchecked(workspace, instance_id)?;
        let envelope = serde_json::json!({
            "instanceId": instance_id, "experimentId": experiment_id,
            "sessionId": session_id, "operationId": operation_id,
            "kind": kind, "request": request
        });
        if let Some(response) = self.idempotent_response::<LabOperation, _>(
            workspace,
            principal,
            idempotency_key,
            "lab.operation.create",
            &envelope,
        )? {
            return self.operation_unchecked(workspace, &response.id);
        }
        self.authorize_operation_access(workspace, principal, instance_id, kind, request)?;
        let run = self.experiment_unchecked(workspace, experiment_id)?;
        if run.instance_id != instance_id || run.phase != ExperimentPhase::Active {
            return Err(StoreError::Validation(
                "action run must be open and belong to this lab".into(),
            ));
        }
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
        let handle_phase: Option<String> = transaction
            .query_row(
                "SELECT phase FROM lab_handles WHERE workspace_id=?1 AND instance_id=?2",
                params![workspace, instance_id],
                |row| row.get(0),
            )
            .optional()?;
        if handle_phase.is_some_and(|phase| phase != "\"open\"") {
            return Err(StoreError::Validation(
                "lab is closing; new actions are not admitted".into(),
            ));
        }
        transaction.execute("UPDATE sessions SET last_activity_at=MAX(last_activity_at,?1) WHERE workspace_id=?2 AND id=?3",params![accepted_at,workspace,session_id])?;
        let last_sequence = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM actions
             WHERE workspace_id = ?1 AND experiment_id = ?2",
            params![workspace, experiment_id],
            |row| row.get::<_, i64>(0),
        )?;
        let sequence = u64::try_from(last_sequence + 1)
            .map_err(|_| StoreError::InvalidStoredVersion(last_sequence))?;
        let operation = LabOperation {
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
        let active = transaction.query_row(
            "SELECT COUNT(*) FROM actions
             WHERE workspace_id = ?1 AND instance_id = ?2 AND id <> ?3
               AND phase_json IN ('\"pending\"', '\"running\"')
               AND accepted_at >= unixepoch() - 600",
            params![workspace, instance_id, operation_id],
            |row| row.get::<_, u32>(0),
        )?;
        if active >= MAX_ACTIVE_OPERATIONS {
            return Err(StoreError::OperationLimit {
                instance: instance_id.to_owned(),
                active,
                maximum: MAX_ACTIVE_OPERATIONS,
            });
        }
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO actions(workspace_id, id, instance_id, experiment_id, session_id,
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
        if inserted == 0 {
            let (existing_digest, existing_kind, existing_session) = transaction.query_row(
                "SELECT request_digest, kind_json, session_id FROM actions
                 WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            if existing_digest != operation.request_digest
                || existing_kind != serde_json::to_string(&kind)?
                || existing_session != session_id
            {
                return Err(StoreError::Conflict {
                    resource: "operation",
                    id: operation_id.to_owned(),
                });
            }
        }
        if let Some(claim) = payment_claim {
            let claim_inserted = transaction.execute(
                "INSERT OR IGNORE INTO wallet_payment_claims(
                   workspace_id, instance_id, experiment_id, session_id, principal_id,
                   operation_id, recipient_wallet_id, recipient_mint_id, mint_quote_id,
                   payer_wallet_id, payer_mint_id, admitted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    workspace,
                    instance_id,
                    experiment_id,
                    session_id,
                    principal,
                    operation_id,
                    claim.recipient_wallet,
                    claim.recipient_mint,
                    claim.mint_quote,
                    claim.payer_wallet,
                    claim.payer_mint,
                    accepted_at,
                ],
            )?;
            if claim_inserted == 0 {
                let existing_operation = transaction.query_row(
                    "SELECT operation_id FROM wallet_payment_claims
                     WHERE workspace_id = ?1 AND instance_id = ?2
                       AND recipient_wallet_id = ?3 AND recipient_mint_id = ?4
                       AND mint_quote_id = ?5",
                    params![
                        workspace,
                        instance_id,
                        claim.recipient_wallet,
                        claim.recipient_mint,
                        claim.mint_quote,
                    ],
                    |row| row.get::<_, String>(0),
                )?;
                if existing_operation != operation_id {
                    return Err(StoreError::QuotePaymentAlreadyClaimed {
                        quote: claim.mint_quote.to_owned(),
                        operation: existing_operation,
                    });
                }
            }
        }
        transaction.commit()?;
        drop(connection);
        if inserted == 0 {
            let existing = self.operation_unchecked(workspace, operation_id)?;
            if existing.request_digest != operation.request_digest
                || existing.kind != kind
                || existing.session_id != session_id
            {
                return Err(StoreError::Conflict {
                    resource: "operation",
                    id: operation_id.to_owned(),
                });
            }
            self.record_idempotency(
                workspace,
                principal,
                idempotency_key,
                "lab.operation.create",
                &envelope,
                &existing,
            )?;
            return Ok(existing);
        }
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lab.operation.create",
            &envelope,
            &operation,
        )?;
        Ok(operation)
    }

    pub fn operation(
        &self,
        workspace: &str,
        principal: &str,
        operation_id: &str,
    ) -> Result<LabOperation, StoreError> {
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        self.operation_unchecked(workspace, operation_id)
    }

    pub fn operation_for_cancel(
        &self,
        workspace: &str,
        principal: &str,
        operation_id: &str,
    ) -> Result<LabOperation, StoreError> {
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

    pub fn actions(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<LabOperation>, StoreError> {
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

    /// Every pending or running operation recorded for one lab instance, in
    /// journal order. The store is the ledger of record, so lab close uses this
    /// to finalize operations whose runtime resources are about to disappear.
    pub fn active_operations(
        &self,
        workspace: &str,
        instance_id: &str,
    ) -> Result<Vec<LabOperation>, StoreError> {
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
    ) -> Result<LabOperation, StoreError> {
        self.record_operation_result_with_quote_observations(
            workspace,
            operation_id,
            phase,
            content,
            &[],
        )
    }

    /// Atomically terminalize an action-journal entry and append every
    /// wallet-native quote observation decoded from its sanitized artifact.
    pub fn record_operation_result_with_quote_observations(
        &self,
        workspace: &str,
        operation_id: &str,
        phase: OperationPhase,
        content: serde_json::Value,
        observations: &[WalletQuoteObservationInput],
    ) -> Result<LabOperation, StoreError> {
        if matches!(phase, OperationPhase::Pending | OperationPhase::Running) {
            return Err(StoreError::Validation(
                "operation result phase must be terminal".into(),
            ));
        }
        for observation in observations {
            validate_quote_observation(observation)?;
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
        for observation in observations {
            transaction.execute(
                "INSERT INTO wallet_quote_observations(
                   workspace_id, instance_id, experiment_id, session_id, principal_id,
                   operation_id, observation_role_json, wallet_id, mint_id,
                   direction_json, quote_id, amount_sat, state, wallet_created_at,
                   wallet_paid_at, wallet_expires_at, fee_reserve_sat, fee_paid_sat,
                   observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![
                    workspace,
                    existing.instance_id,
                    existing.experiment_id,
                    existing.session_id,
                    existing.principal_id,
                    operation_id,
                    serde_json::to_string(&observation.role)?,
                    observation.wallet_id,
                    observation.mint_id,
                    serde_json::to_string(&observation.direction)?,
                    observation.quote_id,
                    sql_version(observation.amount_sat)?,
                    observation.state,
                    observation.wallet_created_at_unix,
                    observation.wallet_paid_at_unix,
                    observation.wallet_expires_at_unix,
                    observation.fee_reserve_sat.map(sql_version).transpose()?,
                    observation.fee_paid_sat.map(sql_version).transpose()?,
                    completed_at,
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.operation_unchecked(workspace, operation_id)
    }

    /// Read the most recently recorded observation for one fully scoped
    /// adapter-native quote. This is historical data, not a live wallet read.
    #[allow(clippy::too_many_arguments)]
    pub fn wallet_quote_observation(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        wallet_id: &str,
        mint_id: &str,
        direction: WalletQuoteDirection,
        quote_id: &str,
    ) -> Result<WalletQuoteObservation, StoreError> {
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        validate_quote_observation_identity(wallet_id, mint_id, quote_id)?;
        let direction_json = serde_json::to_string(&direction)?;
        let row = self
            .lock()?
            .query_row(
                "SELECT observation_sequence, workspace_id, instance_id,
                        experiment_id, session_id, principal_id, operation_id,
                        observation_role_json, wallet_id, mint_id, direction_json,
                        quote_id, amount_sat, state, wallet_created_at,
                        wallet_paid_at, wallet_expires_at, fee_reserve_sat,
                        fee_paid_sat, observed_at
                 FROM wallet_quote_observations
                 WHERE workspace_id = ?1 AND instance_id = ?2 AND wallet_id = ?3
                   AND mint_id = ?4 AND direction_json = ?5 AND quote_id = ?6
                 ORDER BY observation_sequence DESC LIMIT 1",
                params![
                    workspace,
                    instance_id,
                    wallet_id,
                    mint_id,
                    direction_json,
                    quote_id
                ],
                wallet_quote_observation_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "wallet quote observation",
                id: quote_id.to_owned(),
            })?;
        let observation = WalletQuoteObservation::try_from(row)?;
        if observation.principal_id != principal {
            return Err(StoreError::QuoteOwnerMismatch {
                quote: quote_id.to_owned(),
                owner: observation.principal_id,
                principal: principal.to_owned(),
            });
        }
        Ok(observation)
    }

    /// List the latest stored observation for each fully scoped quote in an
    /// experiment, ordered by the sequence of that latest observation.
    pub fn wallet_quote_observations(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        after_sequence: u64,
        through_sequence: u64,
        limit: u32,
    ) -> Result<Vec<WalletQuoteObservation>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.experiment_unchecked(workspace, experiment_id)?;
        if !(1..=100).contains(&limit) {
            return Err(StoreError::Validation(
                "wallet quote observation list limit must be 1..=100".into(),
            ));
        }
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT observation_sequence, workspace_id, instance_id,
                    experiment_id, session_id, principal_id, operation_id,
                    observation_role_json, wallet_id, mint_id, direction_json,
                    quote_id, amount_sat, state, wallet_created_at,
                    wallet_paid_at, wallet_expires_at, fee_reserve_sat,
                    fee_paid_sat, observed_at
             FROM wallet_quote_observations AS observation
             WHERE workspace_id = ?1 AND experiment_id = ?2 AND principal_id = ?3
               AND observation_sequence > ?4 AND observation_sequence <= ?5
               AND observation_sequence = (
                 SELECT MAX(candidate.observation_sequence)
                 FROM wallet_quote_observations AS candidate
                 WHERE candidate.workspace_id = observation.workspace_id
                   AND candidate.instance_id = observation.instance_id
                   AND candidate.wallet_id = observation.wallet_id
                   AND candidate.mint_id = observation.mint_id
                   AND candidate.direction_json = observation.direction_json
                   AND candidate.quote_id = observation.quote_id
                   AND candidate.observation_sequence <= ?5
               )
             ORDER BY observation_sequence ASC LIMIT ?6",
        )?;
        let rows = statement
            .query_map(
                params![
                    workspace,
                    experiment_id,
                    principal,
                    sql_version(after_sequence)?,
                    sql_version(through_sequence)?,
                    limit
                ],
                wallet_quote_observation_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(WalletQuoteObservation::try_from)
            .collect()
    }

    pub fn wallet_quote_observation_max_sequence(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
    ) -> Result<u64, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.experiment_unchecked(workspace, experiment_id)?;
        let sequence = self.lock()?.query_row(
            "SELECT COALESCE(MAX(observation_sequence), 0)
             FROM wallet_quote_observations
             WHERE workspace_id = ?1 AND experiment_id = ?2 AND principal_id = ?3",
            params![workspace, experiment_id, principal],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(sequence).map_err(|_| StoreError::InvalidStoredVersion(sequence))
    }

    pub fn update_operation_phase(
        &self,
        workspace: &str,
        operation_id: &str,
        phase: OperationPhase,
    ) -> Result<LabOperation, StoreError> {
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
            .map(|encoded| serde_json::from_str(&encoded).map_err(StoreError::from))
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
            .map(|encoded| {
                serde_json::from_str(&encoded.map_err(StoreError::from)?).map_err(StoreError::from)
            })
            .collect()
    }

    fn effective_catalog_unchecked(&self, workspace: &str) -> Result<CatalogResponse, StoreError> {
        effective_catalog(
            default_catalog(),
            &self.candidate_builds_unchecked(workspace)?,
        )
        .map_err(StoreError::Catalog)
    }

    fn read_draft_unchecked(&self, workspace: &str, id: &str) -> Result<Draft, StoreError> {
        let record = self
            .lock()?
            .query_row(
                "SELECT version, lab_json FROM drafts WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let (version, lab_json) = record.ok_or_else(|| StoreError::NotFound {
            resource: "draft",
            id: id.to_owned(),
        })?;
        let version =
            u64::try_from(version).map_err(|_| StoreError::InvalidStoredVersion(version))?;
        Ok(Draft {
            id: id.to_owned(),
            workspace_id: workspace.to_owned(),
            version,
            lab: serde_json::from_str(&lab_json)?,
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

    fn instance_unchecked(&self, workspace: &str, id: &str) -> Result<LabInstance, StoreError> {
        self.lock()?
            .query_row(
                "SELECT revision_digest, lock_digest, instance_key, resource_name
                 FROM instances WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| {
                    Ok(LabInstance {
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

    fn operation_unchecked(&self, workspace: &str, id: &str) -> Result<LabOperation, StoreError> {
        self.lock()?
            .query_row(
                "SELECT instance_id, experiment_id, session_id, principal_id, sequence, kind_json,
                        capability_json, resource_name, request_digest, request_json, phase_json,
                        accepted_at, started_at, completed_at, artifact_json
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
                )| {
                    let sequence = u64::try_from(sequence)
                        .map_err(|_| StoreError::InvalidStoredVersion(sequence))?;
                    Ok::<LabOperation, StoreError>(LabOperation {
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
        let found = self
            .lock()?
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
        self.lock()?.execute(
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

fn validate_quote_observation_identity(
    wallet_id: &str,
    mint_id: &str,
    quote_id: &str,
) -> Result<(), StoreError> {
    if !is_slug(wallet_id) || !is_slug(mint_id) || !is_slug(quote_id) {
        return Err(StoreError::Validation(
            "wallet, mint, and adapter quote ids must be lowercase kebab-case identifiers of 1..=63 bytes"
                .into(),
        ));
    }
    Ok(())
}

fn validate_quote_observation(observation: &WalletQuoteObservationInput) -> Result<(), StoreError> {
    validate_quote_observation_identity(
        &observation.wallet_id,
        &observation.mint_id,
        &observation.quote_id,
    )?;
    if !(1..=500_000).contains(&observation.amount_sat) {
        return Err(StoreError::Validation(
            "wallet quote observation amount_sat must be 1..=500000".into(),
        ));
    }
    if observation.state.is_empty()
        || observation.state.len() > 63
        || !observation
            .state
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StoreError::Validation(
            "wallet quote observation state must contain 1..=63 ASCII letters, digits, hyphens, or underscores"
                .into(),
        ));
    }
    let expected_direction = match observation.role {
        WalletQuoteObservationRole::InvoiceReceive
        | WalletQuoteObservationRole::PaymentReceive
        | WalletQuoteObservationRole::ClaimReceive => WalletQuoteDirection::Receive,
        WalletQuoteObservationRole::PaymentMelt => WalletQuoteDirection::Pay,
    };
    if observation.direction != expected_direction {
        return Err(StoreError::Validation(
            "wallet quote observation role and direction disagree".into(),
        ));
    }
    if observation.direction == WalletQuoteDirection::Receive
        && (observation.fee_reserve_sat.is_some() || observation.fee_paid_sat.is_some())
    {
        return Err(StoreError::Validation(
            "receive quote observations cannot carry melt fee fields".into(),
        ));
    }
    Ok(())
}

fn wallet_quote_observation_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WalletQuoteObservationRow> {
    Ok(WalletQuoteObservationRow {
        observation_sequence: row.get(0)?,
        workspace_id: row.get(1)?,
        instance_id: row.get(2)?,
        experiment_id: row.get(3)?,
        session_id: row.get(4)?,
        principal_id: row.get(5)?,
        operation_id: row.get(6)?,
        observation_role_json: row.get(7)?,
        wallet_id: row.get(8)?,
        mint_id: row.get(9)?,
        direction_json: row.get(10)?,
        quote_id: row.get(11)?,
        amount_sat: row.get(12)?,
        state: row.get(13)?,
        wallet_created_at_unix: row.get(14)?,
        wallet_paid_at_unix: row.get(15)?,
        wallet_expires_at_unix: row.get(16)?,
        fee_reserve_sat: row.get(17)?,
        fee_paid_sat: row.get(18)?,
        observed_at_unix: row.get(19)?,
    })
}

fn optional_sql_u64(value: Option<i64>) -> Result<Option<u64>, StoreError> {
    value
        .map(|value| u64::try_from(value).map_err(|_| StoreError::InvalidStoredVersion(value)))
        .transpose()
}

fn validate_candidate_build(
    workspace: &str,
    principal: &str,
    candidate: &CandidateBuild,
) -> Result<(), StoreError> {
    if candidate.api_version != proofstorm_core::CANDIDATE_BUILD_API_VERSION
        || candidate.workspace_id != workspace
        || candidate.principal_id != principal
        || !is_slug(&candidate.id)
        || !is_slug(&candidate.implementation)
        || candidate.base_version.is_empty()
        || candidate.phase != CandidateBuildPhase::Pending
        || candidate.repository.as_deref().is_none_or(str::is_empty)
        || candidate.version.as_deref().is_none_or(str::is_empty)
        || candidate.image.is_some()
        || candidate.error_code.is_some()
        || candidate.error_message.is_some()
    {
        return Err(StoreError::Validation(
            "candidate build has an invalid immutable identity or initial state".into(),
        ));
    }
    if !candidate
        .pull_request_url
        .starts_with("https://github.com/")
    {
        return Err(StoreError::Validation(
            "candidate pull request must be a public https://github.com URL".into(),
        ));
    }
    let commit_sha = candidate.commit_sha.as_deref().unwrap_or_default();
    if commit_sha.len() != 40 || !commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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
    if base.support_lifecycle == proofstorm_core::SupportLifecycle::Experimental {
        return Err(StoreError::Validation(
            "candidate builds must derive from a built-in release".into(),
        ));
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
