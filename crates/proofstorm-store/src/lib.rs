#![allow(
    clippy::missing_errors_doc,
    reason = "all public store operations return the documented StoreError contract"
)]

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use proofstorm_core::{
    Capability, DraftMutation, Experiment, ExperimentLease, ExperimentPhase, LabInstance,
    LabOperation, LabSpec, LeasePhase, OperationArtifact, OperationKind, OperationPhase,
    PublishedRevision, WalletQuote, WalletQuoteDirection, WalletQuotePhase, apply_draft_mutation,
    default_catalog, resolve_lock, validate_lab,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const MAX_ACTIVE_OPERATIONS: u32 = 4;
const MAX_ARTIFACT_BYTES: usize = 32 * 1024;

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
    #[error("lab instance {instance:?} has active experiment lease {lease:?}")]
    InstanceLeased { instance: String, lease: String },
    #[error("experiment {experiment:?} has active lease {lease:?}")]
    ExperimentLeased { experiment: String, lease: String },
    #[error("experiment lease {lease:?} belongs to principal {owner:?}, not {principal:?}")]
    LeaseOwnerMismatch {
        lease: String,
        owner: String,
        principal: String,
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
    #[error("experiment lease {lease:?} is not active")]
    LeaseInactive { lease: String },
    #[error("experiment lease {lease:?} exhausted its {maximum}-action budget")]
    ActionBudgetExceeded { lease: String, maximum: u32 },
    #[error("wallet quote {quote:?} expected phase {expected:?}, current phase is {actual:?}")]
    StaleQuote {
        quote: String,
        expected: WalletQuotePhase,
        actual: WalletQuotePhase,
    },
    #[error("wallet quote {quote:?} cannot transition from {from:?} to {to:?}")]
    InvalidQuoteTransition {
        quote: String,
        from: WalletQuotePhase,
        to: WalletQuotePhase,
    },
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
            Self::InstanceLeased { .. } => "instance_leased",
            Self::ExperimentLeased { .. } => "experiment_leased",
            Self::LeaseOwnerMismatch { .. } => "lease_owner_mismatch",
            Self::OperationOwnerMismatch { .. } => "operation_owner_mismatch",
            Self::QuoteOwnerMismatch { .. } => "quote_owner_mismatch",
            Self::LeaseInactive { .. } => "lease_inactive",
            Self::ActionBudgetExceeded { .. } => "action_budget_exceeded",
            Self::StaleQuote { .. } => "stale_quote",
            Self::InvalidQuoteTransition { .. } => "invalid_quote_transition",
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

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
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
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS workspaces (
               id TEXT PRIMARY KEY, name TEXT NOT NULL
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
             CREATE TABLE IF NOT EXISTS experiment_leases (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               principal_id TEXT NOT NULL REFERENCES principals(id),
               phase_json TEXT NOT NULL,
               acquired_at INTEGER NOT NULL,
               expires_at INTEGER NOT NULL,
               max_actions INTEGER NOT NULL,
               released_at INTEGER,
               PRIMARY KEY (workspace_id, id),
               FOREIGN KEY (workspace_id, experiment_id) REFERENCES experiments(workspace_id, id),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS one_active_lease_per_instance
               ON experiment_leases(workspace_id, instance_id)
               WHERE phase_json = '\"active\"';
             CREATE TABLE IF NOT EXISTS actions (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               lease_id TEXT NOT NULL,
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
               FOREIGN KEY (workspace_id, lease_id) REFERENCES experiment_leases(workspace_id, id)
             );
             CREATE TABLE IF NOT EXISTS wallet_quotes (
               workspace_id TEXT NOT NULL REFERENCES workspaces(id),
               id TEXT NOT NULL,
               instance_id TEXT NOT NULL,
               experiment_id TEXT NOT NULL,
               lease_id TEXT NOT NULL,
               principal_id TEXT NOT NULL REFERENCES principals(id),
               wallet_id TEXT NOT NULL,
               mint_id TEXT NOT NULL,
               direction_json TEXT NOT NULL,
               amount_sat INTEGER NOT NULL,
               phase_json TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               expires_at INTEGER,
               settled_at INTEGER,
               operation_id TEXT,
               terminal_code TEXT,
               PRIMARY KEY (workspace_id, id),
               FOREIGN KEY (workspace_id, instance_id) REFERENCES instances(workspace_id, id),
               FOREIGN KEY (workspace_id, experiment_id) REFERENCES experiments(workspace_id, id),
               FOREIGN KEY (workspace_id, lease_id) REFERENCES experiment_leases(workspace_id, id)
             );
             CREATE INDEX IF NOT EXISTS wallet_quotes_by_experiment
               ON wallet_quotes(workspace_id, experiment_id, created_at, id);
             CREATE INDEX IF NOT EXISTS wallet_quotes_experiment_page
               ON wallet_quotes(workspace_id, experiment_id, principal_id, id);
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
        Ok(Self {
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
                "INSERT INTO grants(workspace_id, principal_id, capability) VALUES (?1, ?2, ?3)",
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
        self.lock()?.execute(
            "INSERT INTO drafts(workspace_id, id, version, lab_json) VALUES (?1, ?2, 1, ?3)",
            params![workspace, id, serde_json::to_string(lab)?],
        )?;
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
        apply_draft_mutation(&mut lab, mutation, &default_catalog())
            .map_err(StoreError::Validation)?;
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
        let lock = resolve_lock(&draft.lab, &default_catalog()).map_err(StoreError::Catalog)?;
        let digest = proofstorm_core::publication_digest(workspace, &draft.lab, &lock);
        let revision = PublishedRevision {
            workspace_id: workspace.to_owned(),
            digest: digest.clone(),
            lab: draft.lab,
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
        let now = now_unix();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_leases(&transaction, workspace, now)?;
        let active = transaction
            .query_row(
                "SELECT id FROM experiment_leases
                 WHERE workspace_id = ?1 AND instance_id = ?2 AND phase_json = '\"active\"' LIMIT 1",
                params![workspace, id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        transaction.commit()?;
        drop(connection);
        if let Some(lease) = active {
            return Err(StoreError::InstanceLeased {
                instance: id.to_owned(),
                lease,
            });
        }
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

    pub fn experiment_for_lease(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
    ) -> Result<Experiment, StoreError> {
        self.authorize(workspace, principal, Capability::LeaseAcquire)?;
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
        expire_leases(&transaction, workspace, now)?;
        let active = transaction
            .query_row(
                "SELECT id FROM experiment_leases
                 WHERE workspace_id = ?1 AND experiment_id = ?2 AND phase_json = '\"active\"' LIMIT 1",
                params![workspace, experiment_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(lease) = active {
            return Err(StoreError::ExperimentLeased {
                experiment: experiment_id.to_owned(),
                lease,
            });
        }
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

    #[allow(
        clippy::too_many_arguments,
        reason = "lease authority, identity, deadline, budget, and idempotency are explicit"
    )]
    pub fn acquire_lease(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        lease_id: &str,
        duration_seconds: u32,
        max_actions: u32,
        idempotency_key: &str,
    ) -> Result<ExperimentLease, StoreError> {
        self.authorize(workspace, principal, Capability::LeaseAcquire)?;
        validate_lease_request(lease_id, duration_seconds, max_actions)?;
        let experiment = self.experiment_unchecked(workspace, experiment_id)?;
        if experiment.phase != ExperimentPhase::Active {
            return Err(StoreError::Validation(format!(
                "experiment {experiment_id:?} is closed"
            )));
        }
        let request = serde_json::json!({
            "experimentId": experiment_id, "leaseId": lease_id,
            "durationSeconds": duration_seconds, "maxActions": max_actions
        });
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lease.acquire",
            &request,
        )? {
            return Ok(response);
        }
        if let Ok(existing) = self.lease_unchecked(workspace, lease_id) {
            if existing.experiment_id != experiment_id
                || existing.principal_id != principal
                || existing.max_actions != max_actions
                || existing.expires_at_unix - existing.acquired_at_unix
                    != i64::from(duration_seconds)
            {
                return Err(StoreError::Conflict {
                    resource: "experiment lease",
                    id: lease_id.to_owned(),
                });
            }
            self.record_idempotency(
                workspace,
                principal,
                idempotency_key,
                "lease.acquire",
                &request,
                &existing,
            )?;
            return Ok(existing);
        }
        let acquired_at = now_unix();
        let expires_at = acquired_at + i64::from(duration_seconds);
        let lease = ExperimentLease {
            id: lease_id.to_owned(),
            workspace_id: workspace.to_owned(),
            experiment_id: experiment_id.to_owned(),
            instance_id: experiment.instance_id.clone(),
            principal_id: principal.to_owned(),
            phase: LeasePhase::Active,
            acquired_at_unix: acquired_at,
            expires_at_unix: expires_at,
            max_actions,
            released_at_unix: None,
        };
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_leases(&transaction, workspace, acquired_at)?;
        let active = transaction
            .query_row(
                "SELECT id FROM experiment_leases
                 WHERE workspace_id = ?1 AND instance_id = ?2 AND phase_json = '\"active\"' LIMIT 1",
                params![workspace, experiment.instance_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(active) = active {
            return Err(StoreError::InstanceLeased {
                instance: experiment.instance_id,
                lease: active,
            });
        }
        transaction.execute(
            "INSERT INTO experiment_leases(workspace_id, id, experiment_id, instance_id, principal_id,
             phase_json, acquired_at, expires_at, max_actions)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![workspace, lease_id, experiment_id, lease.instance_id, principal,
                serde_json::to_string(&LeasePhase::Active)?, acquired_at, expires_at, max_actions],
        )?;
        transaction.commit()?;
        drop(connection);
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lease.acquire",
            &request,
            &lease,
        )?;
        Ok(lease)
    }

    pub fn lease(
        &self,
        workspace: &str,
        principal: &str,
        lease_id: &str,
    ) -> Result<ExperimentLease, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.refresh_lease(workspace, lease_id)
    }

    pub fn release_lease(
        &self,
        workspace: &str,
        principal: &str,
        lease_id: &str,
        idempotency_key: &str,
    ) -> Result<ExperimentLease, StoreError> {
        self.authorize(workspace, principal, Capability::LeaseRelease)?;
        let request = serde_json::json!({"leaseId": lease_id});
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lease.release",
            &request,
        )? {
            return Ok(response);
        }
        let lease = self.refresh_lease(workspace, lease_id)?;
        if lease.principal_id != principal {
            return Err(StoreError::LeaseOwnerMismatch {
                lease: lease_id.to_owned(),
                owner: lease.principal_id,
                principal: principal.to_owned(),
            });
        }
        if lease.phase == LeasePhase::Active {
            let now = now_unix();
            self.lock()?.execute(
                "UPDATE experiment_leases SET phase_json = ?1, released_at = ?2
                 WHERE workspace_id = ?3 AND id = ?4 AND phase_json = '\"active\"'",
                params![
                    serde_json::to_string(&LeasePhase::Released)?,
                    now,
                    workspace,
                    lease_id
                ],
            )?;
        }
        let lease = self.lease_unchecked(workspace, lease_id)?;
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "lease.release",
            &request,
            &lease,
        )?;
        Ok(lease)
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
        reason = "quote identity and lease admission stay in one visibly atomic transaction"
    )]
    pub fn create_wallet_quote(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        experiment_id: &str,
        lease_id: &str,
        quote_id: &str,
        wallet_id: &str,
        mint_id: &str,
        direction: WalletQuoteDirection,
        amount_sat: u64,
        ttl_seconds: u32,
        idempotency_key: &str,
    ) -> Result<WalletQuote, StoreError> {
        let capability = match direction {
            WalletQuoteDirection::Receive => Capability::WalletFund,
            WalletQuoteDirection::Pay => Capability::WalletControl,
        };
        self.authorize(workspace, principal, capability)?;
        validate_wallet_quote_request(quote_id, wallet_id, mint_id, amount_sat, ttl_seconds)?;
        self.instance_unchecked(workspace, instance_id)?;
        let request = serde_json::json!({
            "instanceId": instance_id,
            "experimentId": experiment_id,
            "leaseId": lease_id,
            "quoteId": quote_id,
            "walletId": wallet_id,
            "mintId": mint_id,
            "direction": direction,
            "amountSat": amount_sat,
            "ttlSeconds": ttl_seconds,
        });
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "wallet.quote.create",
            &request,
        )? {
            return Ok(response);
        }

        let created_at = now_unix();
        let expires_at = created_at.saturating_add(i64::from(ttl_seconds));
        let quote = WalletQuote {
            id: quote_id.to_owned(),
            workspace_id: workspace.to_owned(),
            instance_id: instance_id.to_owned(),
            experiment_id: experiment_id.to_owned(),
            lease_id: lease_id.to_owned(),
            principal_id: principal.to_owned(),
            wallet_id: wallet_id.to_owned(),
            mint_id: mint_id.to_owned(),
            direction,
            amount_sat,
            phase: WalletQuotePhase::Requested,
            created_at_unix: created_at,
            updated_at_unix: created_at,
            expires_at_unix: Some(expires_at),
            settled_at_unix: None,
            operation_id: None,
            terminal_code: None,
        };

        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_leases(&transaction, workspace, created_at)?;
        let lease = transaction
            .query_row(
                "SELECT experiment_id, instance_id, principal_id, phase_json
                 FROM experiment_leases WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, lease_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "experiment lease",
                id: lease_id.to_owned(),
            })?;
        if lease.3 != serde_json::to_string(&LeasePhase::Active)? {
            return Err(StoreError::LeaseInactive {
                lease: lease_id.to_owned(),
            });
        }
        if lease.0 != experiment_id || lease.1 != instance_id {
            return Err(StoreError::Validation(
                "quote experiment, lease, and instance identity do not match".into(),
            ));
        }
        if lease.2 != principal {
            return Err(StoreError::LeaseOwnerMismatch {
                lease: lease_id.to_owned(),
                owner: lease.2,
                principal: principal.to_owned(),
            });
        }
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO wallet_quotes(
               workspace_id, id, instance_id, experiment_id, lease_id, principal_id,
               wallet_id, mint_id, direction_json, amount_sat, phase_json, created_at,
               updated_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                workspace,
                quote_id,
                instance_id,
                experiment_id,
                lease_id,
                principal,
                wallet_id,
                mint_id,
                serde_json::to_string(&direction)?,
                sql_version(amount_sat)?,
                serde_json::to_string(&quote.phase)?,
                created_at,
                created_at,
                expires_at,
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        if inserted == 0 {
            let existing = self.wallet_quote_unchecked(workspace, quote_id)?;
            if existing.instance_id != instance_id
                || existing.experiment_id != experiment_id
                || existing.lease_id != lease_id
                || existing.principal_id != principal
                || existing.wallet_id != wallet_id
                || existing.mint_id != mint_id
                || existing.direction != direction
                || existing.amount_sat != amount_sat
                || existing.expires_at_unix.is_none_or(|expires| {
                    expires.saturating_sub(existing.created_at_unix) != i64::from(ttl_seconds)
                })
            {
                return Err(StoreError::Conflict {
                    resource: "wallet quote",
                    id: quote_id.to_owned(),
                });
            }
            return Ok(existing);
        }
        self.record_idempotency(
            workspace,
            principal,
            idempotency_key,
            "wallet.quote.create",
            &request,
            &quote,
        )?;
        Ok(quote)
    }

    pub fn wallet_quote(
        &self,
        workspace: &str,
        principal: &str,
        quote_id: &str,
    ) -> Result<WalletQuote, StoreError> {
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        let quote = self.wallet_quote_unchecked(workspace, quote_id)?;
        if quote.principal_id != principal {
            return Err(StoreError::QuoteOwnerMismatch {
                quote: quote_id.to_owned(),
                owner: quote.principal_id,
                principal: principal.to_owned(),
            });
        }
        Ok(quote)
    }

    pub fn wallet_quotes(
        &self,
        workspace: &str,
        principal: &str,
        experiment_id: &str,
        after_quote_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<WalletQuote>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.experiment_unchecked(workspace, experiment_id)?;
        if !(1..=100).contains(&limit) {
            return Err(StoreError::Validation(
                "wallet quote list limit must be 1..=100".into(),
            ));
        }
        if after_quote_id.is_some_and(|id| !is_slug(id)) {
            return Err(StoreError::Validation(
                "wallet quote page cursor must be a lowercase kebab-case identifier of 1..=63 bytes"
                    .into(),
            ));
        }
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id FROM wallet_quotes
             WHERE workspace_id = ?1 AND experiment_id = ?2 AND principal_id = ?3
               AND id > COALESCE(?4, '')
             ORDER BY id LIMIT ?5",
        )?;
        let ids = statement
            .query_map(
                params![workspace, experiment_id, principal, after_quote_id, limit],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        ids.into_iter()
            .map(|id| self.wallet_quote_unchecked(workspace, &id))
            .collect()
    }

    pub fn transition_wallet_quote(
        &self,
        workspace: &str,
        quote_id: &str,
        expected: WalletQuotePhase,
        next: WalletQuotePhase,
        operation_id: Option<&str>,
        terminal_code: Option<&str>,
    ) -> Result<WalletQuote, StoreError> {
        let current = self.wallet_quote_unchecked(workspace, quote_id)?;
        if current.phase == next
            && operation_id.is_none_or(|id| current.operation_id.as_deref() == Some(id))
            && terminal_code.is_none_or(|code| current.terminal_code.as_deref() == Some(code))
        {
            return Ok(current);
        }
        if current.phase != expected {
            return Err(StoreError::StaleQuote {
                quote: quote_id.to_owned(),
                expected,
                actual: current.phase,
            });
        }
        if !current.phase.can_transition_to(next) {
            return Err(StoreError::InvalidQuoteTransition {
                quote: quote_id.to_owned(),
                from: current.phase,
                to: next,
            });
        }
        validate_quote_transition_metadata(next, operation_id, terminal_code)?;
        if let Some(existing) = current.operation_id.as_deref()
            && operation_id.is_some_and(|candidate| candidate != existing)
        {
            return Err(StoreError::Conflict {
                resource: "wallet quote operation",
                id: quote_id.to_owned(),
            });
        }
        let updated_at = now_unix();
        let settled_at = (next == WalletQuotePhase::Settled).then_some(updated_at);
        let changed = self.lock()?.execute(
            "UPDATE wallet_quotes
             SET phase_json = ?1, updated_at = ?2, settled_at = ?3,
                 operation_id = COALESCE(operation_id, ?4), terminal_code = ?5
             WHERE workspace_id = ?6 AND id = ?7 AND phase_json = ?8",
            params![
                serde_json::to_string(&next)?,
                updated_at,
                settled_at,
                operation_id,
                terminal_code,
                workspace,
                quote_id,
                serde_json::to_string(&expected)?,
            ],
        )?;
        if changed == 0 {
            let actual = self.wallet_quote_unchecked(workspace, quote_id)?;
            return Err(StoreError::StaleQuote {
                quote: quote_id.to_owned(),
                expected,
                actual: actual.phase,
            });
        }
        self.wallet_quote_unchecked(workspace, quote_id)
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "action identity and the lease, budget, sequence, quota, and insert checks remain one atomic admission transaction"
    )]
    pub fn create_operation(
        &self,
        workspace: &str,
        principal: &str,
        instance_id: &str,
        experiment_id: &str,
        lease_id: &str,
        operation_id: &str,
        kind: OperationKind,
        request: &serde_json::Value,
        idempotency_key: &str,
        capability: Capability,
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
            "leaseId": lease_id, "operationId": operation_id,
            "kind": kind, "request": request
        });
        if let Some(response) = self.idempotent_response(
            workspace,
            principal,
            idempotency_key,
            "lab.operation.create",
            &envelope,
        )? {
            return Ok(response);
        }
        let digest = proofstorm_core::digest_json(&(
            workspace,
            instance_id,
            lease_id,
            operation_id,
            &kind,
            request,
        ));
        let accepted_at = now_unix();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_leases(&transaction, workspace, accepted_at)?;
        let lease = transaction
            .query_row(
                "SELECT experiment_id, instance_id, principal_id, phase_json, max_actions
                 FROM experiment_leases WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, lease_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u32>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "experiment lease",
                id: lease_id.to_owned(),
            })?;
        if lease.3 != serde_json::to_string(&LeasePhase::Active)? {
            return Err(StoreError::LeaseInactive {
                lease: lease_id.to_owned(),
            });
        }
        if lease.0 != experiment_id || lease.1 != instance_id {
            return Err(StoreError::Validation(
                "action experiment, lease, and instance identity do not match".into(),
            ));
        }
        if lease.2 != principal {
            return Err(StoreError::LeaseOwnerMismatch {
                lease: lease_id.to_owned(),
                owner: lease.2,
                principal: principal.to_owned(),
            });
        }
        let last_sequence = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM actions
             WHERE workspace_id = ?1 AND experiment_id = ?2",
            params![workspace, experiment_id],
            |row| row.get::<_, i64>(0),
        )?;
        let action_count = transaction.query_row(
            "SELECT COUNT(*) FROM actions WHERE workspace_id = ?1 AND lease_id = ?2",
            params![workspace, lease_id],
            |row| row.get::<_, u32>(0),
        )?;
        if action_count >= lease.4 {
            return Err(StoreError::ActionBudgetExceeded {
                lease: lease_id.to_owned(),
                maximum: lease.4,
            });
        }
        let sequence = u64::try_from(last_sequence + 1)
            .map_err(|_| StoreError::InvalidStoredVersion(last_sequence))?;
        let operation = LabOperation {
            id: operation_id.to_owned(),
            workspace_id: workspace.to_owned(),
            instance_id: instance_id.to_owned(),
            experiment_id: experiment_id.to_owned(),
            lease_id: lease_id.to_owned(),
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
            "INSERT OR IGNORE INTO actions(workspace_id, id, instance_id, experiment_id, lease_id,
             principal_id, sequence, kind_json, capability_json, resource_name, request_digest,
             request_json, phase_json, accepted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                workspace,
                operation_id,
                instance_id,
                experiment_id,
                lease_id,
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
        transaction.commit()?;
        drop(connection);
        if inserted == 0 {
            let existing = self.operation_unchecked(workspace, operation_id)?;
            if existing.request_digest != operation.request_digest
                || existing.kind != kind
                || existing.lease_id != lease_id
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

    pub fn record_operation_result(
        &self,
        workspace: &str,
        operation_id: &str,
        phase: OperationPhase,
        content: serde_json::Value,
    ) -> Result<LabOperation, StoreError> {
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
        self.lock()?.execute(
            "UPDATE actions SET phase_json = ?1, artifact_json = ?2, completed_at = ?3
             WHERE workspace_id = ?4 AND id = ?5
               AND phase_json IN ('\"pending\"', '\"running\"')",
            params![
                serde_json::to_string(&phase)?,
                serde_json::to_string(&artifact)?,
                now_unix(),
                workspace,
                operation_id
            ],
        )?;
        self.operation_unchecked(workspace, operation_id)
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
                "SELECT instance_id, experiment_id, lease_id, principal_id, sequence, kind_json,
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
                    lease_id,
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
                        lease_id,
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

    fn lease_unchecked(&self, workspace: &str, id: &str) -> Result<ExperimentLease, StoreError> {
        self.lock()?
            .query_row(
                "SELECT experiment_id, instance_id, principal_id, phase_json, acquired_at,
                        expires_at, max_actions, released_at
                 FROM experiment_leases WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, u32>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    experiment_id,
                    instance_id,
                    principal_id,
                    phase,
                    acquired_at_unix,
                    expires_at_unix,
                    max_actions,
                    released_at_unix,
                )| {
                    Ok::<ExperimentLease, StoreError>(ExperimentLease {
                        id: id.to_owned(),
                        workspace_id: workspace.to_owned(),
                        experiment_id,
                        instance_id,
                        principal_id,
                        phase: serde_json::from_str(&phase)?,
                        acquired_at_unix,
                        expires_at_unix,
                        max_actions,
                        released_at_unix,
                    })
                },
            )
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "experiment lease",
                id: id.to_owned(),
            })
    }

    fn wallet_quote_unchecked(&self, workspace: &str, id: &str) -> Result<WalletQuote, StoreError> {
        self.lock()?
            .query_row(
                "SELECT instance_id, experiment_id, lease_id, principal_id, wallet_id,
                        mint_id, direction_json, amount_sat, phase_json, created_at,
                        updated_at, expires_at, settled_at, operation_id, terminal_code
                 FROM wallet_quotes WHERE workspace_id = ?1 AND id = ?2",
                params![workspace, id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, Option<i64>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    instance_id,
                    experiment_id,
                    lease_id,
                    principal_id,
                    wallet_id,
                    mint_id,
                    direction,
                    amount_sat,
                    phase,
                    created_at_unix,
                    updated_at_unix,
                    expires_at_unix,
                    settled_at_unix,
                    operation_id,
                    terminal_code,
                )| {
                    Ok::<WalletQuote, StoreError>(WalletQuote {
                        id: id.to_owned(),
                        workspace_id: workspace.to_owned(),
                        instance_id,
                        experiment_id,
                        lease_id,
                        principal_id,
                        wallet_id,
                        mint_id,
                        direction: serde_json::from_str(&direction)?,
                        amount_sat: u64::try_from(amount_sat)
                            .map_err(|_| StoreError::InvalidStoredVersion(amount_sat))?,
                        phase: serde_json::from_str(&phase)?,
                        created_at_unix,
                        updated_at_unix,
                        expires_at_unix,
                        settled_at_unix,
                        operation_id,
                        terminal_code,
                    })
                },
            )
            .transpose()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "wallet quote",
                id: id.to_owned(),
            })
    }

    fn refresh_lease(
        &self,
        workspace: &str,
        lease_id: &str,
    ) -> Result<ExperimentLease, StoreError> {
        let now = now_unix();
        self.lock()?.execute(
            "UPDATE experiment_leases SET phase_json = ?1
             WHERE workspace_id = ?2 AND id = ?3 AND phase_json = '\"active\"' AND expires_at <= ?4",
            params![serde_json::to_string(&LeasePhase::Expired)?, workspace, lease_id, now],
        )?;
        self.lease_unchecked(workspace, lease_id)
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

fn expire_leases(
    transaction: &Transaction<'_>,
    workspace: &str,
    now: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE experiment_leases SET phase_json = ?1
         WHERE workspace_id = ?2 AND phase_json = '\"active\"' AND expires_at <= ?3",
        params![serde_json::to_string(&LeasePhase::Expired)?, workspace, now],
    )?;
    Ok(())
}

