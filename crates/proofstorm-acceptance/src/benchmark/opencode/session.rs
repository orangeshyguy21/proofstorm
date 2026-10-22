//! Normalize the durable v2 session, whose final turn the CLI may omit.
use super::read;
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn rows(work: &Path) -> Result<Vec<Value>> {
    let export = read(&work.join("opencode-session.private.json"))?;
    let messages = export["data"]["messages"]
        .as_array()
        .context("session messages missing")?;
    let mut rows = Vec::new();
    for message in messages {
        if message["type"] == "error"
            || (message["type"] == "idle" && message["outcome"] == "failed")
        {
            rows.push(json!({"type":"error","part":message}));
        }
        if message["type"] != "assistant" {
            continue;
        }
        for part in message["content"]
            .as_array()
            .context("assistant content missing")?
        {
            match part["type"].as_str() {
                Some("tool") => rows.push(json!({"type":"tool_use","part":{"id":part["id"],"messageID":message["id"],"tool":part["name"],"state":part["state"]}})),
                Some("text") => rows.push(json!({"type":"text","part":part})),
                _ => {}
            }
        }
        rows.push(json!({"type":"step_finish","part":{"tokens":message["tokens"],"cost":message["cost"],"reason":message["finish"]}}));
    }
    Ok(rows)
}
