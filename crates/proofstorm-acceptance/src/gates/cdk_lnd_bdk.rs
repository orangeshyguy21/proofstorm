//! One CDK mint with a linked LND node and embedded BDK on-chain backend.
//! Verifies both payment sections render together, the mint advertises both
//! methods, the linked node issues BOLT11 mint quotes, and an on-chain deposit
//! settles, all through the single unified CDK catalog entry.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, http, json as expect};

const INSTANCE: &str = "cdk-lnd-bdk-instance";
const IMAGE: &str = proofstorm_core::CDK_MINT_IMAGE;
const PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-lnd-bdk-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-cdk-lnd-bdk"}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK LND + BDK", "embedded_onchain": "bdk"}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-chain", "kind": "chain_backend", "from": "mint", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-lightning", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

const CONFIG_FRAGMENTS: &[&str] = &[
    "[payment_backend]\nbackend = \"lnd\"",
    "[lnd]",
    "[onchain]\nonchain_backend = \"bdk\"",
    "[bdk]",
    "mnemonic = \"file:/mint-secrets/bdk-mnemonic\"",
];

fn bitcoin(context: &GateContext, namespace: &str, arguments: &[&str]) -> Result<String> {
    let mut argv = vec![
        "bitcoin-cli",
        "-regtest",
        "-rpcuser=proofstorm",
        "-rpcpassword=proofstorm-regtest-only",
    ];
    argv.extend_from_slice(arguments);
    context.kubectl.exec(namespace, "statefulset/chain", &argv)
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("cdk-lnd-bdk-live", "designer")?;
    let selected_version = context.selected_version("cdk", "0.18.1");
    let selected_image = context.selected_image("cdk", IMAGE);
    let mut document = context.document(cell_document())?;
    document["components"][2]["version"] = json!(selected_version);

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":document,"request_id":"create-cdk-lnd-bdk"}),
    )?;
    let published = cell::review(&mut client, &preview)?;
    let entry = cell::lock_entry(&published, "cdk")?;
    if expect::string(entry, "/version")? != selected_version
        || expect::string(entry, "/image")? != selected_image
    {
        bail!("unexpected CDK lock: {entry}");
    }
    context.record("cdk-lnd-bdk-plan.json", &published)?;

    cell::apply(&mut client, &preview)?;
    let ready = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let config = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in CONFIG_FRAGMENTS {
        if !config.contains(fragment) {
            bail!("mint configuration is missing {fragment:?}: {config}");
        }
    }

    bitcoin(context, namespace, &["createwallet", "default"])?;
    let miner = bitcoin(context, namespace, &["-rpcwallet=default", "getnewaddress"])?;
    bitcoin(
        context,
        namespace,
        &["-rpcwallet=default", "generatetoaddress", "101", &miner],
    )?;

    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let info = http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    let methods = expect::array(&info, "/nuts/4/methods")?
        .iter()
        .filter_map(|method| method.get("method").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if !methods.contains(&"bolt11") || !methods.contains(&"onchain") {
        bail!("mint does not advertise both linked BOLT11 and embedded on-chain minting: {info}");
    }

    let bolt11 = http::post_json(
        &forward.url("/v1/mint/quote/bolt11"),
        &json!({"amount": 1000, "unit": "sat"}),
    )?;
    if !expect::string(&bolt11, "/request")?.starts_with("lnbcrt") {
        bail!("linked LND did not issue a regtest BOLT11 mint quote: {bolt11}");
    }

    let onchain = http::post_json(
        &forward.url("/v1/mint/quote/onchain"),
        &json!({"unit": "sat", "pubkey": PUBKEY}),
    )?;
    let address = expect::string(&onchain, "/request")?;
    if !address.starts_with("bcrt1") {
        bail!("embedded BDK did not return a regtest deposit address: {onchain}");
    }
    bitcoin(
        context,
        namespace,
        &["-rpcwallet=default", "sendtoaddress", address, "0.00002000"],
    )?;
    bitcoin(
        context,
        namespace,
        &["-rpcwallet=default", "generatetoaddress", "1", &miner],
    )?;
    let status_url = forward.url(&format!(
        "/v1/mint/quote/onchain/{}",
        expect::string(&onchain, "/quote")?
    ));
    let mut settled = Value::Null;
    for _ in 0..60 {
        settled = http::get_json(&status_url)?;
        if settled
            .get("amount_paid")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            >= 2000
        {
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if settled
        .get("amount_paid")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        < 2000
    {
        bail!("on-chain deposit did not settle beside the linked Lightning backend: {settled}");
    }
    context.record(
        "cdk-lnd-bdk-quotes.json",
        &json!({"bolt11_quote": bolt11["quote"], "onchain": settled}),
    )?;
    drop(forward);

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_closed(&mut client, INSTANCE)?;
    println!("CDK linked LND + embedded BDK configuration, quotes, deposit and teardown passed");
    Ok(())
}
