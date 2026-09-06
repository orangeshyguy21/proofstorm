//! Read-only discovery over retained instances, including unnamed and historical labs.
use super::{Capability, LabHandle, LabOperation, Store, StoreError, params};
use rusqlite::OptionalExtension;

pub struct EnvironmentEntry {
    pub id: String,
    pub handle: Option<LabHandle>,
}

impl Store {
    pub fn environment_entries(
        &self,
        workspace: &str,
        principal: &str,
        cursor: &str,
        limit: u32,
    ) -> Result<(Vec<EnvironmentEntry>, Option<String>), StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        validate_page(cursor, limit)?;
        let rows = {
            let db = self.lock()?;
            db.prepare("SELECT ids.id,h.name FROM (SELECT id FROM instances WHERE workspace_id=?1 UNION SELECT instance_id AS id FROM lab_handles WHERE workspace_id=?1) ids LEFT JOIN lab_handles h ON h.workspace_id=?1 AND h.instance_id=ids.id WHERE ids.id>?2 ORDER BY ids.id LIMIT ?3")?
                .query_map(params![workspace,cursor,limit+1], |r| Ok((r.get::<_,String>(0)?, r.get::<_,Option<String>>(1)?)))?
                .collect::<Result<Vec<_>,_>>()?
        };
        let next = (rows.len() > limit as usize).then(|| rows[limit as usize - 1].0.clone());
        let entries = rows
            .into_iter()
            .take(limit as usize)
            .map(|(id, name)| {
                let handle = name
                    .map(|n| self.lab_handle(workspace, principal, &n))
                    .transpose()?;
                // A concurrent close/up may replace the name; do not attach it to the old instance.
                Ok(EnvironmentEntry {
                    handle: handle.filter(|h| h.instance_id == id),
                    id,
                })
            })
            .collect::<Result<_, StoreError>>()?;
        Ok((entries, next))
    }

    pub fn environment_entry(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<EnvironmentEntry, StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        let name: Option<Option<String>> = {
            let db = self.lock()?;
            db.query_row("SELECT h.name FROM (SELECT id FROM instances WHERE workspace_id=?1 UNION SELECT instance_id AS id FROM lab_handles WHERE workspace_id=?1) ids LEFT JOIN lab_handles h ON h.workspace_id=?1 AND h.instance_id=ids.id WHERE ids.id=?2",params![workspace,id],|r|r.get(0)).optional()?
        };
        let name = name.ok_or_else(|| StoreError::NotFound {
            resource: "instance",
            id: id.into(),
        })?;
        Ok(EnvironmentEntry {
            id: id.into(),
            handle: name
                .map(|n| self.lab_handle(workspace, principal, &n))
                .transpose()?
                .filter(|h| h.instance_id == id),
        })
    }

    /// Newest-first activity across all runs. The cursor is the previous item's opaque ID.
    pub fn instance_activity(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        cursor: &str,
        limit: u32,
    ) -> Result<(Vec<LabOperation>, Option<String>), StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        validate_page(cursor, limit)?;
        let ids = {
            let db = self.lock()?;
            let boundary: Option<i64> = if cursor.is_empty() {
                None
            } else {
                Some(db.query_row("SELECT accepted_at FROM actions WHERE workspace_id=?1 AND instance_id=?2 AND id=?3",params![workspace,instance,cursor],|r|r.get(0)).optional()?.ok_or_else(||StoreError::Validation("activity cursor does not belong to this lab".into()))?)
            };
            db.prepare("SELECT id FROM actions WHERE workspace_id=?1 AND instance_id=?2 AND (?3 IS NULL OR accepted_at<?3 OR (accepted_at=?3 AND id<?4)) ORDER BY accepted_at DESC,id DESC LIMIT ?5")?
                .query_map(params![workspace,instance,boundary,cursor,limit+1],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?
        };
        let next = (ids.len() > limit as usize).then(|| ids[limit as usize - 1].clone());
        let operations = ids
            .into_iter()
            .take(limit as usize)
            .map(|id| self.operation_unchecked(workspace, &id))
            .collect::<Result<_, _>>()?;
        Ok((operations, next))
    }

    pub fn last_instance_activity(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<Option<i64>, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        Ok(self.lock()?.query_row("SELECT MAX(COALESCE(completed_at,started_at,accepted_at)) FROM actions WHERE workspace_id=?1 AND instance_id=?2",params![workspace,instance],|r|r.get(0))?)
    }

    pub fn session_overlap_count(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        observed_at: i64,
    ) -> Result<i64, StoreError> {
        let session = self.session(workspace, principal, id)?;
        Ok(self.lock()?.query_row("SELECT COUNT(*) FROM sessions WHERE workspace_id=?1 AND instance_id=?2 AND id!=?3 AND started_at<=?4 AND COALESCE(finished_at,?5)>=?6",params![workspace,session.instance_id,id,session.finished_at_unix.unwrap_or(observed_at),observed_at,session.started_at_unix],|r|r.get(0))?)
    }
}

fn validate_page(cursor: &str, limit: u32) -> Result<(), StoreError> {
    if !(1..=50).contains(&limit) || cursor.len() > 128 {
        return Err(StoreError::Validation(
            "page limit must be 1..=50 and cursor at most 128 bytes".into(),
        ));
    }
    Ok(())
}
