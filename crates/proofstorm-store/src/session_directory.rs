//! Typed filtering before candidate pagination; reads never refresh session activity.
use crate::{Capability, Store, StoreError, now_unix};
use proofstorm_core::Session;
use rusqlite::named_params;
use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct SessionFilters {
    pub id: Option<String>,
    pub principal_id: Option<String>,
    pub run_id: Option<String>,
    pub phase: Option<proofstorm_core::SessionPhase>,
    pub started_after_unix: Option<i64>,
    pub started_before_unix: Option<i64>,
    pub last_activity_after_unix: Option<i64>,
    pub last_activity_before_unix: Option<i64>,
    pub overlaps_with: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionWindow<'a> {
    pub after_id: &'a str,
    pub limit: u32,
    /// Fixed cutoff for unfinished overlap intervals throughout a continuation.
    pub observed_at: i64,
}

impl Store {
    /// Legal session updates only advance last activity or finish an interval.
    /// Counts and sums detect those changes without decoding every session.
    /// The instance key also fences deletion and same-ID replacement.
    pub fn session_observation_digest(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
    ) -> Result<String, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let identity = self.instance_unchecked(workspace, instance)?;
        let db = self.lock()?;
        let counts = db.query_row("SELECT COUNT(*),COALESCE(MAX(rowid),0),COALESCE(SUM(last_activity_at),0),COUNT(finished_at),COALESCE(SUM(finished_at),0) FROM sessions WHERE workspace_id=?1 AND instance_id=?2",
            rusqlite::params![workspace, instance], |row| Ok([row.get::<_,i64>(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?]))?;
        Ok(proofstorm_core::digest_json(&(
            workspace,
            instance,
            identity.instance_key,
            counts,
        )))
    }

    /// Exact filters run in SQL before LIMIT. Text/regex callers scan these
    /// bounded candidates before constructing their own matching result page.
    pub fn session_candidates(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        filters: &SessionFilters,
        window: SessionWindow<'_>,
    ) -> Result<Vec<Session>, StoreError> {
        let SessionWindow {
            after_id: boundary,
            limit,
            observed_at,
        } = window;
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        if !(1..=201).contains(&limit) {
            return Err(StoreError::Validation(
                "session candidate limit must be 1..=201".into(),
            ));
        }
        for (after, before) in [
            (filters.started_after_unix, filters.started_before_unix),
            (
                filters.last_activity_after_unix,
                filters.last_activity_before_unix,
            ),
        ] {
            if after.zip(before).is_some_and(|(a, b)| a >= b) {
                return Err(StoreError::Validation(
                    "session time range must have after < before".into(),
                ));
            }
        }
        let overlap = filters
            .overlaps_with
            .as_ref()
            .map(|id| self.session(workspace, principal, id))
            .transpose()?;
        if overlap.as_ref().is_some_and(|s| s.instance_id != instance) {
            return Err(StoreError::Validation(
                "overlap session belongs to another cell".into(),
            ));
        }
        let phase = filters
            .phase
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let db = self.lock()?;
        let mut statement = db.prepare("SELECT id,experiment_id,instance_id,principal_id,phase_json,started_at,last_activity_at,finished_at FROM sessions
            WHERE workspace_id=:workspace AND instance_id=:instance AND id>:boundary
            AND (:id IS NULL OR id=:id) AND (:actor IS NULL OR principal_id=:actor)
            AND (:run IS NULL OR experiment_id=:run) AND (:phase IS NULL OR phase_json=:phase)
            AND (:started_after IS NULL OR started_at>=:started_after) AND (:started_before IS NULL OR started_at<:started_before)
            AND (:activity_after IS NULL OR last_activity_at>=:activity_after) AND (:activity_before IS NULL OR last_activity_at<:activity_before)
            AND (:overlap IS NULL OR (id!=:overlap AND started_at<=:overlap_end AND COALESCE(finished_at,:observed)>=:overlap_start))
            ORDER BY id LIMIT :limit")?;
        let rows = statement.query_map(named_params!{
            ":workspace":workspace, ":instance":instance, ":boundary":boundary, ":id":filters.id,
            ":actor":filters.principal_id, ":run":filters.run_id, ":phase":phase,
            ":started_after":filters.started_after_unix, ":started_before":filters.started_before_unix,
            ":activity_after":filters.last_activity_after_unix, ":activity_before":filters.last_activity_before_unix,
            ":overlap":filters.overlaps_with, ":overlap_end":overlap.as_ref().map(|s|s.finished_at_unix.unwrap_or(observed_at)),
            ":overlap_start":overlap.as_ref().map(|s|s.started_at_unix), ":observed":observed_at, ":limit":limit,
        }, |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<i64>>(7)?)))?;
        rows.map(|row| {
            let (
                id,
                experiment_id,
                instance_id,
                principal_id,
                phase,
                started_at_unix,
                last_activity_at_unix,
                finished_at_unix,
            ) = row?;
            Ok(Session {
                id,
                experiment_id,
                instance_id,
                principal_id,
                phase: serde_json::from_str(&phase)?,
                workspace_id: workspace.into(),
                started_at_unix,
                last_activity_at_unix,
                finished_at_unix,
            })
        })
        .collect()
    }

    #[must_use]
    pub fn session_observed_at() -> i64 {
        now_unix()
    }
}
