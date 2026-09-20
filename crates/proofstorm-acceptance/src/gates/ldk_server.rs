//! Native regtest payments through CDK's separate, mutually authenticated processor.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

use crate::{GateContext, McpClient, cell, http, json as expect, native};

const INSTANCE: &str = "ldk-server-instance";
const RUN: &str = "ldk-server-payments";
const LDK: &str = "ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536";
const WALLET: &str = "cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive";

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("ldk-server", "designer")?;
    let document: Value =
        serde_json::from_str(include_str!("../../../../examples/ldk-server-cell.json"))?;
    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":document,"request_id":"ldk-plan"}),
    )?;
    context.record(
        "ldk-server-plan.json",
        &cell::review(&mut client, &preview)?,
    )?;
    cell::apply(&mut client, &preview)?;
    let result = (|| {
        let ready = cell::wait_ready(&mut client, INSTANCE)?;
        context.record("ldk-server-ready.json", &ready)?;
        client.call(
            "run_start",
            json!({"request_id":"ldk-run","run_id":RUN,"name":INSTANCE}),
        )?;
        exercise(
            context,
            &mut client,
            expect::string(&ready, "/instance_namespace")?,
        )
    })();
    let _ = client.call(
        "run_finish",
        json!({"request_id":"ldk-finish","run_id":RUN}),
    );
    let cleanup = client
        .call("cell_remove", json!({"name":INSTANCE}))
        .and_then(|_| cell::wait_closed(&mut client, INSTANCE));
    result?;
    cleanup?;
    println!(
        "LDK Server processor: BOLT11 mint/melt, BOLT12 payment, node identity and pending quote recovery passed"
    );
    Ok(())
}

fn bootstrap(client: &mut McpClient) -> Result<Value> {
    let mut session = native::Session::new(client, INSTANCE, RUN);
    session.execute(
        "chain",
        "chain-wallet",
        &format!("{} createwallet default >/dev/null", native::BITCOIN_ROOT),
    )?;
    session.mine("chain", "mature-coins", 101)?;
    for node in ["ldk", "payer"] {
        let address = session.json(
            node,
            &format!("address-{node}"),
            &format!("{LDK} onchain-receive"),
        )?;
        session.execute(
            "chain",
            &format!("fund-{node}"),
            &format!(
                "{} sendtoaddress {} 1",
                native::BITCOIN,
                native::quote(expect::string(&address, "/address")?)
            ),
        )?;
    }
    session.mine("chain", "confirm-funds", 6)?;
    for node in ["ldk", "payer"] {
        session.poll(
            node,
            &format!("funded-{node}"),
            &format!("{LDK} get-balances"),
            |value| {
                Ok(
                    (expect::integer(value, "/spendable_onchain_balance_sats")? >= 1_000_000)
                        .then_some(()),
                )
            },
        )?;
    }
    let identity = session.json("ldk", "node-identity", &format!("{LDK} get-node-info"))?;
    session.execute(
        "payer",
        "open-channel",
        &format!(
            "{LDK} open-channel {} ldk:9735 1000000sat --push-to-counterparty 500000sat",
            native::quote(expect::string(&identity, "/node_id")?)
        ),
    )?;
    session.poll(
        "chain",
        "channel-funding-broadcast",
        &format!("{} getrawmempool", native::BITCOIN),
        |value| Ok((!expect::array(value, "")?.is_empty()).then_some(())),
    )?;
    session.mine("chain", "confirm-channel", 6)?;
    for node in ["ldk", "payer"] {
        session.poll(
            node,
            &format!("usable-{node}"),
            &format!("{LDK} list-channels"),
            |value| {
                Ok(expect::array(value, "/channels")?
                    .iter()
                    .any(|channel| channel["is_usable"] == true)
                    .then_some(()))
            },
        )?;
    }
    Ok(identity)
}

fn balance(context: &GateContext, client: &mut McpClient, id: &str, amount: u64) -> Result<()> {
    let observed = native::observe_wallet(
        client,
        "cdk-cli-wallet",
        &json!({"name":INSTANCE,"run_id":RUN,"request_id":id,"wallet":"wallet","mint":"mint"}),
    )?;
    ensure!(
        observed["balance_sat"] == amount
            && observed["reserved_sat"] == 0
            && observed["pending_sat"] == 0
            && observed["pending_spent_sat"] == 0,
        "unexpected passive wallet state: {observed}"
    );
    context.record(&format!("{id}.json"), &observed)
}

