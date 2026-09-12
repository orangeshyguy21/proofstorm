use super::common::{action_kinds, assert_handle, scoped};
use crate::{GateContext, McpClient, cell, gate::CONTROL_NAMESPACE, json as expect};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

pub(super) fn bootstrap(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    instance_key: &str,
    restart_controller: bool,
) -> Result<String> {
    let kubectl = &context.kubectl;
    // --- bootstrap survives a caller retry and a controller restart --------
    let bootstrap_request = scoped(
        "bootstrap",
        json!({
            "chain": "chain", "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
            "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000,
            "idempotency_key": "bootstrap-slice5"
        }),
    );
    let accepted_bootstrap = client.call("liquidity_bootstrap", bootstrap_request.clone())?;
    let mut items = Value::Null;
    let mut created = false;
    for _ in 0..30 {
        items = action_kinds(context, instance_key)?;
        if !expect::array(&items, "/items")?.is_empty() {
            created = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !created {
        bail!("controller-owned ProofstormCellAction was not created");
    }
    let entries = expect::array(&items, "/items")?;
    if entries.len() != 1
        || expect::string(&entries[0], "/spec/action/kind")? != "bootstrap_liquidity"
    {
        bail!("unexpected typed runtime action: {items}");
    }
    let retried_bootstrap = client.call("liquidity_bootstrap", bootstrap_request)?;
    if expect::string(&retried_bootstrap, "/resource_name")?
        != expect::string(&accepted_bootstrap, "/resource_name")?
        || expect::integer(&retried_bootstrap, "/sequence")?
            != expect::integer(&accepted_bootstrap, "/sequence")?
    {
        bail!("caller retry changed the accepted action identity");
    }
    if restart_controller {
        kubectl.rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    }
    let jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        namespace,
        "-l",
        "proofstorm.dev/action",
    ])?;
    if expect::array(&jobs, "/items")?.len() != 1 {
        bail!("caller retry or controller restart duplicated the bootstrap Job");
    }
    let bootstrap = cell::wait_operation(client, "bootstrap", 120)?;
    let bootstrap_content = cell::artifact_content(&bootstrap)?;
    if !expect::boolean(bootstrap_content, "/ready")? {
        bail!("bootstrap artifact is invalid: {bootstrap}");
    }
    let bootstrap_channel_id = assert_handle(bootstrap_content, "bootstrap")?;

    Ok(bootstrap_channel_id)
}
