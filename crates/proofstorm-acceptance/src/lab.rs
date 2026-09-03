//! Lab lifecycle helpers shared by every gate.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{McpClient, json as expect};

/// Poll `proofstorm_lab_status` until the lab reports `phase`.
///
/// Mirrors the fixed-attempt, fixed-delay loop every Python client used, so a
/// hung lab fails the gate rather than hanging the run.
pub fn wait_phase(
    client: &mut McpClient,
    instance_id: &str,
    phase: &str,
    attempts: u32,
    delay: Duration,
) -> Result<Value> {
    let mut last = Value::Null;
    for attempt in 0..attempts {
        last = client.call("proofstorm_lab_status", json!({"instance_id": instance_id}))?;
        if expect::string(&last, "/phase")? == phase {
            return Ok(last);
        }
        if attempt + 1 < attempts {
            sleep(delay);
        }
    }
    bail!("lab {instance_id} did not reach phase {phase}: {last}");
}

/// Wait for readiness with the three-second cadence the gates use.
pub fn wait_ready(client: &mut McpClient, instance_id: &str) -> Result<Value> {
    wait_phase(client, instance_id, "ready", 160, Duration::from_secs(3))
}

/// Wait for a verified close.
pub fn wait_closed(client: &mut McpClient, instance_id: &str) -> Result<Value> {
    wait_phase(client, instance_id, "closed", 60, Duration::from_secs(3))
}

/// Poll `proofstorm_operation_status` until the operation reaches a terminal phase.
///
/// A `failed` or `cancelled` phase aborts immediately rather than burning the
/// remaining attempts, matching the Python helper.
pub fn wait_operation(client: &mut McpClient, operation_id: &str, attempts: u32) -> Result<Value> {
    for attempt in 0..attempts {
        let operation = client.call(
            "proofstorm_operation_status",
            json!({"operation_id": operation_id}),
        )?;
        match expect::string(&operation, "/phase")? {
            "succeeded" => return Ok(operation),
            "failed" | "cancelled" => {
                bail!("operation {operation_id} failed: {operation}")
            }
            _ => {}
        }
        if attempt + 1 < attempts {
            sleep(delay_seconds(3));
        }
    }
    bail!("operation {operation_id} did not finish within {attempts} attempts");
}

/// Poll until an operation reaches one exact phase, failing on any other
/// terminal phase.
pub fn wait_operation_phase(
    client: &mut McpClient,
    operation_id: &str,
    expected: &str,
    attempts: u32,
) -> Result<Value> {
    for attempt in 0..attempts {
        let operation = client.call(
            "proofstorm_operation_status",
            json!({"operation_id": operation_id}),
        )?;
        let phase = expect::string(&operation, "/phase")?;
        if phase == expected {
            return Ok(operation);
        }
        if matches!(phase, "succeeded" | "failed" | "cancelled") {
            bail!("operation {operation_id} reached {phase}, expected {expected}");
        }
        if attempt + 1 < attempts {
            sleep(Duration::from_secs(1));
        }
    }
    bail!("operation {operation_id} did not reach {expected}");
}

/// Wait for an operation with the 180-attempt default the gates use.
pub fn wait_succeeded(client: &mut McpClient, operation_id: &str) -> Result<Value> {
    wait_operation(client, operation_id, 180)
}

/// The `content` object of a succeeded operation's terminal artifact.
pub fn artifact_content(operation: &Value) -> Result<&Value> {
    operation
        .pointer("/artifact/content")
        .ok_or_else(|| anyhow::anyhow!("operation has no artifact content: {operation}"))
}

fn delay_seconds(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// Find the resolved lock entry for one catalog identity.
pub fn lock_entry<'a>(published: &'a Value, catalog_id: &str) -> Result<&'a Value> {
    let entries = expect::array(published, "/lock/entries")?;
    for entry in entries {
        if entry.get("catalog_id").and_then(Value::as_str) == Some(catalog_id) {
            return Ok(entry);
        }
    }
    bail!("no lock entry for {catalog_id} in {published}");
}
