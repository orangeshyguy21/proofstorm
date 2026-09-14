//! Immutable, actor-scoped previews and creation receipts. A preview never reserves a live cell.
use super::{
    Capability, Connection, OptionalExtension, Store, StoreError, TransactionBehavior, params,
};
use proofstorm_core::{CellSpec, CellUpdatePlan, digest_json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellPreview {
    pub id: String,
    pub request_digest: String,
    pub name: String,
    pub cell: CellSpec,
    pub revision_digest: String,
    pub lock_digest: String,
    pub update: Option<CellUpdatePlan>,
}

pub(super) fn initialize_schema(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS cell_previews(workspace_id TEXT NOT NULL,principal_id TEXT NOT NULL,id TEXT NOT NULL,preview_json TEXT NOT NULL,PRIMARY KEY(workspace_id,principal_id,id));
        CREATE TABLE IF NOT EXISTS preview_admissions(workspace_id TEXT NOT NULL,principal_id TEXT NOT NULL,plan_id TEXT NOT NULL,instance_id TEXT NOT NULL,PRIMARY KEY(workspace_id,principal_id,plan_id));
        CREATE TABLE IF NOT EXISTS cell_submission_requests(workspace_id TEXT NOT NULL,principal_id TEXT NOT NULL,request_id TEXT NOT NULL,preview_id TEXT NOT NULL,preview_digest TEXT NOT NULL,PRIMARY KEY(workspace_id,principal_id,request_id));")?;
    Ok(())
}

impl Store {
    /// Bind an apply request before admission, including when the caller supplies a saved plan.
    /// Keep this receipt after teardown so a retry ID cannot be reused for different work.
    pub fn bind_cell_submission(
        &self,
        workspace: &str,
        principal: &str,
        request_id: &str,
        preview: &CellPreview,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::CellCreate)?;
        let digest = digest_json(preview);
        let db = self.lock()?;
        db.execute(
            "INSERT OR IGNORE INTO cell_submission_requests VALUES(?1,?2,?3,?4,?5)",
            params![workspace, principal, request_id, preview.id, digest],
        )?;
        let stored: (String, String) = db.query_row(
            "SELECT preview_id,preview_digest FROM cell_submission_requests WHERE workspace_id=?1 AND principal_id=?2 AND request_id=?3",
            params![workspace, principal, request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if stored != (preview.id.clone(), digest) {
            return Err(StoreError::IdempotencyConflict {
                key: request_id.into(),
            });
        }
        Ok(())
    }

    pub fn cell_preview(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<Option<CellPreview>, StoreError> {
        self.authorize(workspace, principal, Capability::CellRead)?;
        let encoded: Option<String> = self.lock()?.query_row("SELECT preview_json FROM cell_previews WHERE workspace_id=?1 AND principal_id=?2 AND id=?3", params![workspace,principal,id], |row| row.get(0)).optional()?;
        encoded
            .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub fn save_cell_preview(
        &self,
        workspace: &str,
        principal: &str,
        preview: &CellPreview,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::CellCreate)?;
        let encoded = serde_json::to_string(preview)?;
        let db = self.lock()?;
        db.execute(
            "INSERT OR IGNORE INTO cell_previews VALUES(?1,?2,?3,?4)",
            params![workspace, principal, preview.id, encoded],
        )?;
        let stored: String = db.query_row("SELECT preview_json FROM cell_previews WHERE workspace_id=?1 AND principal_id=?2 AND id=?3",params![workspace,principal,preview.id],|row|row.get(0))?;
        if stored != encoded {
            return Err(StoreError::IdempotencyConflict {
                key: preview.id.clone(),
            });
        }
        Ok(())
    }

    /// Atomically bind an absent name to exactly one preview. Keep the admission tombstone
    /// after teardown so an old creation request cannot create a new incarnation.
    pub fn reserve_preview(
        &self,
        workspace: &str,
        principal: &str,
        preview: &CellPreview,
    ) -> Result<super::CellHandle, StoreError> {
        self.authorize(workspace, principal, Capability::CellCreate)?;
        self.authorize(workspace, principal, Capability::CellStatus)?;
        if !super::is_slug(&preview.name) || preview.update.is_some() {
            return Err(StoreError::Validation(
                "creation requires a new lowercase kebab-case cell name".into(),
            ));
        }
        if self
            .cell_preview(workspace, principal, &preview.id)?
            .as_ref()
            != Some(preview)
        {
            return Err(StoreError::Validation(
                "preview does not match its immutable record".into(),
            ));
        }
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = tx.query_row("SELECT instance_id FROM preview_admissions WHERE workspace_id=?1 AND principal_id=?2 AND plan_id=?3",params![workspace,principal,preview.id],|row|row.get(0)).optional()?;
        let current = super::cells::read(&tx, workspace, &preview.name)?;
        if let Some(previous) = previous {
            return current.filter(|cell| cell.instance_id==previous && cell.phase==super::CellHandlePhase::Open).ok_or_else(||StoreError::CellUpdate { code:"stale_incarnation",message:"This creation was already admitted and its cell was removed or replaced; use a new request_id".into() });
        }
        let exact_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM instances WHERE workspace_id=?1 AND id=?2)",
            params![workspace, preview.name],
            |row| row.get(0),
        )?;
        if current.is_some() || exact_exists {
            return Err(StoreError::CellUpdate {code:"cell_update_conflict",message:"The preview expected an absent name; inspect the current cell and prepare a fenced update".into()});
        }
        let nonce: String = tx.query_row("SELECT hex(randomblob(16))", [], |row| row.get(0))?;
        let identity = digest_json(&(workspace, &preview.name, nonce));
        let id = format!("cell-{}", &identity[7..31]);
        let config_digest = digest_json(&preview.cell);
        tx.execute("INSERT INTO cell_handles(workspace_id,name,generation,owner,config_digest,phase,instance_id) VALUES(?1,?2,1,?3,?4,'\"open\"',?5)",params![workspace,preview.name,principal,config_digest,id])?;
        tx.execute(
            "INSERT INTO preview_admissions VALUES(?1,?2,?3,?4)",
            params![workspace, principal, preview.id, id],
        )?;
        let handle = super::cells::read(&tx, workspace, &preview.name)?
            .ok_or_else(|| StoreError::Validation("inserted cell handle is missing".into()))?;
        tx.commit()?;
        Ok(handle)
    }
}
