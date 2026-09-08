//! Explicit synchronization of bounded runtime receipts into durable history.
use crate::{Error, Runtime};
use proofstorm_core::{
    LabOperation, OperationKind, OperationPhase, WalletQuoteObservationInput,
    WalletQuoteObservationRole, wallet_quote_observations_from_artifact,
};
use proofstorm_store::Store;

pub fn record(
    store: &Store,
    workspace: &str,
    operation: &LabOperation,
    phase: OperationPhase,
    artifact: serde_json::Value,
) -> Result<LabOperation, Error> {
    let Ok(observations) = wallet_quote_observations_from_artifact(&artifact) else {
        return store
            .record_operation_result(
                workspace,
                &operation.id,
                OperationPhase::Failed,
                invalid_terminal_artifact(
                    operation,
                    phase,
                    "invalid_wallet_quote_observation",
                    "the runtime produced a wallet quote observation outside the strict contract",
                ),
            )
            .map_err(Error::from);
    };
    if validate_operation_quote_observations(operation, &observations).is_err() {
        return store
                .record_operation_result(
                    workspace,
                    &operation.id,
                    OperationPhase::Failed,
                    invalid_terminal_artifact(
                        operation,
                        phase,
                        "wallet_quote_observation_identity_mismatch",
                        "the runtime wallet quote observations do not match the admitted typed operation",
                    ),
                )
                .map_err(Error::from);
    }
    store
        .record_operation_result_with_quote_observations(
            workspace,
            &operation.id,
            phase,
            artifact,
            &observations,
        )
        .map_err(Error::from)
}
pub fn validate_operation_quote_observations(
    operation: &LabOperation,
    observations: &[WalletQuoteObservationInput],
) -> Result<(), Error> {
    let field = |name: &str| {
        operation
            .request
            .get(name)
            .and_then(serde_json::Value::as_str)
    };
    let valid = observations
        .iter()
        .all(|observation| match (operation.kind, observation.role) {
            (OperationKind::WalletInvoice, WalletQuoteObservationRole::InvoiceReceive) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
                    && operation
                        .request
                        .get("amount_sat")
                        .and_then(serde_json::Value::as_u64)
                        == Some(observation.amount_sat)
            }
            (OperationKind::WalletPay, WalletQuoteObservationRole::PaymentMelt) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
            }
            (OperationKind::WalletMeltQuoteRefresh, WalletQuoteObservationRole::PaymentMelt) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
                    && field("melt_quote_id") == Some(&observation.quote_id)
            }
            (OperationKind::WalletPay, WalletQuoteObservationRole::PaymentReceive) => {
                field("recipient_wallet") == Some(&observation.wallet_id)
                    && field("recipient_mint") == Some(&observation.mint_id)
                    && field("mint_quote_id") == Some(&observation.quote_id)
            }
            (OperationKind::WalletQuoteClaim, WalletQuoteObservationRole::ClaimReceive) => {
                field("wallet") == Some(&observation.wallet_id)
                    && field("mint") == Some(&observation.mint_id)
                    && field("mint_quote_id") == Some(&observation.quote_id)
            }
            _ => false,
        });
    if valid {
        Ok(())
    } else {
        Err(Error::problem(
            "wallet_quote_observation_identity_mismatch",
            "terminal wallet quote observations do not match the admitted typed operation",
        ))
    }
}
#[must_use]
pub fn invalid_terminal_artifact(
    operation: &LabOperation,
    reported_phase: OperationPhase,
    code: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "message": message,
        "operation_id": operation.id,
        "reported_phase": reported_phase,
        "recoverable": false,
    })
}
/// Synchronize a run without requiring callers to poll every action handle.
pub async fn reconcile(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    run: &str,
) -> Result<Vec<LabOperation>, Error> {
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
