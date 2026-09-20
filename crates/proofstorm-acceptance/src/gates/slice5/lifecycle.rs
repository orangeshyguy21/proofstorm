use super::common::{INSTANCE, scoped};
use crate::{GateContext, McpClient, cell, json as expect};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

pub(super) fn run(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let kubectl = &context.kubectl;
    // --- node lifecycle -----------------------------------------------------
    let node = json!({"component": "payer-lnd"});
    let node_scoped = |operation: &str| scoped(operation, node.clone());

    let accepted = client.call("component_stop", node_scoped("payer-stop"))?;
    let retried = client.call("component_stop", node_scoped("payer-stop"))?;
    for field in ["id", "experiment_id", "kind", "sequence", "request_digest"] {
        ensure!(
            accepted.get(field).is_some() && accepted[field] == retried[field],
            "component stop retry changed {field}"
        );
    }
    let stopped = cell::wait_operation(client, "payer-stop", 120)?;
    if expect::string(cell::artifact_content(&stopped)?, "/state")? != "stopped" {
        bail!("node stop artifact is invalid: {stopped}");
    }
    let stateful = kubectl.get_json(&["get", "statefulset/payer-lnd", "-n", namespace])?;
    if expect::integer(&stateful, "/spec/replicas")? != 0 {
        bail!("stopped Lightning node did not retain zero desired replicas");
    }
    let mut degraded_ok = false;
    for _ in 0..60 {
        let stopped_cell = crate::cell::status(client, INSTANCE)?;
        let stopped_components = client.call(
            "cell_component_status_list",
            json!({"name": INSTANCE, "limit": 50}),
        )?;
        let payer = expect::array(&stopped_components, "/components")?
            .iter()
            .find(|component| component.get("id").and_then(Value::as_str) == Some("payer-lnd"))
            .ok_or_else(|| anyhow::anyhow!("payer-lnd is missing from component status"))?;
        if expect::string(&stopped_cell, "/phase")? == "ready" && !expect::boolean(payer, "/ready")?
        {
            degraded_ok = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !degraded_ok {
        bail!("intentionally stopped node corrupted cell readiness");
    }

    client.call("component_start", node_scoped("payer-start"))?;
    let started = cell::wait_operation(client, "payer-start", 120)?;
    if expect::string(cell::artifact_content(&started)?, "/state")? != "running" {
        bail!("node start artifact is invalid: {started}");
    }
    let pod_before = kubectl.run(&[
        "get",
        "pod/payer-lnd-0",
        "-n",
        namespace,
        "-o",
        "jsonpath={.metadata.uid}",
    ])?;
    client.call("component_restart", node_scoped("payer-restart"))?;
    let restarted = cell::wait_operation(client, "payer-restart", 120)?;
    if !expect::boolean(cell::artifact_content(&restarted)?, "/restarted")? {
        bail!("node restart artifact is invalid: {restarted}");
    }
    let pod_after = kubectl.run(&[
        "get",
        "pod/payer-lnd-0",
        "-n",
        namespace,
        "-o",
        "jsonpath={.metadata.uid}",
    ])?;
    if pod_after == pod_before {
        bail!("node restart completed without replacing the component pod");
    }
    cell::wait_ready(client, INSTANCE)?;

    Ok(())
}
