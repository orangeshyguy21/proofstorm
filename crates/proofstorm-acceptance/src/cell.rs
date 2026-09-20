//! Cell lifecycle helpers shared by every gate.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{McpClient, json as expect};

/// Poll `cell_inspect` until the cell reports `phase`.
///
/// Mirrors the fixed-attempt, fixed-delay loop every Python client used, so a
/// hung cell fails the gate rather than hanging the run.
pub fn wait_phase(
    client: &mut McpClient,
    instance_id: &str,
    phase: &str,
    attempts: u32,
    delay: Duration,
) -> Result<Value> {
    if phase == "closed" {
        return wait_closed(client, instance_id);
    }
    let mut last = Value::Null;
    for attempt in 0..attempts {
        last = status(client, instance_id)?;
        if expect::string(&last, "/phase")? == phase {
            return Ok(last);
        }
        if attempt + 1 < attempts {
            sleep(delay);
        }
    }
    bail!("cell {instance_id} did not reach phase {phase}: {last}");
}

/// Wait for readiness and record reconciliation through the canonical wait contract.
pub fn wait_ready(client: &mut McpClient, instance_id: &str) -> Result<Value> {
    for _ in 0..8 {
        let waited = client.call(
            "cell_wait",
            json!({"name":instance_id,"target_phase":"ready","timeout_seconds":60}),
        )?;
        if waited["reached"] == true {
            return status(client, instance_id);
        }
        if waited["timed_out"] != true {
            bail!("cell readiness blocked or superseded: {waited}");
        }
    }
    bail!("cell {instance_id} did not become ready within 480 seconds")
}

/// Wait for a verified close.
pub fn wait_closed(client: &mut McpClient, instance_id: &str) -> Result<Value> {
    // Verified close removes the named instance. Wait on the cached incarnation
    // token rather than calling cell_status for a name that may already be absent.
    let mut last = Value::Null;
    for _ in 0..3 {
        last = client.call(
            "cell_wait",
            json!({"name":instance_id,"target_phase":"closed","timeout_seconds":60}),
        )?;
        if last["reached"] == true && last["teardown_receipt"]["verified_absent"] == true {
            return Ok(last);
        }
    }
    bail!("cell {instance_id} close was not verified: {last}");
}

