//! Small installed-runtime acceptance gate: one Bitcoin cell through real MCP.
use crate::{GateContext, LIFECYCLE_CAPABILITIES, cell};
use anyhow::{Result, ensure};
use serde_json::json;

pub fn run(context: &GateContext) -> Result<()> {
    let mut client =
        context.session("acceptance-smoke", "smoke-designer", LIFECYCLE_CAPABILITIES)?;
    let mut spec: serde_json::Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json"))?;
    spec["name"] = json!("acceptance-smoke");
    spec["components"] = json!([spec["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["implementation"] == "bitcoin-core")
        .unwrap()
        .clone()]);
    spec["links"] = json!([]);
    client.call(
        "cell_create",
        json!({"draft_id":"smoke", "cell":spec,"idempotency_key":"smoke-create"}),
    )?;
    let published = client.call(
        "cell_publish",
        json!({"draft_id":"smoke","expected_version":1,"idempotency_key":"smoke-publish"}),
    )?;
    client.call("cell_materialize", json!({"instance_id":"smoke","revision_digest":published["digest"],"idempotency_key":"smoke-up"}))?;
    let status = cell::wait_ready(&mut client, "smoke")?;
    ensure!(status["phase"] == "ready", "smoke cell not ready");
    // A second, read-only identity cannot create cells; do not widen its grants.
    let mut reader = context.session(
        "acceptance-smoke",
        "smoke-reader",
        &["cell.read", "cell.status", "experiment.read"],
    )?;
    let read = reader.call("cell_status", json!({"instance_id":"smoke"}))?;
    ensure!(
        read["phase"] == "ready",
        "read-only identity did not read back the ready cell"
    );
    let cli = context.inspect_cli("acceptance-smoke", "smoke-reader", "smoke")?;
    ensure!(
        cli["runtime"]["phase"] == "ready",
        "CLI and MCP did not read the same ready cell"
    );
    reader.call_error(
        "cell_create",
        json!({"draft_id":"forbidden","cell":spec,"idempotency_key":"forbidden"}),
    )?;
    client.call("cell_close", json!({"instance_id":"smoke"}))?;
    cell::wait_closed(&mut client, "smoke")?;
    context.kubectl.assert_no_instance_namespaces()?;
    context.kubectl.assert_no_cell_actions()?;
    Ok(())
}
