//! Re-link gate: a CDK mint mounts its own volume and its Lightning backend's.
//! Cell pods spread across nodes, so re-linking the mint to a Lightning node
//! on another node would need volumes on two nodes. The controller must hold
//! that edit back: the running mint keeps its pod, the cell reports a
//! `pod_unschedulable` blocker, and reverting the link clears it.
//!
//! Needs a cluster with more than one schedulable node; run it with
//! `--cluster-agents 2` so the Lightning nodes can land apart.
use super::{measure, prober};
use crate::{GateContext, McpClient, json as expect};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, thread, time::Duration};

const NAME: &str = "relink-cell";
const WORKSPACE: &str = "acceptance-relink";
const PRINCIPAL: &str = "relink-designer";
const LIGHTNING: usize = 3;

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session(WORKSPACE, PRINCIPAL)?;
    let developer: Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json"))?;
    let chain = developer["components"][0].clone();
    let mut mint = developer["components"][1].clone();
    mint["config"] = json!({});
    let lnd = prober::preferred(&mut client, "lnd")?;
    let mut components = vec![chain, mint];
    let mut links = vec![payment_link("ln-0")];
    for index in 0..LIGHTNING {
        let id = format!("ln-{index}");
        components.push(prober::component(&lnd, "lnd", &id));
        links.push(
            json!({"id":format!("{id}-chain"),"kind":"chain_backend","from":id,
            "to":"chain","binding":{"type":"chain","network":"regtest"}}),
        );
    }
    let spec = json!({"api_version":"proofstorm/v1alpha1","name":NAME,"components":components,"links":links});
    let preview = client.call(
        "cell_plan",
        json!({"name":NAME,"request_id":"relink-create","cell":spec}),
    )?;
    crate::cell::apply(&mut client, &preview)?;
    measure::wait_ready(&mut client, NAME, None, 15)?;
    let status = crate::cell::status(&mut client, NAME)?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_owned();
    let placed = pods(context, &namespace)?;
    let (mint_node, mint_pod) = placed.get("mint").context("mint pod missing")?.clone();
    let target = (1..LIGHTNING)
        .map(|index| format!("ln-{index}"))
        .find(|id| placed.get(id).is_some_and(|(node, _)| *node != mint_node))
        .with_context(|| {
            format!("every Lightning node landed on the mint's node {mint_node}: {placed:?}")
        })?;
    eprintln!("relink: mint on {mint_node}; moving its backend to {target}");

    let generation = relink(&mut client, "relink-move", "ln-0", &target)?;
    let waited = wait_blocked(&mut client, generation)?;
    let blocker = expect::array(&waited, "/blockers")?
        .iter()
        .find(|blocker| blocker["component_id"] == "mint")
        .with_context(|| format!("no blocker for the mint: {waited}"))?;
    ensure!(
        blocker["reason"] == "pod_unschedulable"
            && expect::string(blocker, "/message")?.contains("different nodes"),
        "unexpected mint blocker: {blocker}"
    );
    // A held edit leaves the running pod alone.
    thread::sleep(Duration::from_secs(10));
    let held = pods(context, &namespace)?;
    ensure!(
        held.get("mint") == Some(&(mint_node.clone(), mint_pod.clone())),
        "the held edit replaced the mint pod: {held:?}"
    );

    let generation = relink(&mut client, "relink-back", &target, "ln-0")?;
    measure::wait_ready(&mut client, NAME, Some(generation), 5)?;
    let reverted = pods(context, &namespace)?;
    ensure!(
        reverted.get("mint") == Some(&(mint_node, mint_pod)),
        "reverting the link replaced the mint pod: {reverted:?}"
    );

    client.call("cell_remove", json!({"name":NAME}))?;
    crate::cell::wait_closed(&mut client, NAME)?;
    context.kubectl.assert_no_instance_namespaces()
}

fn payment_link(to: &str) -> Value {
    json!({"id":format!("mint-{to}"),"kind":"payment_backend","from":"mint","to":to,
        "binding":{"type":"payment","method":"bolt11","unit":"sat"}})
}

/// Point the mint's payment backend from `from` to `to`; the new generation.
fn relink(client: &mut McpClient, request: &str, from: &str, to: &str) -> Result<u64> {
    let inspected = client.call("cell_inspect", json!({"name":NAME}))?;
    let generation = expect::integer(&inspected, "/desired_generation")?;
    let up = client.call(
        "cell_up",
        json!({"name":NAME,"request_id":request,"expected_generation":generation,
            "expected_instance_key":inspected["instance_key"],
            "patch":[{"op":"remove_link","id":format!("mint-{from}")},
                {"op":"add_link","link":{"id":format!("mint-{to}"),"kind":"payment_backend",
                    "from":"mint","to":to,"method":"bolt11","unit":"sat"}}]}),
    )?;
    ensure!(up["accepted"] == true, "relink was not accepted: {up}");
    Ok(generation + 1)
}

/// The ready wait's answer once it reports blockers rather than timing out.
fn wait_blocked(client: &mut McpClient, generation: u64) -> Result<Value> {
    for _ in 0..5 {
        let waited = client.call(
            "cell_wait",
            json!({"name":NAME,"target_phase":"ready","timeout_seconds":60,
                "expected_generation":generation}),
        )?;
        ensure!(
            waited["reached"] != true,
            "the cell became ready with volumes on different nodes: {waited}"
        );
        if waited["timed_out"] != true {
            return Ok(waited);
        }
    }
    bail!("the held edit never reported a blocker")
}

/// Each component's running pod: its node and UID.
fn pods(context: &GateContext, namespace: &str) -> Result<BTreeMap<String, (String, String)>> {
    let pods = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        "proofstorm.dev/component",
    ])?;
    let mut placed = BTreeMap::new();
    for pod in expect::array(&pods, "/items")? {
        if pod["metadata"].get("deletionTimestamp").is_some() {
            continue;
        }
        placed.insert(
            expect::string(pod, "/metadata/labels/proofstorm.dev~1component")?.to_owned(),
            (
                pod["spec"]["nodeName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                expect::string(pod, "/metadata/uid")?.to_owned(),
            ),
        );
    }
    Ok(placed)
}