fn restart(client: &mut McpClient, component: &str, id: &str) -> Result<()> {
    control(client, "component_restart", component, id)?;
    cell::wait_ready(client, INSTANCE)?;
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

fn exercise(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let identity = bootstrap(client)?;
    context.record("ldk-identity.json", &identity)?;
    let settings = native::json_output(
        client,
        INSTANCE,
        RUN,
        "processor",
        "processor-settings",
        "/opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls",
    )?;
    ensure!(
        settings["unit"] == "msat"
            && settings["bolt11"].is_object()
            && settings["bolt12"].is_object(),
        "processor capability mismatch: {settings}"
    );
    context.record("processor-settings.json", &settings)?;
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        "initialize-wallet",
        &format!("{WALLET} balance >/dev/null"),
    )?;
    balance(context, client, "empty-wallet", 0)?;
    // The quote is created before interrupting the processor. The native wallet must
    // claim the original quote; a new quote would hide lost payment correlation.
    client.call(
        "cell_exec",
        json!({"name":INSTANCE,"run_id":RUN,"component":"wallet","request_id":"mint-5000",
            "script":format!("{WALLET} mint http://mint:3338 5000 --wait-duration 240"),
            "timeout_seconds":300,"output":{"mode":"public"}}),
    )?;
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        "await-mint-quote",
        "/opt/proofstorm/driver cdk-quote await UNPAID /wallet/cdk/cdk-cli.sqlite http://mint:3338 5000",
    )?;
    let invoice = context.kubectl.exec(
        namespace,
        "deployment/wallet",
        &[
            "/opt/proofstorm/driver",
            "cdk-quote",
            "invoice",
            "UNPAID",
            "/wallet/cdk/cdk-cli.sqlite",
            "http://mint:3338",
            "5000",
        ],
    )?;
    let quote_id = context.kubectl.exec(
        namespace,
        "deployment/wallet",
        &[
            "/opt/proofstorm/driver",
            "cdk-quote",
            "id",
            "UNPAID",
            "/wallet/cdk/cdk-cli.sqlite",
            "http://mint:3338",
            "5000",
        ],
    )?;
    control(
        client,
        "component_stop",
        "processor",
        "stop-pending-processor",
    )?;
    let payment = native::json_output(
        client,
        INSTANCE,
        RUN,
        "payer",
        "pay-mint-quote",
        &format!("{LDK} bolt11-send {}", native::quote(invoice.trim())),
    )?;
    wait_payment(
        client,
        "payer",
        "mint-payment",
        expect::string(&payment, "/payment_id")?,
        5_000_000,
    )?;
    balance(context, client, "processor-offline-wallet", 0)?;
    control(
        client,
        "component_start",
        "processor",
        "start-pending-processor",
    )?;
    cell::wait_ready(client, INSTANCE)?;
    // LDK's live event subscription does not replay payments missed during an
    // outage. Reconcile the original quote against durable backend history.
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let recovered = http::get_json_retrying(
        &mut forward,
        &format!("/v1/mint/quote/bolt11/{}", quote_id.trim()),
        10,
    )?;
    ensure!(
        recovered["quote"] == quote_id.trim() && recovered["state"] == "PAID",
        "original mint quote was not recovered: {recovered}"
    );
    context.record("recovered-mint-quote.json", &recovered)?;
    native::wait(client, "mint-5000")?;
    balance(context, client, "minted-wallet", 5000)?;
    melt(context, client, namespace, "first", 700, 4300)?;
    for component in ["ldk", "processor", "mint"] {
        restart(client, component, &format!("restart-{component}"))?;
    }
    let recovered = native::json_output(
        client,
        INSTANCE,
        RUN,
        "ldk",
        "recovered-identity",
        &format!("{LDK} get-node-info"),
    )?;
    ensure!(
        identity["node_id"] == recovered["node_id"],
        "node identity changed on restart"
    );
    balance(context, client, "recovered-wallet", 4300)?;
    melt(context, client, namespace, "recovered", 300, 4000)?;
    bolt12(context, client, namespace)?;
    Ok(())
}

fn wait_payment(
    client: &mut McpClient,
    node: &str,
    id: &str,
    payment_id: &str,
    amount_msat: u64,
) -> Result<()> {
    native::Session::new(client, INSTANCE, RUN).poll(
        node,
        id,
        &format!("{LDK} get-payment-details {}", native::quote(payment_id)),
        |value| {
            ensure!(
                value["payment"]["status"] != "FAILED",
                "native payment failed: {value}"
            );
            if value["payment"]["status"] != "SUCCEEDED" {
                return Ok(None);
            }
            ensure!(
                value["payment"]["amount_msat"] == amount_msat,
                "native payment amount differs: {value}"
            );
            Ok(Some(()))
        },
    )
}

