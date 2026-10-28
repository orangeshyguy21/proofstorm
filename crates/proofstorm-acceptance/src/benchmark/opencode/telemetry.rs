//! Reconcile harness tool events with the MCP boundary, counting wrappers once.
use super::super::{score::Call, task::Task};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub fn reconcile(calls: Vec<Call>, transcript: &[Value], task: Option<&Task>) -> (Vec<Call>, bool) {
    let (calls, unauthorized, _) = detailed(calls, transcript, task);
    (calls, unauthorized)
}

pub(super) fn audit(calls: Vec<Call>, transcript: &[Value], task: Option<&Task>) -> Value {
    detailed(calls, transcript, task).2
}

fn detailed(
    mut calls: Vec<Call>,
    transcript: &[Value],
    task: Option<&Task>,
) -> (Vec<Call>, bool, Value) {
    let explicit_outcomes =
        task.is_some_and(|task| task.rules["interpretation"] == "explicit-outcomes-v1");
    let mut available = BTreeMap::<String, VecDeque<usize>>::new();
    for (index, call) in calls.iter().enumerate() {
        available
            .entry(proofstorm_core::digest_json(&json!([
                call.tool,
                call.arguments
            ])))
            .or_default()
            .push_back(index);
    }
    let mut seen = BTreeSet::new();
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    let original_count = calls.len();
    let mut unauthorized = false;
    let mut next = calls.iter().map(|call| call.id).max().unwrap_or(0);
    for event in transcript.iter().filter(|v| v["type"] == "tool_use") {
        let part = &event["part"];
        // V1 uses callID; V2 uses id, scoped to its assistant message.
        let identity = part["callID"].as_str().map(|id| ("", id)).or_else(|| {
            part["id"]
                .as_str()
                .map(|id| (part["messageID"].as_str().unwrap_or(""), id))
        });
        if let Some(call_id) = identity
            && !seen.insert(call_id)
        {
            continue;
        }
        let tool = part["tool"].as_str().unwrap_or("unknown");
        let arguments = &part["state"]["input"];
        // OpenCode rewrites rejected calls to `invalid`, retaining only the
        // original tool name and error. Classify that attempt against the task,
        // without inventing its original arguments or borrowing MCP evidence.
        let rejected_tool =
            if explicit_outcomes && tool == "invalid" && arguments["error"].is_string() {
                arguments["tool"].as_str()
            } else {
                None
            };
        let tool = rejected_tool.unwrap_or(tool);
        let name = tool
            .strip_prefix("proofstorm_")
            .filter(|name| rejected_tool.is_none() || task.is_some_and(|task| task.allowed(name)));
        if rejected_tool.is_none()
            && let Some(name) = name
        {
            let key = proofstorm_core::digest_json(&json!([name, arguments]));
            if let Some(indices) = available.get_mut(&key)
                && let Some(index) = indices.pop_front()
            {
                matched.push(json!({"proxy_call_id":calls[index].id,"harness_identity":identity}));
                // A reply arriving after the harness timed out is not a
                // successful interaction for the agent, even if MCP succeeded.
                if part["state"]["status"] == "error" {
                    calls[index].success = Some(false);
                } else if part["state"]["status"] != "completed" {
                    calls[index].success = None;
                }
                continue;
            }
        }
        if let Some(name) = name
            && rejected_tool.is_none()
        {
            unmatched.push(json!({"tool":name,"harness_identity":identity,
                "status":part["state"]["status"],"arguments":arguments}));
        }
        // Missing MCP success is unknown telemetry; a harness schema/permission
        // rejection is an attempted call failure even though no MCP frame existed.
        // A completed rejection wrapper is still a failed original call.
        let failed = part["state"]["status"] == "error"
            || (rejected_tool.is_some() && part["state"]["status"] == "completed");
        unauthorized |= name.is_none() && (explicit_outcomes || !failed);
        next += 1;
        calls.push(Call {
            id: next,
            tool: name.unwrap_or(tool).into(),
            arguments: arguments.clone(),
            success: if failed {
                Some(false)
            } else if explicit_outcomes && name.is_none() && part["state"]["status"] == "completed"
            {
                Some(true)
            } else {
                None
            },
            elapsed_ms: 0,
        });
    }
    let remaining: Vec<_> = available.values().flatten().copied().collect();
    for entry in &mut unmatched {
        let candidates: Vec<_> = remaining
            .iter()
            .filter(|&&i| calls[i].tool == entry["tool"])
            .collect();
        entry["same_tool_candidates"] = json!(
            candidates
                .iter()
                .take(16)
                .map(|&&i| json!({
                    "proxy_call_id":calls[i].id,"arguments":calls[i].arguments,
                    "differences":differences(&entry["arguments"], &calls[i].arguments, ""),
                }))
                .collect::<Vec<_>>()
        );
        entry["candidate_count"] = json!(candidates.len());
    }
    let audit = json!({"format_version":1,"matching":"exact-tool-and-arguments",
        "candidate_limit_per_call":16,"difference_limit_per_candidate":64,"arrays_compared_as":"whole_values",
        "proxy_call_count":original_count,"matched":matched,"unmatched_harness_calls":unmatched,
        "unmatched_proxy_call_ids":remaining.iter().map(|&i| calls[i].id).collect::<Vec<_>>(),
        "interpretation":"Same-tool candidates are diagnostic comparisons, not inferred identities or permission to normalize arguments. Scoring remains strict."});
    if !remaining.is_empty() {
        next += 1;
        calls.push(Call {
            id: next,
            tool: "telemetry_gap".into(),
            arguments: Value::Null,
            success: None,
            elapsed_ms: 0,
        });
    }
    (calls, unauthorized, audit)
}

