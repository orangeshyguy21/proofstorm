//! Durable preparation shared by application edits and MCP previews, without runtime access.
use crate::Error;
use proofstorm_core::{CellSpec, CellUpdatePlan, CellUpdateTarget, PublishedRevision};
use proofstorm_store::Store;

#[derive(Debug)]
pub struct PreparedPlan {
    pub revision: PublishedRevision,
    pub update: Option<CellUpdatePlan>,
}

/// Publish a candidate and optionally calculate its fenced update.
///
/// Callers own request validation, saved-plan lookup and receipt persistence.
/// Keep their draft IDs and stage keys stable so interrupted preparation resumes
/// the original publication. Each store stage checks its existing authorization;
/// this sequence does not admit a runtime change or form a single transaction.
pub fn prepare_plan(
    store: &Store,
    workspace: &str,
    principal: &str,
    draft_id: &str,
    cell: &CellSpec,
    target: Option<CellUpdateTarget>,
) -> Result<PreparedPlan, Error> {
    store.create_draft(
        workspace,
        principal,
        draft_id,
        cell,
        &format!("{draft_id}:draft"),
    )?;
    let revision = store.publish(
        workspace,
        principal,
        draft_id,
        1,
        &format!("{draft_id}:publish"),
    )?;
    let update = target
        .map(|target| store.plan_update(workspace, principal, target, &revision))
        .transpose()?;
    Ok(PreparedPlan { revision, update })
}