fn melt(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    id: &str,
    amount: u64,
    remaining: u64,
) -> Result<()> {
    let invoice = native::json_output(
        client,
        INSTANCE,
        RUN,
        "payer",
        &format!("invoice-{id}"),
        &format!("{LDK} bolt11-receive {amount}sat --description acceptance-{id}"),
    )?;
    let log = format!("/wallet/acceptance-melt-{id}.log");
    native::submit(
        client,
        INSTANCE,
        RUN,
        "wallet",
        &format!("melt-{id}"),
        &format!(
            "{WALLET} melt --mint-url http://mint:3338 --invoice {} > {} 2>&1; result=$?; cat {}; exit \"$result\"",
            native::quote(expect::string(&invoice, "/invoice")?),
            native::quote(&log),
            native::quote(&log),
        ),
    )?;
    wait_payment(
        client,
        "payer",
        &format!("received-{id}"),
        expect::string(&invoice, "/payment_hash")?,
        amount * 1000,
    )?;
    // Independently establish settlement before reconciling the original melt.
    // A notification missed during reconnect must not cause a second send.
    let output = context
        .kubectl
        .exec(namespace, "deployment/wallet", &["cat", &log])?;
    let quote = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Quote ID: "))
        .ok_or_else(|| anyhow::anyhow!("native wallet did not report its melt quote"))?;
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let path = format!("/v1/melt/quote/bolt11/{quote}");
    let mut paid = false;
    for _ in 0..30 {
        let state = http::get_json_retrying(&mut forward, &path, 3)?;
        if state["state"] == "PAID" {
            ensure!(state["amount"] == amount, "melt amount differs: {state}");
            context.record(
                &format!("melt-{id}-paid.json"),
                &json!({"quote":quote,"state":"PAID","amount":amount}),
            )?;
            paid = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    ensure!(
        paid,
        "settled recipient payment was not reconciled by the mint"
    );
    native::wait(client, &format!("melt-{id}"))?;
    balance(context, client, &format!("balance-{id}"), remaining)
}

fn bolt12(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    // Opening a port-forward starts the child before its local socket is ready.
    // Wait with a read before submitting the quote creation exactly once.
    http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    let quote = http::post_json(
        &forward.url("/v1/mint/quote/bolt12"),
        &json!({"amount":100,"unit":"sat","description":"Processor acceptance","pubkey":"0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"}),
    )?;
    context.record("bolt12-created-quote.json", &quote)?;
    ensure!(
        quote["amount"] == 100 && quote["unit"] == "sat",
        "BOLT12 quote terms differ: {quote}"
    );
    let offer = native::json_output(
        client,
        INSTANCE,
        RUN,
        "ldk",
        "decode-bolt12-offer",
        &format!(
            "{LDK} decode-offer {}",
            native::quote(expect::string(&quote, "/request")?)
        ),
    )?;
    let payment = native::json_output(
        client,
        INSTANCE,
        RUN,
        "payer",
        "pay-bolt12",
        &format!(
            "{LDK} bolt12-send {}",
            native::quote(expect::string(&quote, "/request")?)
        ),
    )?;
    wait_payment(
        client,
        "payer",
        "bolt12-settled",
        expect::string(&payment, "/payment_id")?,
        100_000,
    )?;
    context.record("bolt12-settled-payment.json", &payment)?;
    let received = received_offer(client, expect::string(&offer, "/offer_id")?)?;
    context.record("bolt12-recipient-payment.json", &received)?;
    // Blinded BOLT12 routes can overpay. Credit must equal the independently
    // received amount, converted with CDK's whole-satoshi truncation.
    let received_msat = expect::integer(&received, "/amount_msat")?;
    ensure!(received_msat >= 100_000, "BOLT12 recipient was underpaid");
    let credited_sat = received_msat / 1000;
    let path = format!(
        "/v1/mint/quote/bolt12/{}",
        expect::string(&quote, "/quote")?
    );
    for _ in 0..30 {
        let state = http::get_json_retrying(&mut forward, &path, 3)?;
        context.record("bolt12-observed-quote.json", &state)?;
        if state["amount_paid"] == credited_sat {
            ensure!(
                state["quote"] == quote["quote"]
                    && state["unit"] == "sat"
                    && state["amount_issued"] == 0,
                "BOLT12 quote identity or issuance differs: {state}"
            );
            return context.record("bolt12-paid-quote.json", &state);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    anyhow::bail!("BOLT12 quote did not record the independently settled payment")
}

fn received_offer(client: &mut McpClient, offer_id: &str) -> Result<Value> {
    native::Session::new(client, INSTANCE, RUN).poll(
        "ldk",
        "bolt12-recipient",
        &format!("{LDK} list-payments"),
        |value| {
            let receipts = expect::array(value, "/list")?
                .iter()
                .filter(|payment| {
                    payment["direction"] == "INBOUND"
                        && payment["status"] == "SUCCEEDED"
                        && payment
                            .pointer("/kind/kind/bolt12_offer/offer_id")
                            .and_then(Value::as_str)
                            == Some(offer_id)
                })
                .collect::<Vec<_>>();
            ensure!(receipts.len() <= 1, "offer was paid more than once");
            receipts
                .first()
                .map(|payment| {
                    Ok(json!({
                        "offer_id":offer_id,
                        "payment_id":expect::string(payment, "/id")?,
                        "amount_msat":expect::integer(payment, "/amount_msat")?,
                        "direction":"INBOUND",
                        "status":"SUCCEEDED"
                    }))
                })
                .transpose()
        },
    )
}
