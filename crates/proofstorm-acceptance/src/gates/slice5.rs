//! Focused successors to the former monolithic Slice 5 gate.
//! Each invocation owns a fresh cell. Recovery scenarios require an idle cluster.
use crate::{GateContext, McpClient, cell};
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

use common::{EXPERIMENT, INSTANCE};

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
    let mut cleanup = support::CellCleanup::new(context, workspace.clone());
    // Stop the MCP child before fallback cleanup, also on panic unwinding,
    // so it cannot admit work while its cell is being reclaimed.
    let mut client = context.default_session(&workspace, "experiment-agent")?;
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
    cleanup: &mut support::CellCleanup<'_>,
    workspace: &str,
    scenario: Scenario,
) -> Result<()> {
    println!("{}: composing isolated cell", scenario.name());
    let state = compose::compose(client, cleanup, scenario)?;
    if matches!(scenario, Scenario::Smoke) {
        compose::conformance(context, client, &state.namespace, workspace)?;
    }
    client.call(
        "run_start",
        json!({"request_id":"2182","run_id":EXPERIMENT,"name":INSTANCE}),
    )?;

    println!("{}: exercising scenario", scenario.name());
    let ns = &state.namespace;
    let key = &state.instance_key;
    let mut native_operations = Vec::new();
    match scenario {
        Scenario::Smoke => {
            native_operations = bootstrap::bootstrap(context, client, ns, key, false)?.1;
            native_operations.extend(smoke::run(client)?);
        }
        Scenario::Recovery => {
            native_operations = bootstrap::bootstrap(context, client, ns, key, true)?.1;
            recovery::run(context, client, ns, key)?;
            lifecycle::run(context, client, ns)?;
        }
        Scenario::Network => network::run(context, client, ns)?,
        Scenario::Channels => {
            let (point, operations) = bootstrap::bootstrap(context, client, ns, key, false)?;
            native_operations = operations;
            native_operations.extend(channels::run(client, &point)?);
        }
    }
    evidence::verify(context, client, &state, scenario, &native_operations)?;
    // Test the current close contract after exporting evidence. A fresh active
    // session cannot lease the cell, but a stale incarnation must still refuse.
    client.call(
        "run_start",
        json!({"request_id":"3259","run_id":"close-contract","name":INSTANCE}),
    )?;
    client.call_refused(
        "cell_remove",
        json!({"name":INSTANCE,"expected_instance_key":format!("{}-stale", state.instance_key)}),
        "stale_incarnation",
    )?;
    let still_open = crate::cell::status(client, INSTANCE)?;
    // Protocol observations may refresh after a restart. A refused close must
    // preserve the incarnation and never begin teardown, regardless of readiness.
    ensure!(
        still_open["instance_key"] == state.instance_key
            && !["closing", "closed", "cleanup_blocked"]
                .contains(&crate::json::string(&still_open, "/phase")?),
        "stale close changed the cell: {still_open}"
    );
    client.call(
        "cell_remove",
        json!({"name":INSTANCE,"expected_instance_key":state.instance_key}),
    )?;
    cell::wait_closed(client, INSTANCE)?;
    Ok(())
}
