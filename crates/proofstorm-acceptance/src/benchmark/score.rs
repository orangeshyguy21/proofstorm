//! Deterministic scoring. Inputs are runner observations, never agent-awarded points.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const VERSION: &str = "o1-70-15-15/0.3";
pub const TARGET_SECONDS: f64 = 300.0;
pub const DEADLINE_SECONDS: u64 = 1200;
pub const ASSERTIONS: [(&str, u32); 10] = [
    ("components", 10),
    ("bindings", 10),
    ("mint_settled", 10),
    ("recipient_settled", 15),
    ("accounting", 10),
    ("terminal", 10),
    ("evidence", 10),
    ("report", 10),
    ("autonomy", 5),
    ("agent_cleanup", 10),
];

pub fn task() -> Value {
    let operational: Vec<_> = ASSERTIONS
        .iter()
        .filter_map(|(id, _)| (*id != "report").then_some(*id))
        .collect();
    let mut required = operational.clone();
    required.push("report_valid");
    json!({"id":"O1","version":"0.3","suite":"operate","scorer":VERSION,
        "profile":"opencode-mcp-pilot","target_seconds":TARGET_SECONDS,
        "deadline_seconds":DEADLINE_SECONDS,"timing_calibrated":false,
        "mint_sat":1000,"melt_sat":100,"max_total_fee_sat":10,
        "payment_flow":"One successful 1000-sat payer funding payment and one settled 100-sat recipient invoice. No offsetting mint/melt cycles. Retain terminal operation evidence before removal.",
        "assertions":ASSERTIONS,"required":required,"operational_required":operational,
        "report_rule":"Exactly one JSON object earns reporting points. One trailing JSON object after prose may validate structured claims but earns no reporting points. Missing, ambiguous, duplicate-key, or incorrect claims fail report validation. Prose semantics are not graded.",
        "environment_rule":"Runner cleanup and preservation must both pass. Invalid or unknown environment yields null accepted score, not a model failure. Task outcome and diagnostic points remain visible.",
        "tool_rule":"All failures count. Successful calls deduplicated by tool and semantic arguments; request IDs excluded. At most 3 successful observations per identical read/wait. Discovery capped at 3 per tool. Report calls capped at 1. No expected-negative tool calls in O1.",
        "quality_note":"Nine operational assertions and correct structured claims are required. JSON-only report formatting is worth 7 quality points but is not an operational completion gate."})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub tool: String,
    pub arguments: Value,
    pub success: Option<bool>,
    pub elapsed_ms: u64,
}

fn semantic(value: &Value) -> Value {
    match value {
        Value::Object(fields) => fields
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "request_id" | "idempotency_key"))
            .map(|(k, v)| (k.clone(), semantic(v)))
            .collect(),
        Value::Array(items) => items.iter().map(semantic).collect(),
        other => other.clone(),
    }
}

pub fn counts(calls: &[Call]) -> Value {
    let (mut raw_success, mut failures, mut scored, mut pending) = (0_u32, 0_u32, 0_u32, 0_u32);
    let mut seen = BTreeMap::<String, u32>::new();
    let mut ids = BTreeSet::new();
    let mut complete = !calls.is_empty();
    for call in calls {
        complete &= ids.insert(call.id);
        match call.success {
            None => pending += 1,
            Some(false) => failures += 1,
            Some(true) => {
                raw_success += 1;
                let discovery = call.tool.starts_with("catalog_");
                let key = proofstorm_core::digest_json(&json!([
                    call.tool,
                    if discovery {
                        Value::Null
                    } else {
                        semantic(&call.arguments)
                    }
                ]));
                let count = seen.entry(key).or_default();
                *count += 1;
                let limit = if discovery
                    || matches!(
                        call.tool.as_str(),
                        "operation_wait"
                            | "operation_status"
                            | "cell_wait"
                            | "cell_inspect"
                            | "operation_read"
                    ) {
                    3
                } else {
                    1
                };
                if super::allowed(&call.tool) && *count <= limit {
                    scored += 1;
                }
            }
        }
    }
    let denominator = scored + failures;
    let ratio = (denominator > 0).then(|| f64::from(scored) / f64::from(denominator));
    json!({"raw_successes":raw_success,"raw_failures":failures,"pending":pending,
        "scored_successes":scored,"scored_failures":failures,
        "excluded_successes":raw_success-scored,"success_ratio":ratio,
        "failure_ratio":ratio.map(|r|1.0-r),"complete":complete&&pending==0})
}