fn differences(left: &Value, right: &Value, path: &str) -> Vec<Value> {
    fn kind(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
    if left == right {
        return Vec::new();
    }
    if let (Some(a), Some(b)) = (left.as_object(), right.as_object()) {
        let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
        return keys.into_iter().flat_map(|key| {
            let pointer = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
            match (a.get(key), b.get(key)) {
                (Some(a), Some(b)) => differences(a, b, &pointer),
                (a, b) => vec![json!({"path":pointer,"kind":"property_presence", "harness_present":a.is_some(),"proxy_present":b.is_some()})],
            }
        }).take(64).collect();
    }
    vec![
        json!({"path":path,"kind":if kind(left) == kind(right) {"value"} else {"type"},"harness_type":kind(left),"proxy_type":kind(right)}),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::super::task;
    use super::*;

    #[test]
    fn argument_audit_explains_coercion_and_stripped_fields_without_repairing_scores() {
        for (harness, proxy, path, kind) in [
            (
                json!({"expected_generation":"1"}),
                json!({"expected_generation":1}),
                "/expected_generation",
                "type",
            ),
            (
                json!({"name":"wallet","fields":["balance"]}),
                json!({"name":"wallet"}),
                "/fields",
                "property_presence",
            ),
        ] {
            let call = Call {
                id: 34,
                tool: "cell_inspect".into(),
                arguments: proxy,
                success: Some(true),
                elapsed_ms: 1,
            };
            let row = json!({"type":"tool_use","part":{"id":"tool-1","messageID":"msg-1","tool":"proofstorm_cell_inspect","state":{"status":"completed","input":harness}}});
            let audit = super::audit(
                vec![call.clone()],
                &[row.clone(), row.clone()],
                Some(task::o1()),
            );
            assert_eq!(audit["unmatched_proxy_call_ids"], json!([34]));
            assert_eq!(
                audit["unmatched_harness_calls"].as_array().unwrap().len(),
                1
            );
            let diff =
                &audit["unmatched_harness_calls"][0]["same_tool_candidates"][0]["differences"][0];
            assert_eq!(diff["path"], path);
            assert_eq!(diff["kind"], kind);
            let (calls, unauthorized) = super::reconcile(vec![call], &[row], Some(task::o1()));
            assert!(!unauthorized);
            assert_eq!(calls.len(), 3);
            assert!(calls[1].success.is_none());
            assert_eq!(calls[2].tool, "telemetry_gap");
        }
    }

    fn reconcile(calls: Vec<Call>, transcript: &[Value], explicit: bool) -> (Vec<Call>, bool) {
        super::reconcile(
            calls,
            transcript,
            Some(if explicit {
                task::o1()
            } else {
                task::lookup("O1", "0.7").unwrap()
            }),
        )
    }

    fn rejection(input: &Value, status: &str) -> Value {
        json!({"type":"tool_use","part":{"callID":"rejected","tool":"invalid","state":{"status":status,"input":input}}})
    }

    #[test]
    fn rejected_wrappers_require_an_allowed_original_tool() {
        for input in [
            json!({"tool":"bash","error":"invalid arguments"}),
            json!({"tool":"other_cell_up","error":"invalid arguments"}),
            json!({"tool":"proofstorm_unknown","error":"unknown tool"}),
            json!({"tool":"cell_up","error":"invalid arguments"}),
            json!({"error":"original tool missing"}),
            json!({"tool":null,"error":"original tool missing"}),
            json!({"tool":"proofstorm_cell_up"}),
        ] {
            for status in ["error", "completed", "running"] {
                let (_, unauthorized) = reconcile(vec![], &[rejection(&input, status)], true);
                assert!(unauthorized, "{status}: {input}");
            }
        }
        let mut restricted = task::o1().clone();
        restricted.allowed_tools.retain(|tool| tool != "cell_up");
        let (_, unauthorized) = super::reconcile(
            vec![],
            &[rejection(
                &json!({"tool":"proofstorm_cell_up","error":"invalid arguments"}),
                "error",
            )],
            Some(&restricted),
        );
        assert!(unauthorized);
    }

    #[test]
    fn rejected_wrappers_never_consume_proxy_evidence_or_complete_pending_calls() {
        let input = json!({"tool":"proofstorm_cell_up","error":"invalid arguments"});
        for status in ["error", "completed", "running"] {
            let captured = Call {
                id: 1,
                tool: "cell_up".into(),
                arguments: input.clone(),
                success: Some(true),
                elapsed_ms: 1,
            };
            let (calls, unauthorized) =
                reconcile(vec![captured], &[rejection(&input, status)], true);
            assert!(!unauthorized);
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0].success, Some(true));
            assert_eq!(calls[1].tool, "cell_up");
            assert_eq!(calls[1].arguments, input);
            assert_eq!(
                calls[1].success,
                if status == "running" {
                    None
                } else {
                    Some(false)
                }
            );
            assert_eq!(calls[2].tool, "telemetry_gap");
            assert!(calls[2].success.is_none());
        }
    }

    #[test]
    fn legacy_rejected_wrappers_keep_their_original_normalization() {
        for (id, version) in [("O1", "0.6"), ("O1", "0.7"), ("O5", "0.2"), ("O5", "0.3")] {
            for status in ["error", "completed", "running"] {
                let input = json!({"tool":"proofstorm_cell_up","error":"invalid arguments"});
                let (calls, unauthorized) = super::reconcile(
                    vec![],
                    &[rejection(&input, status)],
                    task::lookup(id, version),
                );
                assert_eq!(unauthorized, status != "error");
                assert_eq!(calls[0].tool, "invalid");
                assert_eq!(calls[0].arguments, input);
                assert_eq!(
                    calls[0].success,
                    if status == "error" { Some(false) } else { None }
                );
            }
        }
    }

    #[test]
    fn foreign_attempts_violate_scope_independently_of_their_outcome() {
        for status in ["error", "completed", "running"] {
            let event = json!({"type":"tool_use","part":{"callID":"a","tool":"bash","state":{"status":status,"input":{}}}});
            let (calls, unauthorized) = reconcile(vec![], &[event], true);
            assert!(unauthorized);
            assert_eq!(
                calls[0].success,
                match status {
                    "error" => Some(false),
                    "completed" => Some(true),
                    _ => None,
                }
            );
        }
    }
    #[test]
    fn wrappers_count_once_and_rejected_arguments_count_as_failures() {
        let args = json!({"name":"benchmark-o1"});
        let call = Call {
            id: 1,
            tool: "cell_inspect".into(),
            arguments: args.clone(),
            success: Some(true),
            elapsed_ms: 1,
        };
        let event = json!({"type":"tool_use","part":{"callID":"a","tool":"proofstorm_cell_inspect","state":{"status":"completed","input":args}}});
        let rejected = json!({"type":"tool_use","part":{"callID":"b","tool":"invalid","state":{"status":"error","input":{}}}});
        let (calls, unauthorized) = reconcile(vec![call], &[event.clone(), event, rejected], false);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].success, Some(false));
        assert!(!unauthorized);
    }

    #[test]
    fn v2_ids_deduplicate_within_messages_but_not_across_turns() {
        let first = json!({"type":"tool_use","part":{"id":"same-id","messageID":"first","tool":"shell","state":{"status":"error","input":{}}}});
        let mut second = first.clone();
        second["part"]["messageID"] = json!("second");
        let (calls, unauthorized) = reconcile(vec![], &[first.clone(), first, second], true);
        assert!(unauthorized);
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.success == Some(false)));
    }
    #[test]
    fn unobserved_success_never_becomes_perfect_telemetry() {
        let event = json!({"type":"tool_use","part":{"callID":"a","tool":"bash","state":{"status":"completed","input":{}}}});
        let (calls, unauthorized) = reconcile(vec![], &[event], false);
        assert!(unauthorized);
        assert!(calls[0].success.is_none());
    }

    #[test]
    fn missing_harness_counterpart_keeps_telemetry_incomplete() {
        let (calls, _) = reconcile(
            vec![Call {
                id: 1,
                tool: "cell_up".into(),
                arguments: json!({}),
                success: Some(true),
                elapsed_ms: 1,
            }],
            &[],
            false,
        );
        assert_eq!(calls.len(), 2);
        assert!(calls[1].success.is_none());
    }

    #[test]
    fn harness_timeout_cannot_be_hidden_by_a_late_successful_reply() {
        let call = Call {
            id: 1,
            tool: "cell_up".into(),
            arguments: json!({}),
            success: Some(true),
            elapsed_ms: 120_000,
        };
        let event = json!({"type":"tool_use","part":{"callID":"a","tool":"proofstorm_cell_up","state":{"status":"error","input":{}}}});
        let (calls, _) = reconcile(vec![call], &[event], false);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].success, Some(false));
    }
}
