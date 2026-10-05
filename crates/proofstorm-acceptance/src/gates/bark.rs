//! Managed Bark qualification using the exact catalog images. The current
//! ARM64 preview requires verified images seeded into the owned local registry.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

use crate::{GateContext, McpClient, cell, http, json as expect, native};

mod funding;
mod payments;
mod recovery;
#[cfg(test)]
mod tests;
mod transport;

const INSTANCE: &str = "bark-instance";
const RUN: &str = "bark-payments";
const CLN: &str = "lightning-cli --notifications=none --lightning-dir=/data --network=regtest";
const PEER: &str =
    "lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest";
const ARK: &str = "captaind --config /usr/local/share/bark/captaind.default.toml rpc";
const WALLET: &str = "cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive";
const MINT: &str = "http://mint:3338";

fn document() -> Result<Value> {
    let mut document: Value = serde_json::from_str(include_str!(
        "../../../proofstorm-core/tests/fixtures/bark-topology.json"
    ))?;
    document["name"] = json!("bark-managed-qualification");
    // Isolate the processor's payment fees. Native wallet receipts include
    // Cashu input fees, which are outside the backend's melt fee reserve.
    let mint = document["components"]
        .as_array_mut()
        .context("components")?
        .iter_mut()
        .find(|component| component["id"] == "mint")
        .context("mint component")?;
    mint["config"]["input_fee_ppk"] = json!(0);
    document["components"].as_array_mut().context("components")?.extend([
        json!({"id":"peer","kind":"lightning","implementation":"cln","version":"26.06.7","config_version":"cln/26.06/v1","control":"cell","config":{}}),
        json!({"id":"wallet","kind":"wallet","implementation":"cdk-cli-wallet","version":"0.18.1","config_version":"cdk-cli-wallet/0.18/v1","control":"cell","config":{}}),
    ]);
    document["links"].as_array_mut().context("links")?.extend([
        json!({"id":"peer-chain","kind":"chain_backend","from":"peer","to":"chain","binding":{"type":"chain","network":"regtest"}}),
        json!({"id":"cln-peer","kind":"lightning_peer","from":"cln","to":"peer"}),
    ]);
    document["policy"] = json!({"allow":["component.exec_live","component.control"],"limits":{"max_components":8,"max_links":16,"max_config_bytes":65536}});
    Ok(document)
}

pub(crate) fn images(catalog: &proofstorm_core::CatalogResponse) -> Result<Vec<String>> {
    let spec = serde_json::from_value(document()?)?;
    let lock = proofstorm_core::resolve_lock(&spec, catalog).map_err(anyhow::Error::msg)?;
    Ok(lock.entries.into_iter().map(|entry| entry.image).collect())
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("bark-managed", "designer")?;
    let document = context.document(document()?)?;
    // Planning is deliberately ordinary catalog resolution. Missing image
    // entries must fail here, before any cell is materialized. Local preview
    // images are independently verified when seeding the owned registry.
    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":document,"request_id":"bark-plan"}),
    )?;
    let published = cell::review(&mut client, &preview)?;
    context.record("bark-plan.json", &published)?;
    for implementation in ["cdk-bark-processor", "bark-server", "cln-hold"] {
        let entry = cell::lock_entry(&published, implementation)?;
        ensure!(
            expect::string(entry, "/image")?.contains("@sha256:"),
            "Bark requires immutable catalog images"
        );
        ensure!(
            entry["build_provenance"].is_object(),
            "Bark requires source provenance"
        );
    }
    let result = (|| {
        cell::apply(&mut client, &preview)?;
        let ready = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
        context.record("bark-ready.json", &ready)?;
        let namespace = expect::string(&ready, "/instance_namespace")?;
        client.call(
            "run_start",
            json!({"name":INSTANCE,"run_id":RUN,"request_id":"bark-run"}),
        )?;
        exercise(context, &mut client, namespace)
    })();
    let _ = client.call(
        "run_finish",
        json!({"run_id":RUN,"request_id":"bark-finish"}),
    );
    let cleanup = cleanup(context, &mut client);
    context.record("bark-result.json", &json!({"passed":result.is_ok() && cleanup.is_ok(),"exercise_error":result.as_ref().err().map(|e|format!("{e:#}")),"cleanup_error":cleanup.as_ref().err().map(|e|format!("{e:#}"))}))?;
    cleanup?;
    result?;
    println!(
        "Managed Bark: settlement, unpaid and interrupted payment recovery, TLS refusal and cleanup passed"
    );
    Ok(())
}

