//! Observed MCP wall intervals, not provider-compute estimates or score deductions.
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn summarize(events: &[Value], elapsed: &Value) -> Value {
    let mut starts = BTreeMap::new();
    let (mut all, mut checkpoints, mut cleanup) = (Vec::new(), Vec::new(), Vec::new());
    for event in events {
        if event["kind"] == "start" {
            if let (Some(id), Some(at)) = (event["id"].as_u64(), event["at_unix_ms"].as_u64()) {
                starts.insert(
                    id,
                    (
                        at,
                        event["tool"].as_str().unwrap_or("").to_owned(),
                        event["arguments"].clone(),
                    ),
                );
            }
        } else if event["kind"] == "end"
            && let (Some(id), Some(ms)) = (event["id"].as_u64(), event["elapsed_ms"].as_u64())
            && let Some((at, tool, args)) = starts.remove(&id)
            && let Some(end) = at.checked_add(ms)
        {
            all.push((at, end));
            if tool == "benchmark_checkpoint" {
                checkpoints.push((at, end));
            }
            if tool == "cell_remove" || (tool == "cell_wait" && args["target_phase"] == "closed") {
                cleanup.push((at, end));
            }
        }
    }
    json!({"elapsed_seconds":elapsed,"mcp_busy_ms":union_ms(all),
        "checkpoint_busy_ms":union_ms(checkpoints),"cleanup_busy_ms":union_ms(cleanup),
        "unfinished_timed_calls":starts.len(),
        "interpretation":"Union of observed completed MCP call intervals within each category. Categories may overlap and must not be summed. Background native work and provider reasoning cannot be separated from these receipts. End-to-end score is unchanged."})
}

fn union_ms(mut spans: Vec<(u64, u64)>) -> u64 {
    spans.sort_unstable();
    let mut end = 0;
    let mut total: u64 = 0;
    for (start, next) in spans {
        total = total.saturating_add(next.saturating_sub(start.max(end)));
        end = end.max(next);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_calls_are_not_double_counted_and_unfinished_calls_are_not_invented() {
        let events = vec![
            json!({"kind":"start","id":1,"at_unix_ms":100,"tool":"benchmark_checkpoint"}),
            json!({"kind":"start","id":2,"at_unix_ms":150,"tool":"cell_remove"}),
            json!({"kind":"end","id":2,"elapsed_ms":100}),
            json!({"kind":"end","id":1,"elapsed_ms":100}),
            json!({"kind":"start","id":3,"at_unix_ms":300,"tool":"cell_wait"}),
        ];
        let result = summarize(&events, &json!(1.0));
        assert_eq!(result["mcp_busy_ms"], 150);
        assert_eq!(result["checkpoint_busy_ms"], 100);
        assert_eq!(result["cleanup_busy_ms"], 100);
        assert_eq!(result["unfinished_timed_calls"], 1);
    }
}