/// Poll `operation_status` until the operation reaches a terminal phase.
///
/// A `failed` or `cancelled` phase aborts immediately rather than burning the
/// remaining attempts, matching the Python helper.
pub fn wait_operation(client: &mut McpClient, operation_id: &str, attempts: u32) -> Result<Value> {
    for attempt in 0..attempts {
        let operation = client.call("operation_status", json!({"operation_id": operation_id}))?;
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
        let operation = client.call("operation_status", json!({"operation_id": operation_id}))?;
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

/// Assemble a review locally from bounded public reads. This is not a server-side draft API.
pub fn review(client: &mut McpClient, preview: &Value) -> Result<Value> {
    let target = json!({"plan_id":expect::string(preview,"/plan/id")?});
    let configuration = read_document(client, &target, "configuration", "")?;
    let lock = read_document(client, &target, "lock", "")?;
    Ok(
        json!({"digest":preview["revision_digest"],"lock":lock,"cell":configuration,"plan":preview["plan"]}),
    )
}

pub fn apply(client: &mut McpClient, preview: &Value) -> Result<Value> {
    client.call("cell_up",json!({"name":preview["name"],"request_id":format!("apply-{}",expect::string(preview,"/plan/id")?),"plan":preview["plan"]}))
}

/// Reconstruct an exact document by following the public scan/slice contract.
pub fn read_document(
    client: &mut McpClient,
    target: &Value,
    document: &str,
    pointer: &str,
) -> Result<Value> {
    fn portion(
        client: &mut McpClient,
        target: &Value,
        document: &str,
        pointer: &str,
        digest: Option<&str>,
    ) -> Result<Value> {
        let mut arguments = target.clone();
        arguments["document"] = json!(document);
        arguments["pointer"] = json!(pointer);
        if let Some(digest) = digest {
            arguments["expected_digest"] = json!(digest);
        }
        let first = client.call("cell_read", arguments.clone())?;
        let digest = expect::string(&first, "/document_digest")?;
        arguments["expected_digest"] = json!(digest);
        let mut page = first.clone();
        let mut result = first["value"].clone();
        if first["scan"] == true {
            let mut object = serde_json::Map::new();
            loop {
                for entry in expect::array(&page, "/entries")? {
                    let path = expect::string(entry, "/path")?;
                    let key = path
                        .rsplit('/')
                        .next()
                        .unwrap()
                        .replace("~1", "/")
                        .replace("~0", "~");
                    object.insert(key, portion(client, target, document, path, Some(digest))?);
                }
                if page["next_offset"].is_null() {
                    break;
                }
                arguments["offset"] = page["next_offset"].clone();
                page = client.call("cell_read", arguments.clone())?;
            }
            return Ok(Value::Object(object));
        }
        while !page["next_offset"].is_null() {
            arguments["offset"] = page["next_offset"].clone();
            page = client.call("cell_read", arguments.clone())?;
            match &mut result {
                Value::String(text) => text.push_str(expect::string(&page, "/value")?),
                Value::Array(items) => {
                    items.extend(expect::array(&page, "/value")?.iter().cloned());
                }
                _ => bail!("non-slice returned next_offset: {page}"),
            }
        }
        Ok(result)
    }
    portion(client, target, document, pointer, None)
}

/// Exact detailed status selected through the canonical inspector.
pub fn status(client: &mut McpClient, name: &str) -> Result<Value> {
    let value = client.call("cell_inspect", json!({"name":name,"fields":["/runtime"]}))?;
    let mut runtime = value["selected"]["/runtime"].clone();
    if runtime.is_null() {
        bail!("cell has no observed runtime: {value}");
    }
    // The selected runtime is already a compact status with its canonical instance_id.
    expect::string(&runtime, "/instance_id")?;
    runtime["instance_key"] = value["instance_key"].clone();
    Ok(runtime)
}

/// The shared wait accepts arrays even when the caller awaits one command.
pub fn wait_one(client: &mut McpClient, id: &str, timeout_seconds: u64) -> Result<Value> {
    let waited = client.call(
        "operation_wait",
        json!({"operation_ids":[id],"timeout_seconds":timeout_seconds}),
    )?;
    let operation = waited["operations"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or_else(|| anyhow::anyhow!("wait failed: {waited}"))?;
    Ok(operation.clone())
}

/// Wrapper-specific rejection tests are replaced by absence from the one public contract.
pub fn assert_retired_wallet_routes(client: &mut McpClient) -> Result<()> {
    let tools = client.request("tools/list", json!({}))?;
    for tool in expect::array(&tools, "/tools")? {
        anyhow::ensure!(
            ![
                "wallet_balance",
                "wallet_initialize",
                "wallet_fund",
                "wallet_round_trip",
                "wallet_quote_claim",
                "wallet_melt_quote_refresh",
                "wallet_pay",
                "wallet_invoice",
                "conservation_oracle",
            ]
            .contains(&expect::string(tool, "/name")?),
            "retired wallet route advertised"
        );
    }
    Ok(())
}

/// Assemble immutable bulk evidence locally for scientific assertions.
pub fn evidence(client: &mut McpClient, request: Value) -> Result<Value> {
    let mut manifest = client.call("evidence_export", request)?;
    let resource = client.request(
        "resources/read",
        json!({"uri":expect::string(&manifest,"/resource_uri")?}),
    )?;
    let content: Value = serde_json::from_str(expect::string(&resource, "/contents/0/text")?)?;
    anyhow::ensure!(
        manifest["digest"] == content["digest"],
        "export resource digest differs"
    );
    manifest["content"] = content["content"].clone();
    Ok(manifest)
}

pub fn assert_tool_absent(client: &mut McpClient, name: &str) -> Result<()> {
    let tools = client.request("tools/list", json!({}))?;
    anyhow::ensure!(
        expect::array(&tools, "/tools")?
            .iter()
            .all(|tool| tool["name"] != name),
        "retired {name} advertised"
    );
    Ok(())
}

/// Collect selected recorded journal fields through search; no runtime polling or hidden list endpoint.
pub fn journal(client: &mut McpClient, run: &str) -> Result<Vec<Value>> {
    let scope = client.call("run_read", json!({"run_id":run}))?;
    let mut request = json!({"name":expect::string(&scope,"/instance_id")?,"run_id":run,"limit":50,
        "fields":["/id","/sequence","/kind","/phase","/principal_id","/session_id","/capability","/request","/resource_name","/revision_digest"]});
    let mut actions = Vec::new();
    loop {
        let page = client.call("activity_search", request.clone())?;
        for item in expect::array(&page, "/items")? {
            let mut action = serde_json::Map::new();
            for field in expect::array(item, "/fields")? {
                anyhow::ensure!(
                    field["exists"] == true && field.get("value").is_some(),
                    "journal projection omitted a required assertion field: {field}"
                );
                action.insert(
                    expect::string(field, "/pointer")?
                        .trim_start_matches('/')
                        .into(),
                    field["value"].clone(),
                );
            }
            actions.push(Value::Object(action));
        }
        if page["next_cursor"].is_null() {
            break;
        }
        request["cursor"] = page["next_cursor"].clone();
    }
    actions.sort_by_key(|action| action["sequence"].as_u64());
    Ok(actions)
}
