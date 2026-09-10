//! Focused successors to the former monolithic Slice 5 gate.
//! Each invocation owns a fresh lab. Recovery scenarios require an idle cluster.
use crate::{GateContext, McpClient, lab};
use anyhow::{Result, ensure};
use serde_json::json;

mod bootstrap;
mod channels;
mod common;
mod compose;
mod evidence;
mod lifecycle;
mod network;
mod recovery;
mod smoke;
mod support;

use common::{CAPABILITIES, EXPERIMENT, INSTANCE, SESSION};

#[derive(Clone, Copy, Debug)]
pub enum Scenario {
    Smoke,
    Recovery,
    Network,
    Channels,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::Smoke => "slice5",
            Self::Recovery => "controller-recovery",
            Self::Network => "network-faults",
            Self::Channels => "channel-lifecycle",
        }
    }
}

pub fn run(context: &GateContext, scenario: Scenario) -> Result<()> {
    support::preflight(context)?;
    let workspace = format!("{}-{}", scenario.name(), context.run_id);
    let mut cleanup = support::LabCleanup::new(context, workspace.clone(), INSTANCE);
    // Stop the MCP child before fallback cleanup, also on panic unwinding,
    // so it cannot admit work while its lab is being reclaimed.
    let mut client = context.session(&workspace, "experiment-agent", CAPABILITIES)?;
    let result = exercise(context, &mut client, &mut cleanup, &workspace, scenario);
    if let Err(error) = &result {
        eprintln!(
            "{} failed; running scoped cleanup: {error:#}",
            scenario.name()
        );
    }
    drop(client);
    support::with_cleanup(|| result, || cleanup.finish())?;
    println!(
        "{} passed, including evidence and verified teardown",
        scenario.name()
    );
    Ok(())
}

fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    cleanup: &mut support::LabCleanup<'_>,
    workspace: &str,
    scenario: Scenario,
) -> Result<()> {
    println!("{}: composing isolated lab", scenario.name());
    let state = compose::compose(client, cleanup, scenario)?;
    if matches!(scenario, Scenario::Smoke) {
        compose::conformance(context, client, &state.namespace, workspace)?;
    }
    client.call(
        "experiment_create",
        json!({"experiment_id":EXPERIMENT,"instance_id":INSTANCE,"idempotency_key":"create-run"}),
    )?;
    client.call(
        "session_start",
        json!({"experiment_id":EXPERIMENT,"session_id":SESSION,"idempotency_key":"start-session"}),
    )?;
    println!("{}: exercising scenario", scenario.name());
    let ns = &state.namespace;
    let key = &state.instance_key;
    match scenario {
        Scenario::Smoke => {
            bootstrap::bootstrap(context, client, ns, key, false)?;
            smoke::run(context, client, ns, key)?;
        }
        Scenario::Recovery => {
            bootstrap::bootstrap(context, client, ns, key, true)?;
            recovery::run(context, client, ns, key)?;
            lifecycle::run(context, client, ns)?;
        }
        Scenario::Network => network::run(context, client, ns)?,
        Scenario::Channels => {
            let channel = bootstrap::bootstrap(context, client, ns, key, false)?;
            channels::run(client, &channel)?;
        }
    }
    evidence::verify(context, client, &state, scenario)?;
    // Test the current close contract after exporting evidence. A fresh active
    // session cannot lease the lab, but a stale incarnation must still refuse.
    client.call("experiment_create", json!({"experiment_id":"close-contract","instance_id":INSTANCE,"idempotency_key":"close-contract"}))?;
    let session = client.call("session_start", json!({"experiment_id":"close-contract","session_id":"active-at-close","idempotency_key":"active-at-close"}))?;
    ensure!(
        session["phase"] == "active",
        "close-contract session is not active"
    );
    client.call_refused("lab_close", json!({"instance_id":INSTANCE,"expected_instance_key":format!("{}-stale", state.instance_key)}), "stale_incarnation")?;
    let still_open = client.call("lab_status", json!({"instance_id":INSTANCE}))?;
    ensure!(
        still_open["instance_key"] == state.instance_key && still_open["phase"] == "ready",
        "stale close changed the lab"
    );
    client.call(
        "lab_close",
        json!({"instance_id":INSTANCE,"expected_instance_key":state.instance_key}),
    )?;
    lab::wait_closed(client, INSTANCE)?;
    Ok(())
}
