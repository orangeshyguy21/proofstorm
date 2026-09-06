//! A melt that cannot settle must be journaled as a payment that did not
//! happen.
//!
//! This gate exists because the wallet pay adapter used to assert `paid`
//! whenever the wallet CLI exited zero, which it does even when the mint
//! rolls a failed Lightning payment back. Proofstorm then promoted the
//! recipient's receive quote as well, so the journal recorded a settlement
//! that never occurred. That is the exact hazard these laboratories exist to
//! detect in a mint, manufactured by the harness itself.
//!
//! The failure is structural rather than injected. The recipient mint runs on
//! an island Lightning node that holds no channels, so its invoice has no
//! route from anywhere and the payer's mint fails the payment every time. No
//! component is stopped or partitioned, so nothing else in the lab becomes
//! unready and the outcome does not depend on fault-detection timing.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, json as expect, lab};

const INSTANCE: &str = "failed-melt-instance";
const EXPERIMENT: &str = "failed-melt-experiment";
const LEASE: &str = "failed-melt-session";
const DRAFT: &str = "failed-melt";
const FUNDED_SAT: u64 = 2_000;
const INVOICE_SAT: u64 = 1_000;

const CAPABILITIES: &[&str] = &[
    "catalog.read",
    "lab.read",
    "lab.create",
    "lab.validate",
    "lab.publish",
    "lab.materialize",
    "lab.status",
    "lab.close",
    "experiment.create",
    "experiment.read",
    "experiment.close",
    "lab.operate",
    "action.cancel",
    "wallet.create",
    "wallet.control",
    "wallet.fund",
    "chain.mine",
    "peer.connect",
    "channel.open",
    "artifact.read",
];

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "failed-melt-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-funder"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-payer"}},
            // Deliberately channel-less: nothing can route to it, so every
            // invoice it issues is unpayable.
            {"id": "island-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-island"}},
            {"id": "payer-mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Failed Melt Payer", "description": "Melt failure acceptance"}},
            {"id": "recipient-mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Failed Melt Recipient", "description": "Melt failure acceptance"}},
            {"id": "payer-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
            {"id": "recipient-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}}
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
    let mut client = context.session("failed-melt-live", "experiment-agent", CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(), "idempotency_key": "create-failed-melt"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-failed-melt", "include_revision": true}),
    )?;
    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-failed-melt"}),
    )?;
    lab::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;

    client.call(
        "proofstorm_experiment_create",
        json!({"experiment_id": EXPERIMENT, "instance_id": INSTANCE, "idempotency_key": "create-failed-melt-experiment"}),
    )?;
    client.call(
        "proofstorm_session_start",
        json!({"experiment_id": EXPERIMENT, "session_id": LEASE, "idempotency_key": "acquire-failed-melt-session"}),
    )?;

    // Liquidity is opened between the funder and the payer only. The island
    // node is funded by nobody and peers with nobody.
    client.call(
        "proofstorm_liquidity_bootstrap",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
            "operation_id": "failed-melt-bootstrap", "chain": "chain",
            "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
            "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000,
            "idempotency_key": "bootstrap-failed-melt"
        }),
    )?;
    let bootstrap = lab::wait_operation(&mut client, "failed-melt-bootstrap", 160)?;
    if !expect::boolean(lab::artifact_content(&bootstrap)?, "/ready")? {
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
        client.call(
            "proofstorm_wallet_initialize",
            json!({
                "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
                "operation_id": operation, "wallet": wallet, "mint": mint,
                "idempotency_key": operation
            }),
        )?;
        let initialized = lab::wait_operation(&mut client, operation, 160)?;
        if !expect::boolean(lab::artifact_content(&initialized)?, "/initialized")? {
            bail!("{wallet} initialization failed: {initialized}");
        }
    }

    // The funder pays the payer mint's own invoice, so the payer wallet holds
    // real ecash. A later failure therefore cannot be blamed on an empty
    // wallet.
    client.call(
        "proofstorm_wallet_fund",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
            "operation_id": "failed-melt-fund", "wallet": "payer-wallet", "mint": "payer-mint",
            "payer_lightning": "mint-lnd", "amount_sat": FUNDED_SAT,
            "idempotency_key": "fund-failed-melt"
        }),
    )?;
    let funded = lab::wait_operation(&mut client, "failed-melt-fund", 160)?;
    if expect::integer(lab::artifact_content(&funded)?, "/balance_sat")? != FUNDED_SAT {
        bail!("payer wallet was not funded: {funded}");
    }

    client.call(
        "proofstorm_wallet_balance",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
            "operation_id": "failed-melt-balance-before", "wallet": "payer-wallet", "mint": "payer-mint",
            "idempotency_key": "balance-before-failed-melt"
        }),
    )?;
    let balance_before = lab::wait_operation(&mut client, "failed-melt-balance-before", 160)?;
    let before = expect::integer(lab::artifact_content(&balance_before)?, "/balance_sat")?;

    client.call(
        "proofstorm_wallet_invoice",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
            "operation_id": "failed-melt-invoice",
            "wallet": "recipient-wallet", "mint": "recipient-mint",
            "amount_sat": INVOICE_SAT, "timeout_seconds": 300,
            "idempotency_key": "invoice-failed-melt"
        }),
    )?;
    let invoice = lab::wait_operation(&mut client, "failed-melt-invoice", 160)?;
    let invoice_content = lab::artifact_content(&invoice)?;
    let mint_quote_id = expect::string(invoice_content, "/mint_quote_id")?.to_string();
    if expect::string(invoice_content, "/quote_observations/0/state")? != "UNPAID" {
        bail!("receive quote did not begin unpaid: {invoice}");
    }

    // The melt cannot settle: the invoice was issued by a node with no
    // channels. The operation still succeeds, because an authoritative "did
    // not happen" is an observation, not an infrastructure failure.
    client.call(
        "proofstorm_wallet_pay",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
            "operation_id": "failed-melt-pay", "mint_quote_id": mint_quote_id,
            "wallet": "payer-wallet", "mint": "payer-mint",
            "recipient_wallet": "recipient-wallet", "recipient_mint": "recipient-mint",
            "idempotency_key": "pay-failed-melt"
        }),
    )?;
    let paid = lab::wait_operation(&mut client, "failed-melt-pay", 200)?;
    if expect::string(&paid, "/phase")? != "succeeded" {
        bail!("an unsettled melt must still be a completed observation: {paid}");
    }
    let content = lab::artifact_content(&paid)?.clone();

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
        "proofstorm_wallet_balance",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "session_id": LEASE,
            "operation_id": "failed-melt-balance-after", "wallet": "payer-wallet", "mint": "payer-mint",
            "idempotency_key": "balance-after-failed-melt"
        }),
    )?;
    let balance_after = lab::wait_operation(&mut client, "failed-melt-balance-after", 160)?;
    let after = expect::integer(lab::artifact_content(&balance_after)?, "/balance_sat")?;
    if before != FUNDED_SAT || after != FUNDED_SAT {
        bail!("a failed melt moved value: before={before} after={after} in {content}");
    }

    // The recipient's quote must never be promoted by a payment that did not
    // happen. This is the specific corruption the gate exists to prevent.
    let quote = client.call(
        "proofstorm_wallet_quote_status",
        json!({"instance_id": INSTANCE, "wallet": "recipient-wallet", "mint": "recipient-mint", "direction": "receive", "quote_id": mint_quote_id}),
    )?;
    if quote.get("phase").is_some()
        || expect::string(&quote, "/last_observation/state")? != "UNPAID"
    {
        bail!("an unsettled melt promoted or reinterpreted the receive quote: {quote}");
    }

    client.call(
        "proofstorm_session_finish",
        json!({"session_id": LEASE, "idempotency_key": "release-failed-melt-session"}),
    )?;
    let closed_experiment = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id": EXPERIMENT, "idempotency_key": "close-failed-melt-experiment"}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    let evidence = client.call(
        "proofstorm_artifact_export",
        json!({
            "experiment_id": EXPERIMENT,
            "include_oracle_artifacts": false,
            "include_content": true,
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

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    let closed = lab::wait_phase(&mut client, INSTANCE, "closed", 80, Duration::from_secs(3))?;
    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("failed melt lab teardown was not verified: {closed}");
    }

    println!(
        "Melt failure acceptance passed: an unroutable payment is journaled as unpaid, the receive quote is never promoted, no value moves, and the evidence agrees"
    );
    Ok(())
}
