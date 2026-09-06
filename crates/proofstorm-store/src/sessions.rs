//! Passive activity attribution. Session state never gates operation admission.
use super::{
    Arc, Capability, Connection, JsonSchema, OptionalExtension, Serialize, Store, StoreError,
    TransactionBehavior, now_unix, params, validate_session_request,
};
use proofstorm_core::Session;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SessionPage {
    pub sessions: Vec<Session>,
    pub next_cursor: Option<String>,
    pub observed_at_unix: i64,
}

fn read(db: &Connection, workspace: &str, id: &str) -> Result<Session, StoreError> {
    db.query_row("SELECT experiment_id,instance_id,principal_id,phase_json,started_at,last_activity_at,finished_at FROM sessions WHERE workspace_id=?1 AND id=?2",
        params![workspace,id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<i64>>(6)?)))
        .optional()?.map(|(experiment_id,instance_id,principal_id,phase,started_at_unix,last_activity_at_unix,finished_at_unix)| Ok::<Session,StoreError>(Session {
            id:id.into(),workspace_id:workspace.into(),experiment_id,instance_id,principal_id,
            phase:serde_json::from_str(&phase)?,started_at_unix,last_activity_at_unix,finished_at_unix,
        })).transpose()?.ok_or_else(|| StoreError::NotFound {resource:"session",id:id.into()})
}

