//! Native invoice/payment composition with independent command receipts and
//! exact passive settlement observations.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, json as expect, native::omit_native_request_source};

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "quote-composition",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "quote-mint"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "quote-payer"}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Quote Composition Mint"}},
            {"id": "payer-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "recipient-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}
        ],
        "links": [
            {"id": "mint-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}
        ],
        "policy": {"allow": ["component.forensics"], "limits": {"max_components": 16, "max_links": 32, "max_config_bytes": 32768}}
    })
}

fn assert_no_invoice(value: &Value, label: &str) -> Result<()> {
    let serialized = serde_json::to_string(value)?.to_ascii_lowercase();
    if serialized.contains("lnbcrt") || serialized.contains("payment_request") {
        bail!("{label} disclosed a Lightning invoice");
    }
    Ok(())
}

pub fn run(context: &GateContext) -> Result<()> {
    let run = &context.run_id;
    let workspace = format!("quote-composition-{run}");

    let instance = format!("quote-composition-instance-{run}");
    let experiment = format!("quote-composition-experiment-{run}");

    let mut client = context.default_session(&workspace, "quote-agent")?;

    let preview = client.call(
        "cell_plan",
        json!({"name":instance,"cell":cell_document(),"request_id":format!("create-{run}")}),
    )?;
    crate::cell::review(&mut client, &preview)?;
    crate::cell::apply(&mut client, &preview)?;
    cell::wait_phase(&mut client, &instance, "ready", 200, Duration::from_secs(3))?;

    client.call(
        "run_start",
        json!({"request_id":"5413","run_id": experiment, "name": instance}),
    )?;

    crate::native::bootstrap(
        &mut client,
        &instance,
        &experiment,
        "bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        5_000_000,
    )?;

    let mut native = crate::native::Session::new(&mut client, &instance, &experiment);
    for (operation, wallet) in [
        ("initialize-payer", "payer-wallet"),
        ("initialize-recipient", "recipient-wallet"),
    ] {
        native.nutshell_initialize(wallet, "mint", operation)?;
    }
    native.nutshell_fund("payer-wallet", "mint", "payer-lnd", "fund-payer", 1000)?;

    let composed_quote =
        native.nutshell_invoice("recipient-wallet", "mint", "compose-invoice", 100)?;
    let composed_invoice = native.nutshell_invoice_projection(
        "recipient-wallet",
        "mint",
        "compose-invoice-read",
        &composed_quote,
        100,
    )?;
    let before = native.nutshell_balance("payer-wallet", "mint", "compose-before")?;
    let paid_content = native.nutshell_melt(
        "payer-wallet",
        "mint",
        "compose-pay",
        expect::string(&composed_invoice, "/payment_request")?,
        100,
    )?;
    anyhow::ensure!(
        paid_content["state"] == "PAID",
        "native payment did not settle"
    );
    let after = native.nutshell_balance("payer-wallet", "mint", "compose-after")?;
    anyhow::ensure!(
        before
            .checked_sub(after)
            .is_some_and(|spent| (100..=110).contains(&spent)),
        "native payment moved an unexpected amount"
    );
    native.nutshell_claim(
        "recipient-wallet",
        "mint",
        "compose-claim",
        &composed_quote,
        100,
    )?;
    anyhow::ensure!(
        native.nutshell_balance("recipient-wallet", "mint", "compose-received")? == 100,
        "recipient did not receive 100 sat"
    );

    let external_quote =
        native.nutshell_invoice("recipient-wallet", "mint", "external-invoice", 200)?;
    let external_invoice = native.nutshell_invoice_projection(
        "recipient-wallet",
        "mint",
        "external-invoice-read",
        &external_quote,
        200,
    )?;
    let external_payment = native.projected(
        "payer-lnd",
        "external-lightning-pay",
        &format!(
            "{} payinvoice --force --json {}",
            crate::native::LND,
            crate::native::quote(expect::string(&external_invoice, "/payment_request")?)
        ),
        &json!({"mode":"json_fields","fields":["status","value_sat"]}),
    )?;
    anyhow::ensure!(
        external_payment == json!({"status":"SUCCEEDED","value_sat":"200"}),
        "external payment did not settle"
    );

    let claim_content = native.nutshell_claim(
        "recipient-wallet",
        "mint",
        "external-claim",
        &external_quote,
        200,
    )?;
    anyhow::ensure!(
        native.nutshell_balance("recipient-wallet", "mint", "external-claim-balance")? == 300,
        "native claim did not preserve the recipient's earlier payment"
    );
    assert_no_invoice(&claim_content, "passive claim observation")?;
    let claimed = cell::wait_succeeded(&mut client, "external-claim-observe")?;
    expect::equals(&claimed, "/kind", &json!("component_exec_live"))?;

    let mut journal = crate::cell::journal(&mut client, &experiment)?;
    omit_native_request_source(&mut journal);
    let journal = json!({"actions": journal});
    for (value, label) in [
        (&paid_content, "native melt observation"),
        (&claimed, "native claim observation"),
        (&journal, "journal outside caller-supplied requests"),
    ] {
        assert_no_invoice(value, label)?;
    }

    client.call(
        "run_finish",
        json!({"request_id":"13809","run_id": experiment}),
    )?;
    let evidence = crate::cell::evidence(
        &mut client,
        json!({
            "run_id": experiment, "include_oracle_artifacts": false,
            "artifact_operation_ids": ["compose-pay", "compose-pay-observe", "compose-claim-observe", "external-invoice", "external-claim", "external-claim-observe"]
        }),
    )?;
    let mut generated_evidence = evidence.clone();
    let journal = generated_evidence
        .pointer_mut("/content/journal")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("exported evidence has no journal"))?;
    omit_native_request_source(journal);
    assert_no_invoice(
        &generated_evidence,
        "evidence outside caller-supplied native requests",
    )?;

    client.call("cell_remove", json!({"name": instance}))?;
    let closed = cell::wait_phase(
        &mut client,
        &instance,
        "closed",
        100,
        Duration::from_secs(3),
    )?;
    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("quote composition cell teardown was not verified: {closed}");
    }
    println!(
        "Quote composition acceptance passed: native invoices, wallet and external payments, claims, retries, balances and receipt non-disclosure are verified"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_native_input_does_not_exempt_receipts_or_typed_requests() {
        let mut journal = vec![json!({"kind":"component_exec_live",
            "request":{"script":"lncli payinvoice lnbcrt-caller-supplied"},
            "artifact":{"stdout":"claim complete"}})];
        omit_native_request_source(&mut journal);
        assert!(assert_no_invoice(&json!(journal), "native input").is_ok());
        journal[0]["artifact"]["stdout"] = json!("lnbcrt-generated");
        assert!(assert_no_invoice(&json!(journal), "generated receipt").is_err());

        let mut typed = vec![json!({"kind":"wallet_invoice",
            "request":{"payment_request":"lnbcrt-generated"}})];
        omit_native_request_source(&mut typed);
        assert!(assert_no_invoice(&json!(typed), "typed request").is_err());
    }
}
