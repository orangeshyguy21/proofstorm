use anyhow::{Result, ensure};
use serde_json::json;

use crate::{GateContext, cell, native};

pub(super) fn run(context: &GateContext) -> Result<()> {
    let instance = "qualification-workspace";
    let run = "workspace-persistence";
    let mut client = context.default_session(instance, "qualifier")?;
    let document = context.document(json!({
        "api_version":"proofstorm/v1alpha1", "name":instance,
        "components":[{"id":"workspace","kind":"attacker","implementation":"workspace",
            "version":"0.1.0-alpha.1","config_version":"workspace/v1","control":"cell","config":{}}],
        "links":[], "policy":{"allow":["component.exec_live","component.control"],
            "limits":{"max_components":4,"max_links":4,"max_config_bytes":16384}}
    }))?;
    let preview = client.call(
        "cell_plan",
        json!({"name":instance,"request_id":"create","cell":document}),
    )?;
    cell::review(&mut client, &preview)?;
    cell::apply(&mut client, &preview)?;
    cell::wait_ready(&mut client, instance)?;
    client.call(
        "run_start",
        json!({"name":instance,"run_id":run,"request_id":"run"}),
    )?;
    let mut session = native::Session::new(&mut client, instance, run);
    session.execute("workspace", "write", "set -eu; test \"$(pwd)\" = /workspace; printf 'qualification-persistent-state' > /workspace/qualification-state")?;
    drop(session);
    client.call(
        "component_restart",
        json!({"name":instance,"run_id":run,"request_id":"restart","component":"workspace"}),
    )?;
    cell::wait_operation(&mut client, "restart", 80)?;
    cell::wait_ready(&mut client, instance)?;
    let mut session = native::Session::new(&mut client, instance, run);
    let read = session.execute("workspace", "read", "cat /workspace/qualification-state")?;
    ensure!(
        read["stdout"] == "qualification-persistent-state",
        "workspace state did not survive restart"
    );
    drop(session);
    client.call("run_finish", json!({"run_id":run,"request_id":"finish"}))?;
    client.call("cell_remove", json!({"name":instance}))?;
    cell::wait_closed(&mut client, instance)?;
    Ok(())
}
