//! Embedded CDK LDK: BOLT12 payment recognition, BOLT11 issuance and melting,
//! and payment/quote survival across mint and optional database restarts.
//!
//! Ported from `tests/kubernetes/cdk_ldk_mcp_client.py`. Serves both the
//! `cdk-ldk` and `cdk-ldk-postgres` targets; `PROOFSTORM_STORAGE` selects which.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, http, json as expect, native, postgres};

mod funding;

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
            {"id": "mint", "kind": "mint", "implementation": "cdk-ldk", "version": "0.18.1", "config_version": "cdk-mintd-ldk/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK LDK", "description": "Native CDK embedded-LDK BOLT12 cell"}},
            {"id":"wallet","kind":"wallet","implementation":"nutshell-wallet","version":"0.21.0","config_version":"nutshell-wallet/0.20/v1","control":"cell","config":{}}
        ],
        "links": [
            {"id": "peer-chain", "kind": "chain_backend", "from": "peer", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-chain", "kind": "chain_backend", "from": "mint", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}
        ],
        "policy": {"allow": ["component.exec_live"], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
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
    context.qualification_stage("materialize")?;
    let mut document = context.document(cell_document(postgres_enabled))?;
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
    let ready = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;
    if selected_version.starts_with("candidate-") {
        super::candidates::pod_image(context, namespace, selected_image)?;
    }
    context.record("cdk-ldk-selected-ready.json", &ready)?;

    context.qualification_stage("configuration")?;
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

    context.qualification_stage("version")?;
    let version =
        context
            .kubectl
            .exec(namespace, "deployment/mint", &["cdk-mintd", "--version"])?;
    let expected_version = if selected_version.starts_with("candidate-") {
        "0.18.1"
    } else {
        selected_version
    };
    if !version
        .split_whitespace()
        .any(|part| part == expected_version)
    {
        bail!("live mint reports the wrong version: {version:?}");
    }

    context.qualification_stage("peer-connect")?;
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
    context.qualification_stage("funding")?;
    if let Err(error) = fund_channel(context, &mut client, namespace, node_id) {
        if let Ok(logs) =
            context
                .kubectl
                .run(&["logs", "deployment/mint", "-n", namespace, "--tail=100"])
        {
            context.record("cdk-ldk-funding-mint-log.json", &json!(logs))?;
        }
        return Err(error);
    }

    context.qualification_stage("bolt12-quote")?;
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

    context.qualification_stage("bolt12-payment")?;
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
    context.qualification_stage("issuance")?;
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
    context.qualification_stage("swap-and-melt")?;
    session.nutshell_swap("wallet", "mint", "swap", 50)?;
    let balance = melt(&mut session, "before-restart")?;
    drop(session);
    context.qualification_stage("restart")?;
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
    context.qualification_stage("payment-after-restart")?;
    let mut session = native::Session::new(&mut client, INSTANCE, RUN);
    anyhow::ensure!(
        session.nutshell_balance("wallet", "mint", "persisted")? == balance,
        "wallet accounting changed through restart"
    );
    // Proofstorm starts CLN with --dev-no-reconnect. Re-establish the peer
    // connection explicitly after replacing the mint process.
    session.json(
        "peer",
        "reconnect-mint",
        &format!(
            "{CLN} connect {}",
            native::quote(&format!("{node_id}@mint:9735"))
        ),
    )?;
    session.poll(
        "peer",
        "channel-reconnected",
        &format!("{CLN} listpeerchannels"),
        |value| Ok(channel_has_payment_capacity(value, node_id).then_some(())),
    )?;
    melt(&mut session, "after-restart")?;
    drop(session);
    client.call("run_finish", json!({"run_id":RUN,"request_id":"finish"}))?;

    drop(forward);

    context.qualification_stage("teardown")?;
    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_closed(&mut client, INSTANCE)?;

    if postgres_enabled {
        println!("CDK embedded LDK + PostgreSQL MCP BOLT12 persistence and teardown passed");
    } else {
        println!(
            "CDK {selected_version} embedded-LDK MCP materialization, database-backed configuration, BOLT12 quote, readiness, and teardown passed"
        );
    }
    Ok(())
}

fn fund_channel(
    context: &GateContext,
    client: &mut crate::McpClient,
    namespace: &str,
    node_id: &str,
) -> Result<()> {
    let mut session = native::Session::new(client, INSTANCE, RUN);
    session.execute(
        "chain",
        "miner",
        &format!("{} createwallet default", native::BITCOIN_ROOT),
    )?;
    session.mine("chain", "mature", 110)?;
    // Inbound anchor channels require a separate on-chain emergency reserve in
    // LDK Node. Pushing a channel balance does not supply that reserve.
    let mut dashboard = funding::Dashboard::open(context, namespace)?;
    let ldk_address = dashboard.new_address()?;
    session.execute(
        "chain",
        "fund-ldk-reserve",
        &format!(
            "{} sendtoaddress {} 0.001",
            native::BITCOIN,
            native::quote(&ldk_address)
        ),
    )?;
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
    dashboard.wait_spendable(100_000)?;
    session.poll("peer", "confirmed", &format!("{CLN} listfunds"), |value| {
        Ok(value["outputs"]
            .as_array()
            .is_some_and(|outputs| outputs.iter().any(|output| output["status"] == "confirmed"))
            .then_some(()))
    })?;
    session.json(
        "peer",
        "fund-channel",
        // CLN requires the mint to retain 1% of channel capacity. Seed outbound
        // liquidity above that reserve before asking it to melt small amounts.
        &format!(
            "{CLN} -k fundchannel id={} amount=4000000sat push_msat=1000000000msat",
            native::quote(node_id)
        ),
    )?;
    session.mine("chain", "channel-confirm", 6)?;
    session.poll(
        "peer",
        "channel-ready",
        &format!("{CLN} listpeerchannels"),
        |value| Ok(channel_has_payment_capacity(value, node_id).then_some(())),
    )?;
    Ok(())
}

fn channel_has_payment_capacity(value: &Value, node_id: &str) -> bool {
    value["channels"].as_array().is_some_and(|channels| {
        channels.iter().any(|channel| {
            channel["peer_id"] == node_id
                && channel["state"] == "CHANNELD_NORMAL"
                && channel["peer_connected"] == true
                // These estimates already account for reserves. From CLN's
                // perspective, receivable capacity is the mint's outbound side.
                && channel["spendable_msat"]
                    .as_u64()
                    .is_some_and(|amount| amount >= 100_000_000)
                && channel["receivable_msat"]
                    .as_u64()
                    .is_some_and(|amount| amount >= 100_000_000)
        })
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{CellSpec, resolve_lock};
    use proofstorm_qualification::{Identity, Scenario};

    #[test]
    fn payment_channel_requires_liquidity_in_both_directions_to_the_selected_peer() {
        let ready = json!({"channels":[{
            "peer_id":"mint", "state":"CHANNELD_NORMAL", "peer_connected":true,
            "spendable_msat":2_900_000_000_u64, "receivable_msat":960_000_000
        }]});
        assert!(channel_has_payment_capacity(&ready, "mint"));
        assert!(!channel_has_payment_capacity(&ready, "another-mint"));
        assert!(!channel_has_payment_capacity(
            &json!({"channels":[]}),
            "mint"
        ));
        for (field, value) in [
            ("receivable_msat", json!(0)),
            ("receivable_msat", json!(2_100_000)),
            ("receivable_msat", Value::Null),
            ("spendable_msat", json!(0)),
            ("peer_connected", json!(false)),
            ("state", json!("CHANNELD_AWAITING_LOCKIN")),
        ] {
            let mut unavailable = ready.clone();
            unavailable["channels"][0][field] = value;
            assert!(
                !channel_has_payment_capacity(&unavailable, "mint"),
                "{field}"
            );
        }
    }

    #[test]
    fn embedded_ldk_fixtures_resolve_every_planned_storage_wallet_and_version() {
        let plan = proofstorm_qualification::plan(
            Identity {
                revision: "a".repeat(40),
                run_id: "0".into(),
                attempt: 1,
            },
            true,
        )
        .unwrap();
        let mut covered = std::collections::BTreeSet::new();
        for case in &plan.cases {
            let Scenario::Gate { name, versions } = &case.scenario else {
                continue;
            };
            if !matches!(name.as_str(), "cdk-ldk" | "cdk-ldk-postgres") {
                continue;
            }
            covered.insert((case.platform.as_str(), name.as_str()));
            let mut fixture = cell_document(name == "cdk-ldk-postgres");
            let observer = crate::qualification::Observer::new(case.clone());
            observer.document(&mut fixture).unwrap();
            observer.finish().unwrap();
            let cell: CellSpec = serde_json::from_value(fixture).unwrap();
            let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
            let lock = resolve_lock(&cell, &catalog).unwrap();
            for entry in lock.entries {
                assert_eq!(entry.version, versions[&entry.catalog_id]);
            }
        }
        assert_eq!(covered.len(), 4, "both storage variants on both platforms");
    }
}
