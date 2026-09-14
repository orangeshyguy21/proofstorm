//! Exact accounting assertions for typed driver artifacts, independent of the MCP menu.
use crate::{GateContext, McpClient, json as expect};
use anyhow::{Result, anyhow};
use proofstorm_core::{CellOperation, OperationKind, OperationPhase};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
type ErrorData = anyhow::Error;
fn coded_invalid_request(code: &str, message: impl Into<String>) -> ErrorData {
    anyhow!("{code}: {}", message.into())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConservationOracleRequest {
    pub instance_id: String,
    pub experiment_id: String,
    #[serde(default)]
    pub session_id: String,
    pub operation_id: String,
    pub wallet: String,
    pub mint: String,
    /// Earlier successful `wallet_balance` operation captured before treatment.
    pub baseline_operation_id: String,
    /// Successful `wallet_pay` operation after the baseline. A round trip mints
    /// external value first and is not a valid balance-invariance treatment.
    pub treatment_operation_id: String,
    pub idempotency_key: String,
}
fn conservation_observation(
    request: &ConservationOracleRequest,
    baseline: &CellOperation,
    treatment: &CellOperation,
    workspace: &str,
    principal: &str,
) -> Result<serde_json::Value, ErrorData> {
    let same_scope = |operation: &CellOperation| {
        operation.workspace_id == workspace
            && operation.principal_id == principal
            && operation.instance_id == request.instance_id
            && operation.experiment_id == request.experiment_id
    };
    if baseline.id != request.baseline_operation_id
        || baseline.kind != OperationKind::WalletBalance
        || baseline.phase != OperationPhase::Succeeded
        || !same_scope(baseline)
        || baseline
            .request
            .get("wallet")
            .and_then(serde_json::Value::as_str)
            != Some(request.wallet.as_str())
        || baseline
            .request
            .get("mint")
            .and_then(serde_json::Value::as_str)
            != Some(request.mint.as_str())
    {
        return Err(coded_invalid_request(
            "conservation_baseline_invalid",
            "baseline_operation_id must name an earlier successful wallet_balance for the same principal, instance, experiment, wallet, and mint",
        ));
    }
    if treatment.id != request.treatment_operation_id
        || treatment.kind != OperationKind::WalletPay
        || treatment.phase != OperationPhase::Succeeded
        || !same_scope(treatment)
        || treatment.sequence <= baseline.sequence
        || treatment
            .request
            .get("wallet")
            .and_then(serde_json::Value::as_str)
            != Some(request.wallet.as_str())
        || treatment
            .request
            .get("mint")
            .and_then(serde_json::Value::as_str)
            != Some(request.mint.as_str())
    {
        return Err(coded_invalid_request(
            "conservation_treatment_invalid",
            "treatment_operation_id must name a later successful wallet_pay for the same principal, instance, experiment, wallet, and mint; wallet_round_trip is not balance-invariant because it mints external value first",
        ));
    }
    let baseline_sat = baseline
        .artifact
        .as_ref()
        .and_then(|artifact| artifact.content.get("balance_sat"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_baseline_artifact_invalid",
                "the baseline wallet_balance artifact has no unsigned balance_sat",
            )
        })?;
    if baseline_sat > 100_000_000 {
        return Err(coded_invalid_request(
            "conservation_baseline_out_of_bounds",
            "the baseline balance_sat cannot exceed 100,000,000",
        ));
    }
    let treatment = conservation_treatment_evidence(request, treatment, baseline_sat)?;
    let delta_sat = treatment.expected_sat.abs_diff(treatment.actual_sat);
    Ok(serde_json::json!({
        "baseline_operation_id": request.baseline_operation_id,
        "treatment_operation_id": request.treatment_operation_id,
        "baseline_sat": baseline_sat,
        "melt_state": treatment.melt_state,
        "amount_sat": treatment.amount_sat,
        "fee_paid_sat": treatment.fee_paid_sat,
        "input_fee_sat": treatment.input_fee_sat,
        "input_proof_count": treatment.input_proof_count,
        "expected_sat": treatment.expected_sat,
        "actual_sat": treatment.actual_sat,
        "delta_sat": delta_sat,
        "conserved": delta_sat == 0,
    }))
}
struct ConservationTreatmentEvidence {
    actual_sat: u64,
    melt_state: String,
    amount_sat: u64,
    fee_paid_sat: u64,
    input_fee_sat: u64,
    input_proof_count: u64,
    expected_sat: u64,
}
fn conservation_input_evidence(
    treatment_content: &serde_json::Value,
    melt_state: &str,
) -> Result<(u64, u64), ErrorData> {
    let invalid =
        |message| coded_invalid_request("conservation_treatment_artifact_invalid", message);
    let input_fee_sat = treatment_content
        .get("input_fee_sat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            invalid("the wallet_pay treatment artifact has no exact unsigned input_fee_sat")
        })?;
    let input_proof_count = treatment_content
        .get("input_proof_count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            invalid("the wallet_pay treatment artifact has no exact unsigned input_proof_count")
        })?;
    if input_fee_sat > 100_000 || input_proof_count > 10_000 {
        return Err(invalid(
            "the observed input fee or proof count exceeds its evidence bound",
        ));
    }
    match melt_state {
        "PAID" if input_proof_count == 0 => Err(invalid(
            "a PAID melt must identify at least one spent input proof",
        )),
        "UNPAID" if input_fee_sat != 0 || input_proof_count != 0 => Err(invalid(
            "an UNPAID melt cannot report spent input proofs or an input fee",
        )),
        _ => Ok((input_fee_sat, input_proof_count)),
    }
}
fn conservation_treatment_evidence(
    request: &ConservationOracleRequest,
    treatment: &CellOperation,
    baseline_sat: u64,
) -> Result<ConservationTreatmentEvidence, ErrorData> {
    let treatment_content = treatment
        .artifact
        .as_ref()
        .map(|artifact| &artifact.content)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the wallet_pay treatment has no terminal artifact",
            )
        })?;
    let actual_sat = treatment_content
        .get("payer_balance_sat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the wallet_pay treatment artifact has no unsigned payer_balance_sat",
            )
        })?;
    let melt = treatment_content
        .get("quote_observations")
        .and_then(serde_json::Value::as_array)
        .and_then(|observations| {
            observations.iter().find(|observation| {
                observation.get("role").and_then(serde_json::Value::as_str) == Some("payment_melt")
                    && observation
                        .get("wallet_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(request.wallet.as_str())
                    && observation
                        .get("mint_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(request.mint.as_str())
            })
        })
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the wallet_pay treatment artifact has no matching payment_melt observation",
            )
        })?;
    let melt_state = melt
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the payment_melt observation has no state",
            )
        })?;
    let amount_sat = melt
        .get("amount_sat")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            coded_invalid_request(
                "conservation_treatment_artifact_invalid",
                "the payment_melt observation has no unsigned amount_sat",
            )
        })?;
    let melt_state = melt_state.to_ascii_uppercase();
    let (input_fee_sat, input_proof_count) =
        conservation_input_evidence(treatment_content, &melt_state)?;
    let (expected_sat, fee_paid_sat) = match melt_state.as_str() {
        "PAID" => {
            let fee = melt
                .get("fee_paid_sat")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    coded_invalid_request(
                        "conservation_treatment_artifact_invalid",
                        "a PAID payment_melt observation has no unsigned fee_paid_sat",
                    )
                })?;
            let debit = amount_sat
                .checked_add(fee)
                .and_then(|debit| debit.checked_add(input_fee_sat))
                .ok_or_else(|| {
                    coded_invalid_request(
                        "conservation_expected_balance_invalid",
                        "the observed payment debit overflows",
                    )
                })?;
            let expected = baseline_sat.checked_sub(debit).ok_or_else(|| {
                coded_invalid_request(
                    "conservation_expected_balance_invalid",
                    "the observed payment debit exceeds the baseline balance",
                )
            })?;
            (expected, fee)
        }
        "UNPAID" => (baseline_sat, 0),
        _ => {
            return Err(coded_invalid_request(
                "conservation_treatment_not_settled",
                "the wallet_pay melt must be PAID or UNPAID before conservation can be evaluated",
            ));
        }
    };
    Ok(ConservationTreatmentEvidence {
        actual_sat,
        melt_state,
        amount_sat,
        fee_paid_sat,
        input_fee_sat,
        input_proof_count,
        expected_sat,
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "backend fixtures share an owned request signature with the gate callback contract"
)]
pub fn check(context: &GateContext, client: &mut McpClient, request: Value) -> Result<Value> {
    let scope = crate::driver::scope(context, client, &request)?;
    let input = ConservationOracleRequest {
        instance_id: scope.instance.id.clone(),
        experiment_id: scope.run.clone(),
        session_id: String::new(),
        operation_id: expect::string(&request, "/request_id")?.into(),
        wallet: expect::string(&request, "/wallet")?.into(),
        mint: expect::string(&request, "/mint")?.into(),
        baseline_operation_id: expect::string(&request, "/baseline_operation_id")?.into(),
        treatment_operation_id: expect::string(&request, "/treatment_operation_id")?.into(),
        idempotency_key: expect::string(&request, "/request_id")?.into(),
    };
    let baseline = scope.store.operation(
        &scope.workspace,
        &scope.principal,
        &input.baseline_operation_id,
    )?;
    let treatment = scope.store.operation(
        &scope.workspace,
        &scope.principal,
        &input.treatment_operation_id,
    )?;
    let evidence = conservation_observation(
        &input,
        &baseline,
        &treatment,
        &scope.workspace,
        &scope.principal,
    )?;
    let operation = scope.store.create_operation(
        &scope.workspace,
        &scope.principal,
        &scope.instance.id,
        &scope.run,
        "",
        &input.operation_id,
        OperationKind::ConservationOracle,
        &request,
        &input.idempotency_key,
        proofstorm_core::Capability::OracleRun,
    )?;
    Ok(json!(scope.store.record_operation_result(
        &scope.workspace,
        &operation.id,
        OperationPhase::Succeeded,
        evidence
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{Capability, OperationArtifact};
    #[test]
    fn conservation_expectation_is_anchored_before_a_later_treatment() {
        let operation = |id: &str,
                         sequence: u64,
                         kind: OperationKind,
                         request: serde_json::Value,
                         artifact: serde_json::Value| CellOperation {
            revision_digest: String::new(),
            id: id.into(),
            workspace_id: "alpha".into(),
            instance_id: "instance".into(),
            experiment_id: "experiment".into(),
            session_id: "session".into(),
            principal_id: "designer".into(),
            sequence,
            kind,
            capability: Capability::WalletControl,
            resource_name: format!("resource-{id}"),
            request_digest: format!("sha256:{id}"),
            request,
            phase: OperationPhase::Succeeded,
            accepted_at_unix: 1,
            started_at_unix: Some(2),
            completed_at_unix: Some(3),
            artifact: Some(OperationArtifact {
                media_type: "application/json".into(),
                digest: format!("sha256:artifact-{id}"),
                byte_length: 1,
                content: artifact,
            }),
        };
        let baseline = operation(
            "balance-before",
            10,
            OperationKind::WalletBalance,
            serde_json::json!({"wallet":"wallet", "mint":"mint"}),
            serde_json::json!({"balance_sat": 19_998}),
        );
        let treatment = operation(
            "high-fee-pay",
            11,
            OperationKind::WalletPay,
            serde_json::json!({"wallet":"wallet", "mint":"mint"}),
            serde_json::json!({
                "payer_balance_sat": 19_998,
                "input_fee_sat": 0,
                "input_proof_count": 0,
                "quote_observations": [{
                    "role": "payment_melt",
                    "wallet_id": "wallet",
                    "mint_id": "mint",
                    "state": "UNPAID",
                    "amount_sat": 1_000,
                    "fee_paid_sat": 0
                }]
            }),
        );
        let request = ConservationOracleRequest {
            instance_id: "instance".into(),
            experiment_id: "experiment".into(),
            session_id: "session".into(),
            operation_id: "conservation".into(),
            wallet: "wallet".into(),
            mint: "mint".into(),
            baseline_operation_id: "balance-before".into(),
            treatment_operation_id: "high-fee-pay".into(),
            idempotency_key: "conservation".into(),
        };

        let evidence =
            conservation_observation(&request, &baseline, &treatment, "alpha", "designer")
                .expect("valid anchored conservation request");
        assert_eq!(evidence["baseline_sat"], 19_998);
        assert_eq!(evidence["expected_sat"], 19_998);
        assert_eq!(evidence["actual_sat"], 19_998);
        assert_eq!(evidence["conserved"], true);

        let mut treatment_before_baseline = treatment.clone();
        treatment_before_baseline.sequence = 9;
        let error = conservation_observation(
            &request,
            &baseline,
            &treatment_before_baseline,
            "alpha",
            "designer",
        )
        .expect_err("treatment must follow the balance baseline");
        assert!(error.to_string().contains("conservation_treatment_invalid"));

        let mut round_trip = treatment_before_baseline;
        round_trip.sequence = 11;
        round_trip.kind = OperationKind::WalletRoundTrip;
        let error = conservation_observation(&request, &baseline, &round_trip, "alpha", "designer")
            .expect_err("a value-minting round trip is not a balance-invariance treatment");
        assert!(error.to_string().contains("conservation_treatment_invalid"));

        let mut paid = treatment;
        paid.artifact.as_mut().expect("paid artifact").content = serde_json::json!({
            "payer_balance_sat": 18_996,
            "input_fee_sat": 1,
            "input_proof_count": 1,
            "quote_observations": [{
                "role": "payment_melt",
                "wallet_id": "wallet",
                "mint_id": "mint",
                "state": "PAID",
                "amount_sat": 1_000,
                "fee_paid_sat": 1
            }]
        });
        let evidence = conservation_observation(&request, &baseline, &paid, "alpha", "designer")
            .expect("paid debit evidence");
        assert_eq!(evidence["input_fee_sat"], 1);
        assert_eq!(evidence["input_proof_count"], 1);
        assert_eq!(evidence["expected_sat"], 18_996);
        assert_eq!(evidence["actual_sat"], 18_996);
        assert_eq!(evidence["conserved"], true);

        let mut incomplete = paid;
        incomplete
            .artifact
            .as_mut()
            .expect("incomplete artifact")
            .content
            .as_object_mut()
            .expect("artifact object")
            .remove("input_fee_sat");
        let error = conservation_observation(&request, &baseline, &incomplete, "alpha", "designer")
            .expect_err("missing exact input-fee evidence must fail closed");
        assert!(
            error
                .to_string()
                .contains("conservation_treatment_artifact_invalid")
        );
    }
}