fn cleanup(context: &GateContext, client: &mut McpClient) -> Result<()> {
    // A failed preliminary read must not prevent the removal attempt.
    let status = cell::status(client, INSTANCE);
    client.call("cell_remove", json!({"name":INSTANCE}))?;
    let closed = cell::wait_closed(client, INSTANCE)?;
    context.record("bark-cleanup.json", &closed)?;
    // This runner owns a disposable cluster. These reads never touch or demand
    // that unrelated Docker/Orchard installations be empty.
    context.kubectl.assert_no_instance_namespaces()?;
    context.kubectl.assert_no_cell_actions()?;
    let status = status.context("could not establish the namespace for storage verification")?;
    let namespace = expect::string(&status, "/instance_namespace")?;
    // PersistentVolumes outlive their namespace while reclamation is in
    // progress. Check only claims belonging to this removed instance.
    for _ in 0..60 {
        let volumes = context.kubectl.get_json(&["get", "persistentvolumes"])?;
        if !expect::array(&volumes, "/items")?
            .iter()
            .any(|v| v["spec"]["claimRef"]["namespace"] == namespace)
        {
            return context.record(
                "bark-storage-cleanup.json",
                &json!({"namespace":namespace,"remaining_volumes":0}),
            );
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("owned Bark volumes remain after namespace removal")
}

fn exercise(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    context.qualification_stage("bark-settings-and-transport")?;
    let settings = native::json_output(
        client,
        INSTANCE,
        RUN,
        "processor",
        "bark-settings",
        "/opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls cdk-bark-processor",
    )?;
    ensure!(
        settings["unit"] == "sat"
            && settings["bolt11"].is_object()
            && settings["bolt12"].is_null()
            && settings["onchain"].is_null()
            && settings["custom"].is_null(),
        "Bark capabilities differ from its BOLT11/sat profile"
    );
    context.record("bark-settings.json", &settings)?;
    transport::verify(context, namespace)?;
    context.qualification_stage("bark-funding")?;
    funding::run(context, client)?;
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        "bark-wallet-init",
        &format!("{WALLET} balance >/dev/null"),
    )?;
    balance(context, client, "bark-wallet-empty", 0)?;
    context.qualification_stage("bark-unpaid-restart")?;
    recovery::unpaid(context, client, namespace)?;
    context.qualification_stage("bark-interrupted-mint")?;
    let minted = payments::mint(context, client, namespace)?;
    context.qualification_stage("bark-melt")?;
    let remaining = payments::melt(context, client, namespace, "first", 30_000, 100_000)?;
    context.qualification_stage("bark-completed-restart")?;
    recovery::restart_stack(context, client, namespace, "paid")?;
    let issued = quote_state(context, namespace, "mint", &minted, "ISSUED")?;
    context.record("bark-issued-after-restart.json", &issued)?;
    balance(context, client, "bark-wallet-recovered", remaining)?;
    payments::melt(context, client, namespace, "recovered", 10_000, remaining)?;
    Ok(())
}

fn balance(context: &GateContext, client: &mut McpClient, id: &str, expected: u64) -> Result<()> {
    let state = native::observe_wallet(
        client,
        "cdk-cli-wallet",
        &json!({"name":INSTANCE,"run_id":RUN,"request_id":id,"wallet":"wallet","mint":"mint"}),
    )?;
    check_balance(&state, expected)?;
    context.record(&format!("{id}.json"), &state)
}

fn check_balance(state: &Value, expected: u64) -> Result<()> {
    ensure!(
        state["balance_sat"] == expected
            && state["reserved_sat"] == 0
            && state["pending_sat"] == 0
            && state["pending_spent_sat"] == 0,
        "wallet does not conserve settled funds"
    );
    Ok(())
}

fn control(client: &mut McpClient, tool: &str, component: &str, id: &str) -> Result<()> {
    client.call(
        tool,
        json!({"name":INSTANCE,"run_id":RUN,"request_id":id,"component":component}),
    )?;
    cell::wait_succeeded(client, id)?;
    Ok(())
}

fn quote_state(
    context: &GateContext,
    namespace: &str,
    kind: &str,
    id: &str,
    expected: &str,
) -> Result<Value> {
    quote_states(context, namespace, kind, id, &[expected])
}

fn quote_states(
    context: &GateContext,
    namespace: &str,
    kind: &str,
    id: &str,
    expected: &[&str],
) -> Result<Value> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    for _ in 0..90 {
        let state =
            http::get_json_retrying(&mut forward, &format!("/v1/{kind}/quote/bolt11/{id}"), 3)?;
        ensure!(state["quote"] == id, "quote identity changed");
        if expected.contains(&expect::string(&state, "/state")?) {
            return Ok(state);
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("original {kind} quote did not reach {expected:?}")
}
