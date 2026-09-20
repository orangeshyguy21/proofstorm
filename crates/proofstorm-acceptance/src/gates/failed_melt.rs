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
            {"id": "payer-mint", "kind": "mint", "implementation": "nutshell", "version": "0.21.0", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Failed Melt Payer", "description": "Melt failure acceptance"}},
            {"id": "recipient-mint", "kind": "mint", "implementation": "nutshell", "version": "0.21.0", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Failed Melt Recipient", "description": "Melt failure acceptance"}},
            {"id": "payer-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "recipient-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}
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
    crate::native::bootstrap(
        &mut client,
        INSTANCE,
        EXPERIMENT,
        "failed-melt-bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        5_000_000,
    )?;

    let mut native = crate::native::Session::new(&mut client, INSTANCE, EXPERIMENT);
    native.nutshell_initialize("payer-wallet", "payer-mint", "failed-melt-init-payer")?;
    native.nutshell_initialize(
        "recipient-wallet",
        "recipient-mint",
        "failed-melt-init-recipient",
    )?;
    anyhow::ensure!(
        native.nutshell_fund(
            "payer-wallet",
            "payer-mint",
            "mint-lnd",
            "failed-melt-fund",
            FUNDED_SAT
        )? == FUNDED_SAT,
        "payer wallet was not funded"
    );

    let before =
        native.nutshell_balance("payer-wallet", "payer-mint", "failed-melt-balance-before")?;
    let quote_id = native.nutshell_invoice(
        "recipient-wallet",
        "recipient-mint",
        "failed-melt-invoice",
        INVOICE_SAT,
    )?;
    let invoice = native.nutshell_invoice_projection(
        "recipient-wallet",
        "recipient-mint",
        "failed-melt-invoice-read",
        &quote_id,
        INVOICE_SAT,
    )?;
    // A successful CLI exit is independent of economic settlement. This pinned
    // CLI can return zero when Lightning could not route the payment.
    let melt = native.nutshell_melt(
        "payer-wallet",
        "payer-mint",
        "failed-melt-pay",
        expect::string(&invoice, "/payment_request")?,
        INVOICE_SAT,
    )?;
    let mint = native.nutshell_mint_melt(
        "payer-wallet",
        "payer-mint",
        "failed-melt-mint-observe",
        &melt,
    )?;
    let receive = native.nutshell_receive(
        "recipient-wallet",
        "recipient-mint",
        "failed-melt-receive-observe",
        &quote_id,
    )?;
    anyhow::ensure!(
        melt["state"] == "UNPAID" && mint["state"] == "UNPAID" && receive["state"] == "UNPAID",
        "unroutable payment incorrectly settled or promoted its receive quote"
    );
    anyhow::ensure!(
        melt["input_proof_count"] == 0 && melt["input_fee_sat"] == 0,
        "failed zero-fee melt consumed proofs"
    );
    let after =
        native.nutshell_balance("payer-wallet", "payer-mint", "failed-melt-balance-after")?;
    anyhow::ensure!(
        before == FUNDED_SAT && after == FUNDED_SAT,
        "failed zero-fee melt moved value: before={before} after={after}"
    );
    anyhow::ensure!(
        native.nutshell_balance(
            "recipient-wallet",
            "recipient-mint",
            "failed-melt-recipient-balance"
        )? == 0,
        "unpaid recipient holds value"
    );

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

            "artifact_operation_ids": ["failed-melt-pay", "failed-melt-pay-observe", "failed-melt-mint-observe", "failed-melt-receive-observe"]
        }),
    )?;
    if !expect::string(&evidence, "/digest")?.starts_with("sha256:") {
        bail!("failed melt evidence was not exported: {evidence}");
    }
    for (id, observed) in [
        ("failed-melt-pay-observe", &melt),
        ("failed-melt-mint-observe", &mint),
        ("failed-melt-receive-observe", &receive),
    ] {
        let exported = expect::array(&evidence, "/content/artifacts")?
            .iter()
            .find(|artifact| artifact["operation_id"] == id)
            .ok_or_else(|| anyhow::anyhow!("evidence omitted {id}"))?;
        let receipt = exported
            .pointer("/artifact/content")
            .ok_or_else(|| anyhow::anyhow!("evidence has no native receipt"))?;
        anyhow::ensure!(
            crate::native::json_content(receipt)? == *observed,
            "exported observation differs from {id}"
        );
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
