//! Join Codex item lifecycles to independent proxy calls, never trusting model claims.
use super::{AttemptOutput, read};
use crate::benchmark::{calls, events, score::Call};
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
    let transcript = fs::read_to_string(work.join("harness.jsonl")).unwrap_or_default();
    let mut complete = !transcript.is_empty();
    let mut items = BTreeMap::<String, Value>::new();
    let mut order = Vec::new();
    let mut terminal = BTreeSet::new();
    let mut final_text = String::new();
    let mut usages = Vec::new();
    let mut finished = false;
    let mut failed = false;
    for line in transcript.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            complete = false;
            continue;
        };
        match row["type"].as_str() {
            Some("thread.started" | "turn.started") => {}
            Some("turn.completed") => {
                finished = true;
                usages.push(row["usage"].clone());
            }
            Some("turn.failed" | "error") => {
                failed = true;
            }
            Some("item.started" | "item.updated" | "item.completed") => {
                let item = &row["item"];
                if item["type"] == "agent_message" && row["type"] == "item.completed" {
                    final_text = item["text"].as_str().unwrap_or("").into();
                }
                if let Some(id) = item["id"].as_str() {
                    if !items.contains_key(id) {
                        order.push(id.to_owned());
                    }
                    if terminal.contains(id)
                        && (row["type"] != "item.completed" || items.get(id) != Some(item))
                    {
                        complete = false;
                    }
                    if items
                        .get(id)
                        .is_some_and(|previous| previous["type"] != item["type"])
                    {
                        complete = false;
                    }
                    if let Some(previous) = items.get(id)
                        && item["type"] == "mcp_tool_call"
                        && ["server", "tool", "arguments"]
                            .iter()
                            .any(|key| previous[*key] != item[*key])
                    {
                        complete = false;
                    }
                    items.insert(id.into(), item.clone());
                    if row["type"] == "item.completed" {
                        terminal.insert(id.to_owned());
                    }
                } else {
                    complete = false;
                }
            }
            _ => complete = false,
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
    let mut unauthorized = false;
    for id in order {
        let item = &items[&id];
        match item["type"].as_str() {
            Some("agent_message" | "reasoning" | "todo_list") => {}
            Some("error") => {
                failed = true;
            }
            Some("mcp_tool_call") => {
                let name = item["tool"].as_str().unwrap_or("unknown");
                let args = item["arguments"].clone();
                let failed_call = item["status"] == "failed"
                    || !item["error"].is_null()
                    || item["result"]["isError"] == true;
                let owned = item["server"] == "proofstorm";
                let key = proofstorm_core::digest_json(&json!([name, args]));
                if owned && let Some(index) = available.get_mut(&key).and_then(VecDeque::pop_front)
                {
                    if failed_call {
                        calls[index].success = Some(false);
                    } else if item["status"] != "completed" {
                        calls[index].success = None;
                    }
                } else {
                    unauthorized |= !owned && !failed_call;
                    push(
                        &mut calls,
                        name,
                        args,
                        if failed_call { Some(false) } else { None },
                    )?;
                }
            }
            Some("command_execution" | "file_change" | "web_search" | "collab_tool_call") => {
                unauthorized = true;
                push(
                    &mut calls,
                    item["type"].as_str().unwrap(),
                    item.clone(),
                    None,
                )?;
            }
            _ => {
                complete = false;
                unauthorized = true;
            }
        }
    }
    if available.values().any(|v| !v.is_empty()) {
        complete = false;
    }
    if !complete || telemetry_error.is_some() {
        push(&mut calls, "telemetry_gap", Value::Null, None)?;
        telemetry_error
            .get_or_insert_with(|| "incomplete or unrecognized Codex/proxy events".into());
    }
    let mut outcome = attempt["outcome"]
        .as_str()
        .unwrap_or("not_started")
        .to_owned();
    if outcome == "running" {
        outcome = "interrupted".into();
    }
    if outcome == "completed" && (!finished || failed) {
        outcome = "provider_or_harness_failure".into();
    }
    Ok(AttemptOutput {
        outcome,
        elapsed_seconds: attempt["elapsed_seconds"].as_f64(),
        final_text,
        usage: json!({"turns":usages,"cost":null}),
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
