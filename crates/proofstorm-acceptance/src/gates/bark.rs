//! Managed Bark qualification using the exact published native catalog images.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

use super::stack::{
    self, Stack, quote_state, quote_states,
    transport::{Credentials, Target},
};
use crate::{GateContext, McpClient, cell, http, json as expect, native};

mod funding;
mod onchain;
mod payments;
mod recovery;
#[cfg(test)]
mod tests;

const INSTANCE: &str = "bark-instance";
const RUN: &str = "bark-payments";
const CLN: &str = "lightning-cli --notifications=none --lightning-dir=/data --network=regtest";
const PEER: &str =
    "lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest";
const ARK: &str = "captaind --config /usr/local/share/bark/captaind.default.toml rpc";
const WALLET: &str = "cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive";
const MINT: &str = "http://mint:3338";
const STACK: Stack = Stack {
    instance: INSTANCE,
    run: RUN,
    prefix: "bark",
    name: "Bark",
};

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
        json!({"id":"peer","kind":"lightning","implementation":"cln","version": proofstorm_core::CLN_VERSION,"config_version":"cln/26.06/v1","control":"cell","config":{}}),
        json!({"id":"wallet","kind":"wallet","implementation":"cdk-cli-wallet","version": proofstorm_core::CDK_VERSION,"config_version":"cdk-cli-wallet/0.18/v1","control":"cell","config":{}}),
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
    STACK.run(
        context,
        document()?,
        &["cdk-bark-processor", "bark-server", "cln-hold"],
        |client, namespace| exercise(context, client, namespace),
    )?;
    println!(
        "Managed Bark: rail registration, settlement, unpaid and interrupted payment recovery, on-chain boarding, TLS refusal and cleanup passed"
    );
    Ok(())
}

/// Bark's three mutually authenticated gRPC services, each probed with
/// another service's client identity.
fn transport_targets() -> [Target; 3] {
    [
        Target::service(
            "processor",
            "processor",
            50051,
            "/cdk_payment_processor.CdkPaymentProcessor/GetSettings",
            Credentials::mutual("processor-payment-tls"),
            Credentials::mutual("cln-cln-tls"),
        ),
        Target::service(
            "cln",
            "cln",
            9988,
            "/cln.Node/Getinfo",
            Credentials::mutual("cln-cln-tls"),
            Credentials::mutual("cln-hold-tls"),
        ),
        Target::service(
            "hold",
            "cln",
            9292,
            "/hold.Hold/GetInfo",
            Credentials::mutual("cln-hold-tls"),
            Credentials::mutual("cln-cln-tls"),
        ),
    ]
}

fn exercise(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    context.qualification_stage("bark-settings-and-transport")?;
    let settings = native::json_output(
        client,
        INSTANCE,
        RUN,
        "processor",
        "bark-settings",
        &format!(
            "/opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls cdk-bark-processor {}",
            onchain::METHODS
        ),
    )?;
    context.record("bark-settings.json", &settings)?;
    onchain::check_settings(&settings)?;
    STACK.registered(context, namespace, onchain::METHODS)?;
    stack::transport::verify(context, namespace, STACK.prefix, &transport_targets())?;
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
    context.qualification_stage("bark-onchain-mint")?;
    onchain::mint(context, client, namespace)
}

fn balance(context: &GateContext, client: &mut McpClient, id: &str, expected: u64) -> Result<()> {
    STACK.balance(context, client, id, expected)
}

fn control(client: &mut McpClient, tool: &str, component: &str, id: &str) -> Result<()> {
    STACK.control(client, tool, component, id)
}