pub fn grade(
    observations: &Value,
    calls: &[Call],
    elapsed: Option<f64>,
    outcome: &str,
    cleanup: bool,
    preservation: Option<bool>,
) -> Value {
    let assertions: BTreeMap<_, _> = ASSERTIONS
        .iter()
        .map(|(key, _)| (*key, observations[*key] == true))
        .collect();
    let diagnostic: u32 = ASSERTIONS
        .iter()
        .filter(|(key, _)| assertions[key])
        .map(|(_, p)| p)
        .sum();
    let calls = counts(calls);
    let timing = elapsed.filter(|t| t.is_finite() && *t >= 0.0);
    let within_deadline =
        timing.is_none_or(|t| t <= f64::from(u32::try_from(DEADLINE_SECONDS).unwrap()));
    let task_success = assertions
        .iter()
        .all(|(key, pass)| *key == "report" || *pass)
        && outcome == "completed"
        && within_deadline;
    let report_valid = observations["report_valid"] == true;
    let environment_valid = cleanup && preservation == Some(true);
    let valid_completion = task_success && report_valid;
    let tool_points = calls["success_ratio"].as_f64().map(|r| 15.0 * r);
    let time_points =
        timing.map(|t| 15.0 * ((1200.0 - t) / (1200.0 - TARGET_SECONDS)).clamp(0.0, 1.0));
    let quality = f64::from(diagnostic) * 70.0 / 100.0;
    let telemetry = calls["complete"] == true && timing.is_some();
    let task_score = if valid_completion {
        if telemetry {
            tool_points.zip(time_points).map(|(a, b)| quality + a + b)
        } else {
            None
        }
    } else {
        Some(0.0)
    };
    let accepted_score = if environment_valid { task_score } else { None };
    let status = if !environment_valid {
        "invalid_environment"
    } else if !valid_completion {
        "failed"
    } else if telemetry {
        "accepted"
    } else {
        "unscored"
    };
    let mut reasons = Vec::new();
    if !task_success {
        reasons.push("task_incomplete");
    }
    if !report_valid {
        reasons.push("report_invalid");
    }
    if observations["report_format"] != true {
        reasons.push("report_format");
    }
    if !cleanup {
        reasons.push("runner_cleanup_unverified");
    }
    if preservation != Some(true) {
        reasons.push("preservation_unverified");
    }
    if !telemetry {
        reasons.push("scoring_telemetry_incomplete");
    }
    json!({"scorer":VERSION,"task_hash":proofstorm_core::digest_json(&task()),
        "assertions":assertions,"diagnostic_score":diagnostic,
        "task_success":task_success,"report_valid":report_valid,"report_format":observations["report_format"] == true,
        "environment_valid":environment_valid,"accepted_success":if environment_valid {Some(valid_completion)} else {None},
        "status":status,"reasons":reasons,"task_score":task_score,
        "quality_points":quality,"tool_points":tool_points,"time_points":time_points,
        "accepted_score":accepted_score,"tools":calls,"elapsed_seconds":timing,
        "outcome":outcome,"runner_cleanup":cleanup})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn good() -> Value {
        let mut value: Value = ASSERTIONS
            .iter()
            .map(|(id, _)| ((*id).to_owned(), json!(true)))
            .collect();
        value["report_valid"] = json!(true);
        value["report_format"] = json!(true);
        value
    }
    fn grade(
        observations: &Value,
        calls: &[Call],
        elapsed: Option<f64>,
        outcome: &str,
        cleanup: bool,
    ) -> Value {
        super::grade(observations, calls, elapsed, outcome, cleanup, Some(true))
    }
    fn call(id: u64, success: Option<bool>) -> Call {
        Call {
            id,
            tool: "cell_exec".into(),
            arguments: json!({"request_id":id,"script":"true"}),
            success,
            elapsed_ms: 1,
        }
    }
    #[test]
    fn weights_gates_and_time_boundaries() {
        let calls = vec![call(1, Some(true)), call(2, Some(false))];
        let result = grade(&good(), &calls, Some(600.0), "completed", true);
        assert_eq!(result["accepted_score"], 87.5);
        assert_eq!(
            grade(&good(), &calls, Some(300.0), "completed", true)["time_points"],
            15.0
        );
        assert_eq!(
            grade(&good(), &calls, Some(1200.0), "completed", true)["time_points"],
            0.0
        );
        for (id, _) in ASSERTIONS {
            if id == "report" {
                continue;
            }
            let mut obs = good();
            obs[id] = json!(false);
            assert_eq!(
                grade(&obs, &calls, Some(1.0), "completed", true)["accepted_score"],
                0.0
            );
        }
        for outcome in ["timeout", "cancelled", "harness_failure", "grading_failure"] {
            assert_eq!(
                grade(&good(), &calls, Some(1.0), outcome, true)["accepted_score"],
                0.0
            );
        }
        assert!(grade(&good(), &calls, Some(1.0), "completed", false)["accepted_score"].is_null());
        assert_eq!(
            grade(&good(), &calls, Some(1200.1), "completed", true)["accepted_score"],
            0.0
        );
    }
    #[test]
    fn report_format_and_environment_do_not_erase_operational_completion() {
        let calls = [call(1, Some(true))];
        let mut obs = good();
        obs["report"] = json!(false);
        obs["report_format"] = json!(false);
        let result = grade(&obs, &calls, Some(300.0), "completed", true);
        assert_eq!(result["task_success"], true);
        assert_eq!(result["accepted_score"], 93.0);
        assert_eq!(result["quality_points"], 63.0);
        for preservation in [Some(false), None] {
            let result = super::grade(&obs, &calls, Some(300.0), "completed", true, preservation);
            assert_eq!(result["task_success"], true);
            assert_eq!(result["task_score"], 93.0);
            assert_eq!(result["status"], "invalid_environment");
            assert!(result["accepted_score"].is_null());
            assert!(result["accepted_success"].is_null());
        }
        obs["report_valid"] = json!(false);
        let result = grade(&obs, &calls, Some(300.0), "completed", true);
        assert_eq!(result["task_success"], true);
        assert_eq!(result["quality_points"], 63.0);
        assert_eq!(result["accepted_score"], 0.0);
        assert_eq!(result["status"], "failed");
    }
    #[test]
    fn duplicates_do_not_dilute_failure_and_pending_is_not_success() {
        let calls = vec![
            call(1, Some(true)),
            call(2, Some(true)),
            call(3, Some(false)),
            call(4, Some(false)),
        ];
        assert_eq!(counts(&calls)["scored_successes"], 1);
        assert_eq!(counts(&calls)["scored_failures"], 2);
        assert_eq!(counts(&calls)["excluded_successes"], 1);
        assert_eq!(
            grade(&good(), &[call(1, None)], Some(1.0), "completed", true)["status"],
            "unscored"
        );
        assert!(counts(&[])["success_ratio"].is_null());
        assert_eq!(
            grade(&good(), &[], Some(1.0), "completed", true)["status"],
            "unscored"
        );
    }
    #[test]
    fn invalid_timing_and_duplicate_ids_never_score() {
        for timing in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1.0)] {
            assert!(
                grade(&good(), &[call(1, Some(true))], timing, "completed", true)["accepted_score"]
                    .is_null()
            );
        }
        assert_eq!(
            counts(&[call(1, Some(true)), call(1, Some(true))])["complete"],
            false
        );
    }
}
