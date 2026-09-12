use super::common::{INSTANCE, scoped, submit_idempotent};
use crate::{GateContext, McpClient, cell, json as expect};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

pub(super) fn run(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let kubectl = &context.kubectl;
    // --- node lifecycle -----------------------------------------------------
    let node = json!({"component": "payer-lnd"});
    let node_scoped = |operation: &str, key: &str| -> Value {
        let mut extra = node.clone();
        if let Some(target) = extra.as_object_mut() {
            target.insert("idempotency_key".into(), Value::from(key));
        }
        scoped(operation, extra)
    };

    submit_idempotent(
        client,
        "node_stop",
        node_scoped("payer-stop", "payer-stop-slice5"),
        "node stop",
    )?;
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
        let stopped_cell = client.call("cell_status", json!({"instance_id": INSTANCE}))?;
        let stopped_components = client.call(
            "cell_component_status_list",
            json!({"instance_id": INSTANCE, "limit": 50}),
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

    client.call(
        "node_start",
        node_scoped("payer-start", "payer-start-slice5"),
    )?;
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
    client.call(
        "node_restart",
        node_scoped("payer-restart", "payer-restart-slice5"),
    )?;
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

    Ok(())
}
