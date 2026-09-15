//! Native Nutshell issuance/payment, balance observation, and persistent restart.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

use crate::{GateContext, McpClient, cell, native};

const NAME: &str = "cdk-wallet-instance";
const RUN: &str = "cdk-wallet-experiment";
const CASHU: &str = "cashu -h http://mint:3338 -u sat -w wallet -t -y";

fn operation(client: &mut McpClient, id: &str, component: &str, args: Value) -> Result<Value> {
    let mut args = args;
    args["name"] = json!(NAME);
    args["run_id"] = json!(RUN);
    args["request_id"] = json!(id);
    args["component"] = json!(component);
    client.call("cell_exec", args)?;
    let receipt = cell::wait_succeeded(client, id)?;
    let content = cell::artifact_content(&receipt)?;
    ensure!(
        content["exit_code"] == 0
            && content["cleanup_verified"] == true
            && content["streams_complete"] == true,
        "native Nutshell operation {id} failed: {content}"
    );
    Ok(content.clone())
}

fn balance(client: &mut McpClient, id: &str, expected: u64) -> Result<Value> {
    client.call(
        "wallet_balance",
        json!({"name":NAME,"run_id":RUN,"request_id":id,"wallet":"wallet-a","mint":"mint"}),
    )?;
    let receipt = cell::wait_succeeded(client, id)?;
    ensure!(
        cell::artifact_content(&receipt)?["balance_sat"] == expected,
        "issued balance was not retained: {receipt}"
    );
    Ok(receipt)
}

pub(super) fn exercise(context: &GateContext, client: &mut McpClient) -> Result<()> {
    native::stdout(
        client,
        NAME,
        RUN,
        "wallet-a",
        "nutshell-native-help",
        "cashu --help",
    )?;
    native::bootstrap(
        client,
        NAME,
        RUN,
        "nutshell-bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        5_000_000,
    )?;
    let invoice = operation(
        client,
        "nutshell-invoice",
        "wallet-a",
        json!({
        "script":format!("set -eu; umask 077; {CASHU} invoice 5000 --no-check > /wallet/candidate-invoice.txt 2>&1; sed -n 's/^Invoice: //p' /wallet/candidate-invoice.txt"),
        "timeout_seconds":60,"output":{"mode":"bolt11"}}),
    )?;
    context.record("candidate-nutshell-wallet-invoice.json", &invoice)?;
    let request = super::cocod_wallet::relay_invoice(&invoice, 5000)?;
    let paid = operation(
        client,
        "nutshell-pay-invoice",
        "payer-lnd",
        json!({
        "argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","--rpcserver=127.0.0.1:10009","payinvoice","--force","--json",request],
        "timeout_seconds":60,"output":{"mode":"json_fields","fields":["status","value_sat"]}}),
    )?;
    ensure!(
        paid["selected_output"] == json!({"status":"SUCCEEDED","value_sat":"5000"}),
        "payer did not settle the requested amount"
    );
    context.record("candidate-nutshell-wallet-payer.json", &paid)?;
    let claim = operation(
        client,
        "nutshell-claim",
        "wallet-a",
        json!({
        "script":format!("set -eu; id=$(sed -n 's/.*--id \\([0-9a-f-][0-9a-f-]*\\).*/\\1/p' /wallet/candidate-invoice.txt | head -1); test -n \"$id\"; {CASHU} invoice 5000 --id \"$id\""),
        "timeout_seconds":60}),
    )?;
    context.record("candidate-nutshell-wallet-claim.json", &claim)?;
    context.record(
        "candidate-nutshell-wallet-funded.json",
        &balance(client, "nutshell-funded", 5000)?,
    )?;
    let recipient = operation(
        client,
        "nutshell-recipient-invoice",
        "payer-lnd",
        json!({"argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","--rpcserver=127.0.0.1:10009","addinvoice","--amt=700"],
        "timeout_seconds":60,"output":{"mode":"lnd_invoice"}}),
    )?;
    let request = super::cocod_wallet::relay_invoice(&recipient, 700)?;
    let payment = operation(
        client,
        "nutshell-outgoing-payment",
        "wallet-a",
        json!({"argv":["cashu","-h","http://mint:3338","-u","sat","-w","wallet","-t","-y","pay",request,"--yes"],
        "timeout_seconds":60}),
    )?;
    context.record("candidate-nutshell-wallet-payment.json", &payment)?;
    ensure!(
        payment["output_mode"] == "private" && payment["stdout"] == "" && payment["stderr"] == "",
        "native payment disclosed raw wallet output"
    );
    let settled = operation(
        client,
        "nutshell-recipient-settlement",
        "payer-lnd",
        json!({"argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","--rpcserver=127.0.0.1:10009","lookupinvoice",recipient["selected_output"]["payment_hash"]],
        "timeout_seconds":60,"output":{"mode":"json_fields","fields":["settled"]}}),
    )?;
    context.record("candidate-nutshell-wallet-recipient.json", &settled)?;
    ensure!(
        settled["selected_output"]["settled"] == true,
        "native Nutshell payment did not settle at the recipient"
    );
    context.record(
        "candidate-nutshell-wallet-after-payment.json",
        &balance(client, "nutshell-after-payment", 4300)?,
    )?;
    client.call(
        "component_restart",
        json!({"name":NAME,"run_id":RUN,"request_id":"nutshell-restart","component":"wallet-a"}),
    )?;
    cell::wait_succeeded(client, "nutshell-restart")?;
    cell::wait_ready(client, NAME)?;
    context.record(
        "candidate-nutshell-wallet-after-restart.json",
        &balance(client, "nutshell-restarted-balance", 4300)?,
    )
}
