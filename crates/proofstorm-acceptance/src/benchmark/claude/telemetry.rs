//! Join Claude Code stream-json tool uses to independent proxy calls, never
//! trusting model claims. Unknown events leave telemetry incomplete.
use super::{AttemptOutput, expected_tools, json_lines};
use crate::benchmark::{Context, calls, events, read, score::Call};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::Path,
};

pub(super) fn retained(work: &Path) -> Result<AttemptOutput> {
    let attempt =
        read(&work.join("benchmark-attempt.json")).unwrap_or(json!({"outcome":"not_started"}));
    let (rows, mut complete) =
        json_lines(&fs::read_to_string(work.join("harness.jsonl")).unwrap_or_default());
    let expected = read(&work.join("benchmark-context.json"))
        .ok()
        .and_then(|value| serde_json::from_value::<Context>(value).ok())
        .map(|context| expected_tools(&context.task));
    let mut unauthorized = false;
    let mut inits = Vec::new();
    let mut result = None;
    let mut uses = Vec::<(String, String, Value)>::new();
    let mut seen = BTreeMap::<String, Value>::new();
    let mut results = BTreeMap::<String, bool>::new();
    let mut last_text = String::new();
    let mut models = BTreeSet::new();
    for row in &rows {
        match row["type"].as_str() {
            Some("system") => {
                if row["subtype"] == "init" {
                    inits.push(row);
                }
            }
            Some("rate_limit_event") => {}
            Some("tool_progress") => {
                // CLI 2.1.281 emits periodic heartbeats with a synthetic ID and
                // the real call ID in parent_tool_use_id. They are progress,
                // never another attempt or evidence of a successful reply.
                let parent = row["parent_tool_use_id"].as_str();
                let known = parent.and_then(|id| seen.get(id));
                complete &= row["heartbeat"] == true
                    && row["tool_use_id"].as_str().is_some_and(|id| !id.is_empty())
                    && row["elapsed_time_seconds"]
                        .as_f64()
                        .is_some_and(|elapsed| elapsed.is_finite() && elapsed >= 0.0)
                    && known.is_some_and(|call| call["name"] == row["tool_name"])
                    && row["task_id"].is_null()
                    && row["subagent_type"].is_null()
                    && row["subagent_retry"].is_null();
            }
            Some("assistant") => {
                // Subagent traffic cannot occur without a delegation tool.
                unauthorized |= !row["parent_tool_use_id"].is_null();
                let message = &row["message"];
                if let Some(model) = message["model"].as_str()
                    && model != "<synthetic>"
                {
                    models.insert(model.to_owned());
                }
                for block in message["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => last_text = block["text"].as_str().unwrap_or("").into(),
                        Some("thinking" | "redacted_thinking") => {}
                        Some("tool_use") => {
                            let Some(id) = block["id"].as_str() else {
                                complete = false;
                                continue;
                            };
                            if let Some(previous) = seen.get(id) {
                                complete &= previous == block;
                                continue;
                            }
                            seen.insert(id.into(), block.clone());
                            uses.push((
                                id.into(),
                                block["name"].as_str().unwrap_or("unknown").into(),
                                block["input"].clone(),
                            ));
                        }
                        Some("server_tool_use") => {
                            unauthorized = true;
                            uses.push((
                                block["id"].as_str().unwrap_or("").into(),
                                "server_tool_use".into(),
                                block.clone(),
                            ));
                        }
                        _ => complete = false,
                    }
                }
            }
            Some("user") => {
                for block in row["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        let Some(id) = block["tool_use_id"].as_str() else {
                            complete = false;
                            continue;
                        };
                        let failed = block["is_error"] == true;
                        if results
                            .insert(id.into(), failed)
                            .is_some_and(|old| old != failed)
                        {
                            complete = false;
                        }
                    }
                }
            }
            Some("result") => {
                complete &= result.is_none();
                result = Some(row);
            }
            _ => complete = false,
        }
    }
    complete &= !inits.is_empty();
    for init in &inits {
        let listed: Option<BTreeSet<String>> = init["tools"].as_array().map(|tools| {
            tools
                .iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect()
        });
        // A session exposing any tool outside the profile is not autonomous-by-contract.
        match &expected {
            Some(expected) => unauthorized |= listed.as_ref() != Some(expected),
            None => complete = false,
        }
    }
    let captured = events(work).and_then(|rows| calls(&rows));
    let mut telemetry_error = captured.as_ref().err().map(ToString::to_string);
    let mut calls = captured.unwrap_or_default();
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
    for (id, name, input) in uses {
        let failed = results.get(&id) == Some(&true);
        let finished = results.contains_key(&id);
        let owned = name.strip_prefix(&format!("mcp__{}__", super::SERVER));
        if let Some(tool) = owned
            && let Some(index) = available
                .get_mut(&proofstorm_core::digest_json(&json!([tool, input])))
                .and_then(VecDeque::pop_front)
        {
            // A harness-side error or a missing reply is never a proxy success.
            if failed {
                calls[index].success = Some(false);
            } else if !finished {
                calls[index].success = None;
            }
            continue;
        }
        // A permission refusal is a failed attempt; an unobserved success elsewhere
        // is unauthorized. Missing proxy counterparts stay unknown.
        unauthorized |= owned.is_none() && !failed;
        push(
            &mut calls,
            owned.unwrap_or(&name),
            input,
            if failed { Some(false) } else { None },
        )?;
    }
    if available.values().any(|v| !v.is_empty()) {
        complete = false;
    }
    if !complete || telemetry_error.is_some() {
        push(&mut calls, "telemetry_gap", Value::Null, None)?;
        telemetry_error
            .get_or_insert_with(|| "incomplete or unrecognized Claude Code/proxy events".into());
    }
    let mut outcome = attempt["outcome"]
        .as_str()
        .unwrap_or("not_started")
        .to_owned();
    if outcome == "running" {
        outcome = "interrupted".into();
    }
    let succeeded = result.is_some_and(|r| r["subtype"] == "success" && r["is_error"] == false);
    if outcome == "completed" && !succeeded {
        outcome = "provider_or_harness_failure".into();
    }
    let final_text = result
        .filter(|_| succeeded)
        .and_then(|r| r["result"].as_str())
        .map_or(last_text, str::to_owned);
    let result = result.cloned().unwrap_or(Value::Null);
    let init = inits.first().copied().cloned().unwrap_or(Value::Null);
    Ok(AttemptOutput {
        outcome,
        elapsed_seconds: attempt["elapsed_seconds"].as_f64(),
        final_text,
        usage: json!({"total_cost_usd":result["total_cost_usd"],"cost_basis":"Claude Code reported estimate",
            "usage":result["usage"],"model_usage":result["modelUsage"],"num_turns":result["num_turns"],
            "duration_api_ms":result["duration_api_ms"],"terminal_reason":result["terminal_reason"],
            "result_subtype":result["subtype"],"permission_denials":result["permission_denials"],
            "models_observed":models,"api_key_source":init["apiKeySource"],
            "claude_code_version":init["claude_code_version"],"session_model":init["model"]}),
        calls,
        unauthorized,
        telemetry_error,
    })
}

fn push(calls: &mut Vec<Call>, tool: &str, arguments: Value, success: Option<bool>) -> Result<()> {
    let id = calls
        .iter()
        .map(|c| c.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .context("call ID overflow")?;
    calls.push(Call {
        id,
        tool: tool.into(),
        arguments,
        success,
        elapsed_ms: 0,
    });
    Ok(())
}
