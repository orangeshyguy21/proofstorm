//! A melt that cannot settle must be journaled as a payment that did not
//! happen.
//!
//! This gate exists because the wallet pay adapter used to assert `paid`
//! whenever the wallet CLI exited zero, which it does even when the mint
//! rolls a failed Lightning payment back. Proofstorm then promoted the
//! recipient's receive quote as well, so the journal recorded a settlement
//! that never occurred. That is the exact hazard these cells exist to
//! detect in a mint, manufactured by the harness itself.
//!
//! The failure is structural rather than injected. The recipient mint runs on
//! an island Lightning node that holds no channels, so its invoice has no
//! route from anywhere and the payer's mint fails the payment every time. No
//! component is stopped or partitioned, so nothing else in the cell becomes
//! unready and the outcome does not depend on fault-detection timing.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, json as expect};

const INSTANCE: &str = "failed-melt-instance";
const EXPERIMENT: &str = "failed-melt-experiment";
const FUNDED_SAT: u64 = 2_000;
const INVOICE_SAT: u64 = 1_000;

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "failed-melt-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-funder"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-payer"}},
            // Deliberately channel-less: nothing can route to it, so every
            // invoice it issues is unpayable.
            {"id": "island-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-island"}},
            {"id": "payer-mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Failed Melt Payer", "description": "Melt failure acceptance"}},
            {"id": "recipient-mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Failed Melt Recipient", "description": "Melt failure acceptance"}},
            {"id": "payer-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "recipient-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}
        ],
        "links": [
            {"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "island-lnd-chain", "kind": "chain_backend", "from": "island-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-bolt11", "kind": "payment_backend", "from": "payer-mint", "to": "payer-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "recipient-bolt11", "kind": "payment_backend", "from": "recipient-mint", "to": "island-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("failed-melt-live", "experiment-agent")?;

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":cell_document(),"request_id":"create-failed-melt"}),
    )?;
    crate::cell::review(&mut client, &preview)?;
    crate::cell::apply(&mut client, &preview)?;
    cell::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;

    client.call(
        "run_start",
        json!({"request_id":"5114","run_id": EXPERIMENT, "name": INSTANCE}),
    )?;

    // Liquidity is opened between the funder and the payer only. The island
    // node is funded by nobody and peers with nobody.
    crate::driver::liquidity_bootstrap(
        context,
        &mut client,
        json!({
            "name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": "failed-melt-bootstrap", "chain": "chain",
            "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
            "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000}),
    )?;
    let bootstrap = cell::wait_operation(&mut client, "failed-melt-bootstrap", 160)?;
    if !expect::boolean(cell::artifact_content(&bootstrap)?, "/ready")? {
        bail!("liquidity bootstrap artifact is invalid: {bootstrap}");
    }

    for (wallet, mint, operation) in [
        ("payer-wallet", "payer-mint", "failed-melt-init-payer"),
        (
            "recipient-wallet",
            "recipient-mint",
            "failed-melt-init-recipient",
        ),
    ] {
        crate::driver::wallet_initialize(
            context,
            &mut client,
            json!({
                "name": INSTANCE, "run_id": EXPERIMENT,
                "request_id": operation, "wallet": wallet, "mint": mint}),
        )?;
        let initialized = cell::wait_operation(&mut client, operation, 160)?;
        if !expect::boolean(cell::artifact_content(&initialized)?, "/initialized")? {
            bail!("{wallet} initialization failed: {initialized}");
        }
    }

    // The funder pays the payer mint's own invoice, so the payer wallet holds
    // real ecash. A later failure therefore cannot be blamed on an empty
    // wallet.
    crate::driver::wallet_fund(
        context,
        &mut client,
        json!({
            "name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": "failed-melt-fund", "wallet": "payer-wallet", "mint": "payer-mint",
            "payer_lightning": "mint-lnd", "amount_sat": FUNDED_SAT}),
    )?;
    let funded = cell::wait_operation(&mut client, "failed-melt-fund", 160)?;
    if expect::integer(cell::artifact_content(&funded)?, "/balance_sat")? != FUNDED_SAT {
        bail!("payer wallet was not funded: {funded}");
    }

    client.call(
        "wallet_balance",
        json!({
            "name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": "failed-melt-balance-before", "wallet": "payer-wallet", "mint": "payer-mint"}),
    )?;
    let balance_before = cell::wait_operation(&mut client, "failed-melt-balance-before", 160)?;
    let before = expect::integer(cell::artifact_content(&balance_before)?, "/balance_sat")?;

    crate::driver::wallet_invoice(
        context,
        &mut client,
        json!({
            "name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": "failed-melt-invoice",
            "wallet": "recipient-wallet", "mint": "recipient-mint",
            "amount_sat": INVOICE_SAT, "timeout_seconds": 300}),
    )?;
    let invoice = cell::wait_operation(&mut client, "failed-melt-invoice", 160)?;
    let invoice_content = cell::artifact_content(&invoice)?;
    let mint_quote_id = expect::string(invoice_content, "/mint_quote_id")?.to_string();
    if expect::string(invoice_content, "/quote_observations/0/state")? != "UNPAID" {
        bail!("receive quote did not begin unpaid: {invoice}");
    }

    // The melt cannot settle: the invoice was issued by a node with no
    // channels. The operation still succeeds, because an authoritative "did
    // not happen" is an observation, not an infrastructure failure.
    crate::driver::wallet_pay(
        context,
        &mut client,
        json!({
            "name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": "failed-melt-pay", "mint_quote_id": mint_quote_id,
            "wallet": "payer-wallet", "mint": "payer-mint",
            "recipient_wallet": "recipient-wallet", "recipient_mint": "recipient-mint"}),
    )?;
    let paid = cell::wait_operation(&mut client, "failed-melt-pay", 200)?;
    if expect::string(&paid, "/phase")? != "succeeded" {
        bail!("an unsettled melt must still be a completed observation: {paid}");
    }
    let content = cell::artifact_content(&paid)?.clone();

    if content.get("phase").is_some() {
        bail!("wallet-native observation was polluted with a Proofstorm phase: {content}");
    }
    if expect::string(&content, "/quote_observations/0/role")? != "payment_melt"
        || expect::string(&content, "/quote_observations/0/direction")? != "pay"
        || expect::string(&content, "/quote_observations/0/state")? != "UNPAID"
        || expect::string(&content, "/quote_observations/1/role")? != "payment_receive"
        || expect::string(&content, "/quote_observations/1/direction")? != "receive"
        || expect::string(&content, "/quote_observations/1/state")? != "UNPAID"
    {
        bail!("failed melt did not preserve distinct native observations: {content}");
    }

    client.call(
        "wallet_balance",
        json!({
            "name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": "failed-melt-balance-after", "wallet": "payer-wallet", "mint": "payer-mint"}),
    )?;
    let balance_after = cell::wait_operation(&mut client, "failed-melt-balance-after", 160)?;
    let after = expect::integer(cell::artifact_content(&balance_after)?, "/balance_sat")?;
    if before != FUNDED_SAT || after != FUNDED_SAT {
        bail!("a failed melt moved value: before={before} after={after} in {content}");
    }

    // The recipient's quote must never be promoted by a payment that did not
    // happen. This is the specific corruption the gate exists to prevent.
    let quote = crate::driver::quote_status(
        context,
        &mut client,
        json!({"name": INSTANCE, "wallet": "recipient-wallet", "mint": "recipient-mint", "direction": "receive", "quote_id": mint_quote_id}),
    )?;
    if quote.get("phase").is_some()
        || expect::string(&quote, "/last_observation/state")? != "UNPAID"
    {
        bail!("an unsettled melt promoted or reinterpreted the receive quote: {quote}");
    }

    let closed_experiment = client.call(
        "run_finish",
        json!({"request_id":"11170","run_id": EXPERIMENT}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    let evidence = crate::cell::evidence(
        &mut client,
        json!({
            "run_id": EXPERIMENT,
            "include_oracle_artifacts": false,

            "artifact_operation_ids": ["failed-melt-pay"]
        }),
    )?;
    if !expect::string(&evidence, "/digest")?.starts_with("sha256:") {
        bail!("failed melt evidence was not exported: {evidence}");
    }
    let exported = expect::array(&evidence, "/content/artifacts")?
        .first()
        .ok_or_else(|| anyhow::anyhow!("evidence carries no pay artifact"))?
        .clone();
    if expect::string(&exported, "/artifact/content/quote_observations/0/state")? != "UNPAID"
        || exported.pointer("/artifact/content/phase").is_some()
    {
        bail!("the exported evidence disagrees with the observation: {exported}");
    }

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    let closed = cell::wait_phase(&mut client, INSTANCE, "closed", 80, Duration::from_secs(3))?;
    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("failed melt cell teardown was not verified: {closed}");
    }

    println!(
        "Melt failure acceptance passed: an unroutable payment is journaled as unpaid, the receive quote is never promoted, no value moves, and the evidence agrees"
    );
    Ok(())
}