impl Store {
    fn remember_session(&self, workspace: &str, id: &str) -> Result<(), StoreError> {
        self.context_sessions
            .lock()
            .map_err(|_| StoreError::Poisoned)?
            .insert((workspace.into(), id.into()));
        Ok(())
    }
    pub fn session(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<Session, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        self.session_unchecked(workspace, id)
    }
    pub(super) fn session_unchecked(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<Session, StoreError> {
        read(&*self.lock()?, workspace, id)
    }
    /// Idempotently open a tracking interval. Concurrent sessions are unrestricted.
    pub fn start_session(
        &self,
        workspace: &str,
        principal: &str,
        experiment: &str,
        id: &str,
        key: &str,
    ) -> Result<Session, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let request = serde_json::json!({"experimentId":experiment,"sessionId":id});
        if let Some(previous) = self.idempotent_response::<Session, _>(
            workspace,
            principal,
            key,
            "session.start",
            &request,
        )? {
            return self.session_unchecked(workspace, &previous.id);
        }
        let session = self.track_session(workspace, principal, experiment, id)?;
        self.record_idempotency(
            workspace,
            principal,
            key,
            "session.start",
            &request,
            &session,
        )?;
        Ok(session)
    }
    /// Resolve an optional caller ID, automatically continuing in a fresh interval after finish.
    pub fn track_session(
        &self,
        workspace: &str,
        principal: &str,
        experiment: &str,
        requested: &str,
    ) -> Result<Session, StoreError> {
        let run = self.experiment_unchecked(workspace, experiment)?;
        let context = proofstorm_core::digest_json(&(
            workspace,
            principal,
            experiment,
            self.context_id.as_str(),
        ));
        let mut id = if requested.is_empty() {
            format!("session-{}", &context[7..39])
        } else {
            requested.into()
        };
        validate_session_request(&id)?;
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut generation = 0u64;
        loop {
            match read(&tx, workspace, &id) {
                Ok(existing) => {
                    if existing.principal_id != principal || existing.experiment_id != experiment {
                        // A caller-supplied ID is attribution, never permission to impersonate.
                        generation += 1;
                        let digest =
                            proofstorm_core::digest_json(&(requested, &context, generation));
                        id = format!("session-{}", &digest[7..39]);
                        continue;
                    }
                    if existing.finished_at_unix.is_none() {
                        tx.commit()?;
                        self.remember_session(workspace, &existing.id)?;
                        return Ok(existing);
                    }
                    generation += 1;
                    let digest = proofstorm_core::digest_json(&(requested, &context, generation));
                    id = format!("session-{}", &digest[7..39]);
                }
                Err(StoreError::NotFound { .. }) => break,
                Err(error) => return Err(error),
            }
        }
        let now = now_unix();
        tx.execute("INSERT INTO sessions(workspace_id,id,experiment_id,instance_id,principal_id,phase_json,started_at,last_activity_at) VALUES(?1,?2,?3,?4,?5,'\"active\"',?6,?6)",params![workspace,id,experiment,run.instance_id,principal,now])?;
        let session = read(&tx, workspace, &id)?;
        tx.commit()?;
        self.remember_session(workspace, &session.id)?;
        Ok(session)
    }
    /// Finish tracking only. Accepted work, lab availability and access grants are unaffected.
    pub fn finish_session(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        _key: &str,
    ) -> Result<Session, StoreError> {
        let session = self.session_for_finish(workspace, principal, id)?;
        self.lock()?.execute("UPDATE sessions SET phase_json='\"finished\"',finished_at=COALESCE(finished_at,?1) WHERE workspace_id=?2 AND id=?3",params![now_unix(),workspace,session.id])?;
        self.session_unchecked(workspace, id)
    }
    pub fn session_for_finish(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<Session, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let session = self.session_unchecked(workspace, id)?;
        if session.principal_id != principal {
            return Err(StoreError::Validation(
                "only the recorded actor can finish their session".into(),
            ));
        }
        Ok(session)
    }
    pub fn finish_lab_sessions(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::LabClose)?;
        self.lock()?.execute("UPDATE sessions SET phase_json='\"finished\"',finished_at=COALESCE(finished_at,?1) WHERE workspace_id=?2 AND instance_id=?3",params![now_unix(),workspace,instance])?;
        Ok(())
    }
    /// Stable, bounded pagination. No read mutates session activity or infers liveness.
    pub fn sessions(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        cursor: &str,
        limit: u32,
    ) -> Result<SessionPage, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let limit = limit.clamp(1, 100);
        let db = self.lock()?;
        let ids=db.prepare("SELECT id FROM sessions WHERE workspace_id=?1 AND instance_id=?2 AND id>?3 ORDER BY id LIMIT ?4")?
            .query_map(params![workspace,instance,cursor,limit+1],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let next_cursor = if ids.len() > limit as usize {
            ids.get(limit as usize - 1).cloned()
        } else {
            None
        };
        let sessions = ids
            .iter()
            .take(limit as usize)
            .map(|id| read(&db, workspace, id))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionPage {
            sessions,
            next_cursor,
            observed_at_unix: now_unix(),
        })
    }
    /// List temporal overlaps with one interval, including unfinished intervals. This is advisory.
    pub fn overlapping_sessions(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
        cursor: &str,
        limit: u32,
    ) -> Result<SessionPage, StoreError> {
        let session = self.session(workspace, principal, id)?;
        let limit = limit.clamp(1, 100);
        let now = now_unix();
        let db = self.lock()?;
        let ids=db.prepare("SELECT id FROM sessions WHERE workspace_id=?1 AND instance_id=?2 AND id!=?3 AND id>?4 AND started_at<=?5 AND COALESCE(finished_at,?6)>=?7 ORDER BY id LIMIT ?8")?
            .query_map(params![workspace,session.instance_id,id,cursor,session.finished_at_unix.unwrap_or(now),now,session.started_at_unix,limit+1],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let next_cursor = if ids.len() > limit as usize {
            ids.get(limit as usize - 1).cloned()
        } else {
            None
        };
        let sessions = ids
            .iter()
            .take(limit as usize)
            .map(|id| read(&db, workspace, id))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionPage {
            sessions,
            next_cursor,
            observed_at_unix: now,
        })
    }
}

// A clean CLI/MCP disconnect ends its tracking intervals. Abrupt process death
// leaves them unfinished; reads report last activity without guessing liveness.
impl Drop for Store {
    fn drop(&mut self) {
        if Arc::strong_count(&self.connection) != 1 {
            return;
        }
        let (Ok(db), Ok(sessions)) = (self.connection.lock(), self.context_sessions.lock()) else {
            return;
        };
        for (workspace, id) in sessions.iter() {
            let _=db.execute("UPDATE sessions SET phase_json='\"finished\"',finished_at=COALESCE(finished_at,?1) WHERE workspace_id=?2 AND id=?3",params![now_unix(),workspace,id]);
        }
    }
}
