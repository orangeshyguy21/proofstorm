//! Small installed-runtime acceptance gate: one Bitcoin cell through real MCP.
use crate::{GateContext, cell};
use anyhow::{Result, ensure};
use serde_json::json;

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("acceptance-smoke", "smoke-designer")?;
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
    let preview = client.call(
        "cell_plan",
        json!({"name":"smoke","cell":spec,"request_id":"smoke-create"}),
    )?;
    crate::cell::review(&mut client, &preview)?;
    crate::cell::apply(&mut client, &preview)?;
    let status = cell::wait_ready(&mut client, "smoke")?;
    ensure!(status["phase"] == "ready", "smoke cell not ready");
    // A second, read-only identity cannot create cells; do not widen its grants.
    let mut reader = context.session(
        "acceptance-smoke",
        "smoke-reader",
        &["cell.read", "cell.status", "experiment.read"],
    )?;
    let read = crate::cell::status(&mut reader, "smoke")?;
    ensure!(
        read["phase"] == "ready",
        "read-only identity did not read back the ready cell"
    );
    let cli = context.inspect_cli("acceptance-smoke", "smoke-reader", "smoke")?;
    ensure!(
        cli["runtime"]["phase"] == "ready",
        "CLI and MCP did not read the same ready cell"
    );
    let denied = reader.call_error(
        "cell_up",
        json!({"name":"forbidden","request_id":"denied-create","cell":spec}),
    )?;
    ensure!(
        denied["data"]["code"] == "access_denied",
        "restricted caller bypassed creation authority: {denied}"
    );
    client.call("cell_remove", json!({"name":"smoke"}))?;
    cell::wait_closed(&mut client, "smoke")?;
    context.kubectl.assert_no_instance_namespaces()?;
    context.kubectl.assert_no_cell_actions()?;
    Ok(())
}
