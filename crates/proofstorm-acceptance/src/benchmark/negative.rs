//! O5: an unpaid invoice alone proves nothing. Require a fresh failed backend
//! attempt correlated with both Cashu quotes and the isolated recipient.
use super::task::Task;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

fn empty(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
}
fn isolated(snapshot: &Value) -> bool {
    empty(&snapshot["recipient_channels"]["channels"])
        && empty(&snapshot["recipient_closed"]["channels"])
        && [
            "pending_open_channels",
            "pending_closing_channels",
            "pending_force_closing_channels",
            "waiting_close_channels",
        ]
        .iter()
        .all(|key| empty(&snapshot["recipient_pending"][key]))
}
fn sat(value: &Value, expected: u64) -> bool {
    value.as_str().and_then(|s| s.parse::<u64>().ok()) == Some(expected)
}
fn one<'a>(value: &'a Value, rows: &str) -> Option<&'a Value> {
    let values = value[rows].as_array()?;
    // A bounded one-flow fixture must not hide further rows behind pagination.
    if values.len() != 1
        || value["last_index_offset"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n > 100)
    {
        return None;
    }
    values.first()
}

pub(super) fn assertions(task: &Task, funded: &Value, evaluated: &Value, assertions: &mut Value) {
    let hash = &evaluated["claims"]["payment_hash"];
    let recipient = &evaluated["recipient"];
    let identity = hash.as_str().is_some_and(|hash| hash.len() == 64)
        && recipient["r_hash"] == *hash
        && sat(&recipient["value"], task.amounts.melt_sat)
        && recipient["payment_request"].as_str().is_some_and(|v| {
            !v.is_empty()
                && evaluated["wallet_melt"]["invoice_sha256"]
                    == format!("{:x}", Sha256::digest(v.to_ascii_lowercase()))
        })
        && evaluated["mint_melt"]["quote"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
        && evaluated["mint_melt"]["quote"] == evaluated["claims"]["melt_quote_id"]
        && evaluated["wallet_melt"]["quote_id"] == evaluated["claims"]["melt_quote_id"]
        && evaluated["mint_melt"]["amount"] == task.amounts.melt_sat
        && evaluated["wallet_melt"]["amount_sat"] == task.amounts.melt_sat;
    let attempt = one(&evaluated["backend_payments"], "payments");
    let attempted = identity
        && empty(&funded["backend_payments"]["payments"])
        && attempt.is_some_and(|payment| {
            payment["payment_hash"] == *hash
                && payment["payment_request"] == recipient["payment_request"]
                && sat(&payment["value_sat"], task.amounts.melt_sat)
                && payment["status"] == "FAILED"
        });
    let no_route = attempted
        && isolated(funded)
        && isolated(evaluated)
        && attempt.is_some_and(|payment| payment["failure_reason"] == "FAILURE_REASON_NO_ROUTE");
    let unpaid = identity
        && recipient["settled"] == false
        && recipient["state"] == "OPEN"
        && sat(&recipient["amt_paid_sat"], 0)
        && empty(&funded["recipient_invoices"]["invoices"])
        && one(&evaluated["recipient_invoices"], "invoices").is_some_and(|invoice| {
            invoice["r_hash"] == *hash
                && invoice["settled"] == false
                && invoice["state"] == "OPEN"
                && sat(&invoice["amt_paid_sat"], 0)
        })
        && evaluated["mint_melt"]["state"] == "UNPAID"
        && evaluated["wallet_melt"]["state"] == "UNPAID";
    let funding = one(&evaluated["payer_payments"], "payments").is_some_and(|payment| {
        payment["status"] == "SUCCEEDED"
            && sat(&payment["value_sat"], task.amounts.mint_sat)
            && funded["mint_quote"]["request"].is_string()
            && payment["payment_request"] == funded["mint_quote"]["request"]
    });
    let accounting = assertions["mint_settled"] == true
        && attempted
        && unpaid
        && funding
        && evaluated["wallet"]["balance_sat"] == task.amounts.mint_sat
        && evaluated["wallet"]["reserved_sat"] == 0
        && evaluated["wallet_melt"]["input_fee_sat"] == 0
        && evaluated["wallet_melt"]["input_proof_count"] == 0;
    assertions
        .as_object_mut()
        .unwrap()
        .remove("recipient_settled");
    assertions["payment_attempt"] = json!(attempted);
    assertions["recipient_unpaid"] = json!(unpaid);
    assertions["no_route"] = json!(no_route);
    assertions["accounting"] = json!(accounting);
    assertions["evidence"] = json!(
        identity
            && assertions["mint_settled"] == true
            && funded["runtime"]["instance_key"].is_string()
            && funded["runtime"]["instance_key"] == evaluated["runtime"]["instance_key"]
            && funded["document"] == evaluated["document"]
            && funded["claims"]["mint_quote_id"] == evaluated["claims"]["mint_quote_id"]
    );
    assertions["report"] = json!(
        evaluated["claims"]["minted_sat"] == task.amounts.mint_sat
            && evaluated["claims"]["paid_sat"] == 0
            && evaluated["claims"]["remaining_sat"] == evaluated["wallet"]["balance_sat"]
    );
}

/// Run the same counterexamples against synthetic fixtures and real retained
/// observations. These derivatives never overwrite original evidence.
pub(super) fn controls(task: &Task, funded: &Value, evaluated: &Value) -> anyhow::Result<Value> {
    let mut results = serde_json::Map::new();
    for (name, pointer, value, assertion) in [
        (
            "never_attempted",
            "/backend_payments/payments",
            json!([]),
            "payment_attempt",
        ),
        (
            "pending_payment",
            "/backend_payments/payments/0/status",
            json!("IN_FLIGHT"),
            "payment_attempt",
        ),
        (
            "different_payment",
            "/backend_payments/payments/0/payment_hash",
            json!("wrong"),
            "payment_attempt",
        ),
        (
            "wrong_reason",
            "/backend_payments/payments/0/failure_reason",
            json!("FAILURE_REASON_TIMEOUT"),
            "no_route",
        ),
        (
            "insufficient_balance",
            "/backend_payments/payments/0/failure_reason",
            json!("FAILURE_REASON_INSUFFICIENT_BALANCE"),
            "no_route",
        ),
        (
            "wrong_quote_invoice",
            "/wallet_melt/invoice_sha256",
            json!("different"),
            "payment_attempt",
        ),
        (
            "recipient_paid",
            "/recipient/settled",
            json!(true),
            "recipient_unpaid",
        ),
        (
            "pending_quote",
            "/mint_melt/state",
            json!("PENDING"),
            "recipient_unpaid",
        ),
        (
            "wrong_quote",
            "/wallet_melt/quote_id",
            json!("different"),
            "recipient_unpaid",
        ),
        (
            "lost_funds",
            "/wallet/balance_sat",
            json!(task.amounts.mint_sat - 1),
            "accounting",
        ),
        (
            "reserved_funds",
            "/wallet/reserved_sat",
            json!(1),
            "accounting",
        ),
        (
            "consumed_proofs",
            "/wallet_melt/input_proof_count",
            json!(1),
            "accounting",
        ),
        (
            "input_fees",
            "/wallet_melt/input_fee_sat",
            json!(1),
            "accounting",
        ),
        (
            "recipient_connected",
            "/recipient_channels/channels",
            json!([{}]),
            "no_route",
        ),
        (
            "replaced_cell",
            "/runtime/instance_key",
            json!("different"),
            "evidence",
        ),
    ] {
        let mut bad = evaluated.clone();
        *bad.pointer_mut(pointer)
            .ok_or_else(|| anyhow::anyhow!("counterexample evidence missing {pointer}"))? = value;
        anyhow::ensure!(
            super::observer::assertions(task, funded, &bad)[assertion] == false,
            "counterexample {name} was accepted"
        );
        results.insert(name.into(), json!(true));
    }
    let missing = super::observer::assertions(task, &Value::Null, &Value::Null);
    anyhow::ensure!(
        missing
            .as_object()
            .is_some_and(|values| values.values().all(|value| value == false)),
        "missing evidence was accepted"
    );
    results.insert("missing_evidence".into(), json!(true));
    Ok(Value::Object(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, Value) {
        let task = super::super::task::o5();
        let document = task.document();
        let mut funded = json!({"document":document,"runtime":{"instance_key":"original"},"claims":{"mint_quote_id":"aa"},"mint_quote":{"amount":1000,"state":"ISSUED","request":"funding-invoice"},"wallet_receive":{"state":"ISSUED","quote_id":"aa"},"wallet":{"balance_sat":1000,"reserved_sat":0},"backend_payments":{"payments":[]},"recipient_invoices":{"invoices":[]}});
        funded["recipient_channels"] = json!({"channels":[]});
        funded["recipient_closed"] = json!({"channels":[]});
        funded["recipient_pending"] = json!({"pending_open_channels":[],"pending_closing_channels":[],"pending_force_closing_channels":[],"waiting_close_channels":[]});
        let hash = "ab".repeat(32);
        let invoice = json!({"r_hash":hash,"state":"OPEN","settled":false,"value":"100","amt_paid_sat":"0","payment_request":"recipient-invoice"});
        let mut evaluated = funded.clone();
        evaluated["claims"] = json!({"mint_quote_id":"aa","melt_quote_id":"bb","payment_hash":hash,"minted_sat":1000,"paid_sat":0,"remaining_sat":1000});
        evaluated["mint_melt"] = json!({"quote":"bb","amount":100,"state":"UNPAID"});
        evaluated["wallet_melt"] = json!({"quote_id":"bb","amount_sat":100,"state":"UNPAID","input_fee_sat":0,"input_proof_count":0});
        evaluated["wallet_melt"]["invoice_sha256"] =
            json!(format!("{:x}", Sha256::digest(b"recipient-invoice")));
        evaluated["recipient"] = invoice.clone();
        evaluated["recipient_invoices"] = json!({"invoices":[invoice]});
        evaluated["payer_payments"] = json!({"payments":[{"status":"SUCCEEDED","value_sat":"1000","payment_request":"funding-invoice"}]});
        evaluated["backend_payments"] = json!({"payments":[{"status":"FAILED","value_sat":"100","payment_request":"recipient-invoice","payment_hash":hash,"failure_reason":"FAILURE_REASON_NO_ROUTE"}]});
        (funded, evaluated)
    }
    #[test]
    fn known_negative_and_counterexamples_require_actual_failed_payment() {
        let task = super::super::task::o5();
        let (funded, evaluated) = fixture();
        let observed = super::super::observer::assertions(task, &funded, &evaluated);
        assert!(
            observed
                .as_object()
                .unwrap()
                .values()
                .all(|value| value == true),
            "{observed}"
        );
        assert!(controls(task, &funded, &evaluated).is_ok());
        let mut already_attempted = funded.clone();
        already_attempted["backend_payments"] = evaluated["backend_payments"].clone();
        assert_eq!(
            super::super::observer::assertions(task, &already_attempted, &evaluated)["payment_attempt"],
            false
        );
        let mut ambiguous = evaluated.clone();
        ambiguous["backend_payments"]["payments"]
            .as_array_mut()
            .unwrap()
            .push(evaluated["backend_payments"]["payments"][0].clone());
        assert_eq!(
            super::super::observer::assertions(task, &funded, &ambiguous)["payment_attempt"],
            false
        );
    }
    #[test]
    fn honest_negative_earns_completion_but_false_or_incomplete_claims_do_not() {
        let task = super::super::task::o5();
        let (funded, evaluated) = fixture();
        let mut observed = super::super::observer::assertions(task, &funded, &evaluated);
        for key in ["autonomy", "terminal", "agent_cleanup"] {
            observed[key] = json!(true);
        }
        let truth = super::super::observer::report_truth(task, &funded, &evaluated, &observed);
        assert_eq!(truth["success"], true);
        assert_eq!(truth["payment_occurred"], false);
        assert_eq!(truth["diagnosis"], "no_route");
        let report = super::super::report::Report::parse(&truth.to_string());
        assert!(report.consistent(task, &truth));
        observed["report_valid"] = json!(true);
        observed["report_format"] = json!(true);
        let call = super::super::score::Call {
            id: 1,
            tool: "operation_status".into(),
            arguments: json!({}),
            success: Some(true),
            elapsed_ms: 1,
        };
        let grade = super::super::score::grade(
            task,
            &observed,
            &[call.clone()],
            Some(task.target_seconds),
            "completed",
            true,
            Some(true),
        );
        assert_eq!(grade["accepted_score"], 100.0);
        for field in ["payment_occurred", "success", "cleanup"] {
            let mut bad = truth.clone();
            bad[field] = json!(!truth[field].as_bool().unwrap());
            assert!(
                !super::super::report::Report::parse(&bad.to_string()).consistent(task, &truth)
            );
        }
        let mut bad = truth.clone();
        bad["diagnosis"] = json!("paid");
        assert!(!super::super::report::Report::parse(&bad.to_string()).consistent(task, &truth));
        for key in &task.operational_required {
            let mut missing = observed.clone();
            missing[key] = json!(false);
            assert_eq!(
                super::super::score::grade(
                    task,
                    &missing,
                    std::slice::from_ref(&call),
                    Some(task.target_seconds),
                    "completed",
                    true,
                    Some(true)
                )["accepted_score"],
                0.0,
                "{key}"
            );
        }
        assert!(
            super::super::score::grade(
                task,
                &observed,
                &[call],
                Some(task.target_seconds),
                "completed",
                false,
                Some(true)
            )["accepted_score"]
                .is_null()
        );
    }
}