fn validate_lease_request(
    lease_id: &str,
    duration_seconds: u32,
    max_actions: u32,
) -> Result<(), StoreError> {
    if !is_slug(lease_id) {
        return Err(StoreError::Validation(
            "lease id must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
        ));
    }
    if !(1..=86_400).contains(&duration_seconds) || !(1..=1_000).contains(&max_actions) {
        return Err(StoreError::Validation(
            "lease duration_seconds must be 1..=86400 and max_actions must be 1..=1000".into(),
        ));
    }
    Ok(())
}

fn validate_wallet_quote_request(
    quote_id: &str,
    wallet_id: &str,
    mint_id: &str,
    amount_sat: u64,
    ttl_seconds: u32,
) -> Result<(), StoreError> {
    if !is_slug(quote_id) || !is_slug(wallet_id) || !is_slug(mint_id) {
        return Err(StoreError::Validation(
            "quote, wallet, and mint ids must be lowercase kebab-case identifiers of 1..=63 bytes"
                .into(),
        ));
    }
    if !(1..=500_000).contains(&amount_sat) {
        return Err(StoreError::Validation(
            "wallet quote amount_sat must be 1..=500000".into(),
        ));
    }
    if !(30..=86_400).contains(&ttl_seconds) {
        return Err(StoreError::Validation(
            "wallet quote ttl_seconds must be 30..=86400".into(),
        ));
    }
    Ok(())
}

