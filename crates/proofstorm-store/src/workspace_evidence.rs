//! Captures are committed atomically to an open run. Cell purge removes its local history.
use super::{
    Capability, Connection, OptionalExtension, Store, StoreError, TransactionBehavior, params,
};
use proofstorm_core::workspace::evidence::WorkspaceEvidence;

pub(super) fn initialize_schema(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS workspace_evidence(workspace_id TEXT NOT NULL,capture_id TEXT NOT NULL,run_id TEXT NOT NULL,principal_id TEXT NOT NULL,request_digest TEXT NOT NULL,evidence_json TEXT NOT NULL,PRIMARY KEY(workspace_id,capture_id),FOREIGN KEY(workspace_id,run_id) REFERENCES experiments(workspace_id,id) ON DELETE CASCADE);")?;
    Ok(())
}

impl Store {
    pub fn workspace_capture(
        &self,
        workspace: &str,
        principal: &str,
        capture_id: &str,
        request_digest: &str,
    ) -> Result<Option<WorkspaceEvidence>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        let found: Option<(String, String, String)> = self.lock()?.query_row(
            "SELECT principal_id,request_digest,evidence_json FROM workspace_evidence WHERE workspace_id=?1 AND capture_id=?2",
            params![workspace,capture_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
        ).optional()?;
        found
            .map(|(owner, digest, encoded)| {
                if owner != principal || digest != request_digest {
                    return Err(StoreError::IdempotencyConflict {
                        key: capture_id.into(),
                    });
                }
                Ok(serde_json::from_str(&encoded)?)
            })
            .transpose()
    }

    pub fn record_workspace_capture(
        &self,
        workspace: &str,
        principal: &str,
        request_digest: &str,
        evidence: &WorkspaceEvidence,
    ) -> Result<WorkspaceEvidence, StoreError> {
        self.authorize(workspace, principal, Capability::ComponentExecLive)?;
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        evidence
            .validate()
            .map_err(|error| StoreError::Validation(error.into()))?;
        let content = &evidence.content;
        if content.snapshot.capture_request_digest != request_digest {
            return Err(StoreError::Validation(
                "capture request digest mismatch".into(),
            ));
        }
        if content.principal_id != principal {
            return Err(StoreError::Validation("capture principal mismatch".into()));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(String, String, String)> = tx.query_row(
            "SELECT principal_id,request_digest,evidence_json FROM workspace_evidence WHERE workspace_id=?1 AND capture_id=?2",
            params![workspace,content.capture_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
        ).optional()?;
        if let Some((owner, digest, encoded)) = previous {
            if owner != principal || digest != request_digest {
                return Err(StoreError::IdempotencyConflict {
                    key: content.capture_id.clone(),
                });
            }
            return Ok(serde_json::from_str(&encoded)?);
        }
        // Closure and attachment use the same SQLite write lock. A late capture
        // is refused; it cannot silently change a previously exported bundle.
        let run_state: Option<(String,String,String,String)> = tx.query_row(
            "SELECT e.phase_json,e.instance_id,i.instance_key,i.revision_digest FROM experiments e JOIN instances i ON i.workspace_id=e.workspace_id AND i.id=e.instance_id WHERE e.workspace_id=?1 AND e.id=?2",
            params![workspace,content.run_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
        ).optional()?;
        if run_state
            != Some((
                "\"active\"".into(),
                content.instance_id.clone(),
                content.instance_key.clone(),
                content.revision_digest.clone(),
            ))
        {
            return Err(StoreError::Validation(
                "capture requires an open run in the same unchanged cell incarnation and revision"
                    .into(),
            ));
        }
        tx.execute(
            "INSERT INTO workspace_evidence VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                workspace,
                content.capture_id,
                content.run_id,
                principal,
                request_digest,
                serde_json::to_string(evidence)?
            ],
        )?;
        tx.commit()?;
        Ok(evidence.clone())
    }

    pub fn workspace_captures(
        &self,
        workspace: &str,
        principal: &str,
        run_id: &str,
    ) -> Result<Vec<WorkspaceEvidence>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.authorize(workspace, principal, Capability::ArtifactRead)?;
        self.experiment_unchecked(workspace, run_id)?;
        let connection = self.lock()?;
        let mut query = connection.prepare("SELECT evidence_json FROM workspace_evidence WHERE workspace_id=?1 AND run_id=?2 ORDER BY capture_id")?;
        let rows = query.query_map(params![workspace, run_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
}
