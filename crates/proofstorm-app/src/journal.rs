//! Explicit synchronization of bounded runtime receipts into durable history.
use crate::{Error, Runtime};
use proofstorm_core::{CellOperation, OperationPhase};
use proofstorm_store::Store;

pub fn record(
    store: &Store,
    workspace: &str,
    operation: &CellOperation,
    phase: OperationPhase,
    artifact: serde_json::Value,
) -> Result<CellOperation, Error> {
    store
        .record_operation_result(workspace, &operation.id, phase, artifact)
        .map_err(Error::from)
}
/// Synchronize a run without requiring callers to poll every action handle.
pub async fn reconcile(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    run: &str,
) -> Result<Vec<CellOperation>, Error> {
    let mut after = 0;
    let mut pending = Vec::new();
    loop {
        let page = store.actions(workspace, principal, run, after, 100)?;
        for operation in &page {
            after = operation.sequence;
            if matches!(
                operation.phase,
                OperationPhase::Pending | OperationPhase::Running
            ) {
                if let Some((phase, artifact)) = runtime.action_status(operation).await? {
                    record(store, workspace, operation, phase, artifact)?;
                } else {
                    pending.push(operation.clone());
                }
            }
        }
        if page.len() < 100 {
            break;
        }
    }
    Ok(pending)
}
