//! Reconcile harness tool events with the MCP boundary, counting wrappers once.
use super::super::score::Call;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub fn reconcile(mut calls: Vec<Call>, transcript: &[Value]) -> (Vec<Call>, bool) {
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
    let mut unauthorized = false;
    let mut next = calls.iter().map(|call| call.id).max().unwrap_or(0);
    for event in transcript.iter().filter(|v| v["type"] == "tool_use") {
        let part = &event["part"];
        if let Some(call_id) = part["callID"].as_str()
            && !seen.insert(call_id)
        {
            continue;
        }
        let tool = part["tool"].as_str().unwrap_or("unknown");
        let arguments = part["state"]["input"].clone();
        let name = tool.strip_prefix("proofstorm_");
        if let Some(name) = name {
            let key = proofstorm_core::digest_json(&json!([name, arguments]));
            if let Some(indices) = available.get_mut(&key)
                && let Some(index) = indices.pop_front()
            {
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
        // Missing MCP success is unknown telemetry; a harness schema/permission
        // rejection is an attempted call failure even though no MCP frame existed.
        let failed = part["state"]["status"] == "error";
        unauthorized |= name.is_none() && !failed;
        next += 1;
        calls.push(Call {
            id: next,
            tool: name.unwrap_or(tool).into(),
            arguments,
            success: if failed { Some(false) } else { None },
            elapsed_ms: 0,
        });
    }
    if available.values().any(|indices| !indices.is_empty()) {
        next += 1;
        calls.push(Call {
            id: next,
            tool: "telemetry_gap".into(),
            arguments: Value::Null,
            success: None,
            elapsed_ms: 0,
        });
    }
    (calls, unauthorized)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let (calls, unauthorized) = reconcile(vec![call], &[event.clone(), event, rejected]);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].success, Some(false));
        assert!(!unauthorized);
    }
    #[test]
    fn unobserved_success_never_becomes_perfect_telemetry() {
        let event = json!({"type":"tool_use","part":{"callID":"a","tool":"bash","state":{"status":"completed","input":{}}}});
        let (calls, unauthorized) = reconcile(vec![], &[event]);
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
        let (calls, _) = reconcile(vec![call], &[event]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].success, Some(false));
    }
}
