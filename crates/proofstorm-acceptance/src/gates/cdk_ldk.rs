//! CDK 0.18.1 embedded LDK: BOLT12 offer quoting through the mint's own HTTP
//! API, an inbound CLN peer connection to the embedded node, and, on the
//! PostgreSQL variant, quote survival across database and mint restarts.
//!
//! Ported from `tests/kubernetes/cdk_ldk_mcp_client.py`. Serves both the
//! `cdk-ldk` and `cdk-ldk-postgres` targets; `PROOFSTORM_STORAGE` selects which.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, http, json as expect, native, postgres};

const INSTANCE: &str = "cdk-ldk-instance";
const DATABASE: &str = "proofstorm_ldk";
const MARKER: &str = "ldk-persistent";
const RUN: &str = "embedded-ldk-payments";
const CLN: &str =
    "lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest";
const IMAGE: &str = proofstorm_core::CDK_MINT_IMAGE;

fn cell_document(postgres_enabled: bool) -> Value {
    let mut cell = json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-ldk-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "peer", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "cell", "config": {"alias": "proofstorm-ldk-introduction-peer"}},
            {"id": "mint", "kind": "mint", "implementation": "cdk-ldk", "version": "0.18.1", "config_version": "cdk-mintd-ldk/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK LDK", "description": "Native CDK embedded-LDK BOLT12 cell"}}
        ],
        "links": [
            {"id": "peer-chain", "kind": "chain_backend", "from": "peer", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-chain", "kind": "chain_backend", "from": "mint", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    });
    postgres::augment_cell(postgres_enabled, &mut cell, DATABASE);
    cell
}

/// Pull the embedded node's public key out of the mint's bounded startup logs.
fn ldk_node_id(logs: &str) -> Option<&str> {
    let marker = "Created node ";
    let start = logs.find(marker)? + marker.len();
    let candidate = logs.get(start..start + 66)?;
    candidate
        .chars()
        .all(|character| character.is_ascii_hexdigit())
        .then_some(candidate)
}

pub fn run(context: &GateContext, postgres_enabled: bool) -> Result<()> {
    let client = context.default_session("cdk-ldk-live", "designer")?;
    run_selected(
        context,
        client,
        postgres_enabled,
        context.selected_version("cdk-ldk", "0.18.1"),
        context.selected_image("cdk-ldk", IMAGE),
    )
}

pub(super) fn run_candidate(
    context: &GateContext,
    client: crate::McpClient,
    receipt: &Value,
) -> Result<()> {
    run_selected(
        context,
        client,
        false,
        expect::string(receipt, "/catalog_entry/version")?,
        expect::string(receipt, "/image")?,
    )
}

fn run_selected(
    context: &GateContext,
    mut client: crate::McpClient,
    postgres_enabled: bool,
    selected_version: &str,
    selected_image: &str,
) -> Result<()> {
    let mut document = cell_document(postgres_enabled);
    document["components"].as_array_mut().unwrap().push(json!({"id":"wallet","kind":"wallet","implementation":"nutshell-wallet","version":"0.21.0","config_version":"nutshell-wallet/0.20/v1","control":"cell","config":{}}));
    document["policy"]["allow"] = json!(["component.exec_live"]);
    let mut document = context.document(document)?;
    document["components"][2]["version"] = json!(selected_version);

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":document,"request_id":"create-cdk-ldk"}),
    )?;
    let published = crate::cell::review(&mut client, &preview)?;
    let entry = cell::lock_entry(&published, "cdk-ldk")?;
    expect::equals(entry, "/version", &Value::from(selected_version))?;
    expect::equals(entry, "/image", &Value::from(selected_image))?;
    context.record("cdk-ldk-selected-plan.json", &published)?;

    crate::cell::apply(&mut client, &preview)?;
    let ready = cell::wait_ready(&mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;
    if selected_version.starts_with("candidate-") {
        super::candidates::pod_image(context, namespace, selected_image)?;
    }
    context.record("cdk-ldk-selected-ready.json", &ready)?;

    let config = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in [
        "[payment_backend]\nbackend = \"ldk-node\"",
        "chain_source_type = \"bitcoinrpc\"",
        "bitcoind_rpc_host = \"chain\"",
        "ldk_node_host = \"0.0.0.0\"",
        "ldk_node_port = 9735",
    ] {
        if !config.contains(fragment) {
            bail!("mint configuration is missing {fragment:?}: {config}");
        }
    }
    if config.contains("[lnd]") || config.contains("[cln]") {
        bail!("embedded-LDK cell rendered an external Lightning stanza");
    }
    postgres::assert_materialized(
        postgres_enabled,
        &context.kubectl,
        namespace,
        &config,
        DATABASE,
    )?;

    let version =
        context
            .kubectl
            .exec(namespace, "deployment/mint", &["cdk-mintd", "--version"])?;
    if !version.contains("0.18.1") {
        bail!("live mint reports the wrong version: {version:?}");
    }

    let logs = context
        .kubectl
        .run(&["logs", "deployment/mint", "-n", namespace])?;
    let node_id = ldk_node_id(&logs).ok_or_else(|| {
        anyhow::anyhow!("could not discover embedded LDK node identity from bounded startup logs")
    })?;
    context.kubectl.exec(
        namespace,
        "statefulset/peer",
        &[
            "lightning-cli",
            "--lightning-dir=/home/cln/.lightning",
            "--network=regtest",
            "connect",
            &format!("{node_id}@mint:9735"),
        ],
    )?;
    client.call(
        "run_start",
        json!({"name":INSTANCE,"run_id":RUN,"request_id":"run"}),
    )?;
    fund_channel(&mut client, node_id)?;

    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let info = http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    if !serde_json::to_string(&info)?
        .to_lowercase()
        .contains("bolt12")
    {
        bail!("live mint does not advertise BOLT12: {info}");
    }

    let quote = http::post_json(
        &forward.url("/v1/mint/quote/bolt12"),
        &json!({
            "amount": 100,
            "unit": "sat",
            "description": "Proofstorm BOLT12 acceptance",
            "pubkey": "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        }),
    )?;
    if !expect::string(&quote, "/request")?
        .to_lowercase()
        .starts_with("lno")
    {
        bail!("BOLT12 quote did not return an offer: {quote}");
    }
    if expect::string(&quote, "/unit")? != "sat" || expect::integer(&quote, "/amount")? != 100 {
        bail!("BOLT12 quote returned unexpected terms: {quote}");
    }
    context.record("cdk-ldk-selected-quote.json", &quote)?;

    let quote_id = expect::string(&quote, "/quote")?.to_owned();
    let mut session = native::Session::new(&mut client, INSTANCE, RUN);
    let invoice = session.json(
        "peer",
        "fetch-offer",
        &format!(
            "{CLN} fetchinvoice {}",
            native::quote(expect::string(&quote, "/request")?)
        ),
    )?;
    let payment = session.json(
        "peer",
        "pay-offer",
        &format!(
            "{CLN} pay {}",
            native::quote(expect::string(&invoice, "/invoice")?)
        ),
    )?;
    anyhow::ensure!(
        payment["status"] == "complete",
        "BOLT12 payment did not settle"
    );
    let mut credited = false;
    for _ in 0..60 {
        let paid = http::get_json_retrying(
            &mut forward,
            &format!("/v1/mint/quote/bolt12/{quote_id}"),
            3,
        )?;
        if paid["amount_paid"]
            .as_u64()
            .is_some_and(|value| value >= 100)
        {
            credited = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::ensure!(
        credited,
        "original BOLT12 quote did not recognize the settled payment"
    );
    session.nutshell_initialize("wallet", "mint", "initialize")?;
    let wallet_quote = session.nutshell_invoice("wallet", "mint", "wallet-quote", 2000)?;
    let invoice = session.nutshell_invoice_projection(
        "wallet",
        "mint",
        "wallet-invoice",
        &wallet_quote,
        2000,
    )?;
    let paid = session.json(
        "peer",
        "wallet-pay",
        &format!(
            "{CLN} pay {}",
            native::quote(expect::string(&invoice, "/payment_request")?)
        ),
    )?;
    anyhow::ensure!(
        paid["status"] == "complete",
        "BOLT11 funding did not settle"
    );
    session.nutshell_claim("wallet", "mint", "wallet-claim", &wallet_quote, 2000)?;
    anyhow::ensure!(
        session.nutshell_balance("wallet", "mint", "funded")? == 2000,
        "incorrect ecash issuance"
    );
    session.nutshell_swap("wallet", "mint", "swap", 50)?;
    let balance = melt(&mut session, "before-restart")?;
    drop(session);
    postgres::seed_sentinel(postgres_enabled, &context.kubectl, namespace, MARKER)?;
    postgres::restart_database(postgres_enabled, &context.kubectl, namespace)?;
    context
        .kubectl
        .rollout_restart(namespace, "deployment/mint")?;
    postgres::verify_sentinel(postgres_enabled, &context.kubectl, namespace, MARKER)?;
    drop(forward);
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let recovered = http::get_json_retrying(
        &mut forward,
        &format!("/v1/mint/quote/bolt12/{quote_id}"),
        30,
    )?;
    anyhow::ensure!(
        recovered["quote"] == quote_id
            && recovered["amount_paid"]
                .as_u64()
                .is_some_and(|value| value >= 100),
        "paid BOLT12 quote did not survive restart"
    );
    let logs_after = context
        .kubectl
        .run(&["logs", "deployment/mint", "-n", namespace])?;
    anyhow::ensure!(
        ldk_node_id(&logs_after) == Some(node_id),
        "embedded node identity changed"
    );
    let mut session = native::Session::new(&mut client, INSTANCE, RUN);
    anyhow::ensure!(
        session.nutshell_balance("wallet", "mint", "persisted")? == balance,
        "wallet accounting changed through restart"
    );
    session.poll(
        "peer",
        "channel-reconnected",
        &format!("{CLN} listpeerchannels"),
        |value| {
            Ok(value["channels"]
                .as_array()
                .is_some_and(|channels| {
                    channels.iter().any(|channel| {
                        channel["state"] == "CHANNELD_NORMAL" && channel["peer_connected"] == true
                    })
                })
                .then_some(()))
        },
    )?;
    melt(&mut session, "after-restart")?;
    drop(session);
    client.call("run_finish", json!({"run_id":RUN,"request_id":"finish"}))?;

    drop(forward);

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_closed(&mut client, INSTANCE)?;

    if postgres_enabled {
        println!("CDK embedded LDK + PostgreSQL MCP BOLT12 persistence and teardown passed");
    } else {
        println!(
            "CDK 0.18.1 embedded-LDK MCP materialization, database-backed configuration, BOLT12 quote, readiness, and teardown passed"
        );
    }
    Ok(())
}

fn fund_channel(client: &mut crate::McpClient, node_id: &str) -> Result<()> {
    let mut session = native::Session::new(client, INSTANCE, RUN);
    session.execute(
        "chain",
        "miner",
        &format!("{} createwallet default", native::BITCOIN_ROOT),
    )?;
    session.mine("chain", "mature", 110)?;
    let address = session.json("peer", "address", &format!("{CLN} newaddr bech32"))?;
    session.execute(
        "chain",
        "fund-peer",
        &format!(
            "{} sendtoaddress {} 0.1",
            native::BITCOIN,
            native::quote(expect::string(&address, "/bech32")?)
        ),
    )?;
    session.mine("chain", "fund-confirm", 6)?;
    session.poll("peer", "confirmed", &format!("{CLN} listfunds"), |value| {
        Ok(value["outputs"]
            .as_array()
            .is_some_and(|outputs| outputs.iter().any(|output| output["status"] == "confirmed"))
            .then_some(()))
    })?;
    session.json(
        "peer",
        "fund-channel",
        &format!("{CLN} fundchannel {} 4000000", native::quote(node_id)),
    )?;
    session.mine("chain", "channel-confirm", 6)?;
    session.poll(
        "peer",
        "channel-ready",
        &format!("{CLN} listpeerchannels"),
        |value| {
            Ok(value["channels"]
                .as_array()
                .is_some_and(|channels| {
                    channels
                        .iter()
                        .any(|channel| channel["state"] == "CHANNELD_NORMAL")
                })
                .then_some(()))
        },
    )?;
    Ok(())
}

fn melt(session: &mut native::Session<'_>, id: &str) -> Result<u64> {
    let before = session.nutshell_balance("wallet", "mint", &format!("{id}-balance"))?;
    let invoice = session.json(
        "peer",
        &format!("{id}-invoice"),
        &format!("{CLN} invoice 100000 {} qualification", native::quote(id)),
    )?;
    let paid = session.nutshell_melt(
        "wallet",
        "mint",
        &format!("{id}-melt"),
        expect::string(&invoice, "/bolt11")?,
        100,
    )?;
    anyhow::ensure!(paid["state"] == "PAID", "wallet melt did not reach PAID");
    let recipient = session.json(
        "peer",
        &format!("{id}-recipient"),
        &format!("{CLN} listinvoices {}", native::quote(id)),
    )?;
    anyhow::ensure!(
        recipient["invoices"][0]["status"] == "paid"
            && recipient["invoices"][0]["amount_received_msat"] == 100_000,
        "independent recipient settlement differs"
    );
    let after = session.nutshell_balance("wallet", "mint", &format!("{id}-after"))?;
    anyhow::ensure!(
        before
            .checked_sub(after)
            .is_some_and(|debit| (100..=150).contains(&debit)),
        "unexpected wallet debit"
    );
    Ok(after)
}