fn validate_quote_transition_metadata(
    phase: WalletQuotePhase,
    operation_id: Option<&str>,
    terminal_code: Option<&str>,
) -> Result<(), StoreError> {
    if operation_id.is_some_and(|id| !is_slug(id)) {
        return Err(StoreError::Validation(
            "wallet quote operation id must be a lowercase kebab-case identifier of 1..=63 bytes"
                .into(),
        ));
    }
    if terminal_code.is_some_and(|code| !is_stable_code(code)) {
        return Err(StoreError::Validation(
            "wallet quote terminal code must contain 1..=63 lowercase letters, digits, hyphens, or underscores"
                .into(),
        ));
    }
    if matches!(
        phase,
        WalletQuotePhase::Failed | WalletQuotePhase::Inconclusive
    ) && terminal_code.is_none()
    {
        return Err(StoreError::Validation(
            "failed or inconclusive wallet quotes require a stable terminal code".into(),
        ));
    }
    if !matches!(
        phase,
        WalletQuotePhase::Failed
            | WalletQuotePhase::Inconclusive
            | WalletQuotePhase::Expired
            | WalletQuotePhase::Cancelled
    ) && terminal_code.is_some()
    {
        return Err(StoreError::Validation(
            "non-failure wallet quote phases cannot carry a terminal code".into(),
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

fn is_stable_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}
