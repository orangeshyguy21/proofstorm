//! Deterministic scoring. Inputs are runner observations, never agent-awarded points.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn task() -> Value {
    json!(super::task::o1())
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

pub fn counts(task: &super::task::Task, calls: &[Call]) -> Value {
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
                if task.allowed(&call.tool) && *count <= limit {
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
    task: &super::task::Task,
    observations: &Value,
    calls: &[Call],
    elapsed: Option<f64>,
    outcome: &str,
    cleanup: bool,
    preservation: Option<bool>,
) -> Value {
    let assertions: BTreeMap<_, _> = task
        .assertions
        .iter()
        .map(|(key, _)| (key.as_str(), observations[key] == true))
        .collect();
    let diagnostic: u32 = task
        .assertions
        .iter()
        .filter(|(key, _)| assertions[key.as_str()])
        .map(|(_, p)| p)
        .sum();
    let calls = counts(task, calls);
    let timing = elapsed.filter(|t| t.is_finite() && *t >= 0.0);
    let within_deadline = timing.is_none_or(|t| t <= f64::from(task.deadline_seconds));
    let task_success = task
        .operational_required
        .iter()
        .all(|key| observations[key] == true)
        && outcome == "completed"
        && within_deadline;
    let report_valid = observations["report_valid"] == true;
    let environment_valid = cleanup && preservation == Some(true);
    let valid_completion = task_success && report_valid;
    let tool_points = calls["success_ratio"]
        .as_f64()
        .map(|r| f64::from(task.score_weights[1]) * r);
    let time_points = timing.map(|t| {
        f64::from(task.score_weights[2])
            * ((f64::from(task.deadline_seconds) - t)
                / (f64::from(task.deadline_seconds) - task.target_seconds))
                .clamp(0.0, 1.0)
    });
    let quality = f64::from(diagnostic) * f64::from(task.score_weights[0])
        / f64::from(
            task.assertions
                .iter()
                .map(|(_, weight)| weight)
                .sum::<u32>(),
        );
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
    json!({"scorer":task.scorer,"task_hash":proofstorm_core::digest_json(task),
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
    fn counts(calls: &[Call]) -> Value {
        super::counts(super::super::task::o1(), calls)
    }
    fn good() -> Value {
        let mut value: Value = super::super::task::o1()
            .assertions
            .iter()
            .map(|(id, _)| (id.to_owned(), json!(true)))
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
        super::grade(
            super::super::task::o1(),
            observations,
            calls,
            elapsed,
            outcome,
            cleanup,
            Some(true),
        )
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
        for (id, _) in &super::super::task::o1().assertions {
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
            let result = super::grade(
                super::super::task::o1(),
                &obs,
                &calls,
                Some(300.0),
                "completed",
                true,
                preservation,
            );
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
