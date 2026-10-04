use super::{
    CLN, Context, GateContext, INSTANCE, MINT, McpClient, PEER, RUN, Result, Value, WALLET,
    balance, cell, control, ensure, expect, json, native, quote_state, quote_states,
};

pub(super) fn mint(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
) -> Result<String> {
    // Run the native wallet while the peer pays. It may claim on notification
    // or return after creating its quote; reconcile only that original quote.
    client.call("cell_exec", json!({"name":INSTANCE,"run_id":RUN,"component":"wallet","request_id":"bark-mint-request","script":format!("{WALLET} mint {MINT} 100000 --wait-duration 480"),"timeout_seconds":600,"output":{"mode":"public"}}))?;
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        "bark-await-quote",
        &format!(
            "/opt/proofstorm/driver cdk-quote await UNPAID /wallet/cdk/cdk-cli.sqlite {MINT} 100000"
        ),
    )?;
    let mut parts = Vec::new();
    for field in ["invoice", "id"] {
        parts.push(context.kubectl.exec(
            namespace,
            "deployment/wallet",
            &[
                "/opt/proofstorm/driver",
                "cdk-quote",
                field,
                "UNPAID",
                "/wallet/cdk/cdk-cli.sqlite",
                MINT,
                "100000",
            ],
        )?);
    }
    let invoice = parts[0].trim();
    let quote = parts[1].trim();
    let decoded = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        "bark-decode-mint",
        &format!("{PEER} decodepay {}", native::quote(invoice)),
    )?;
    let hash = expect::string(&decoded, "/payment_hash")?;
    ensure!(
        expect::integer(&decoded, "/amount_msat")? == 100_000_000,
        "mint invoice amount differs"
    );
    control(
        client,
        "component_stop",
        "processor",
        "bark-stop-before-payment",
    )?;
    // Payment remains in flight while Bark's processor is stopped. Observing
    // the HTLC at hold establishes an actual interrupted payment, not merely
    // an unpaid quote or a completed payment followed by a restart.
    client.call("cell_exec", json!({"name":INSTANCE,"run_id":RUN,"component":"peer","request_id":"bark-pay-mint","script":format!("{PEER} pay {}", native::quote(invoice)),"timeout_seconds":600,"output":{"mode":"public"}}))?;
    let held = native::Session::new(client, INSTANCE, RUN).poll(
        "cln",
        "bark-held-payment",
        &format!("{CLN} listholdinvoices {}", native::quote(hash)),
        |v| {
            let rows = expect::array(v, "/holdinvoices")?;
            ensure!(
                rows.len() == 1 && rows[0]["payment_hash"] == hash,
                "held payment identity differs"
            );
            ensure!(
                rows[0]["state"] != "paid" && rows[0]["state"] != "cancelled",
                "payment completed before interruption was established"
            );
            Ok(
                (rows[0]["state"] == "accepted" && !expect::array(&rows[0], "/htlcs")?.is_empty())
                    .then(|| v.clone()),
            )
        },
    )?;
    context.record("bark-payment-interrupted.json", &held)?;
    control(
        client,
        "component_start",
        "processor",
        "bark-resume-processor",
    )?;
    cell::wait_ready_recorded(context, client, INSTANCE)?;
    let paid = native::json_content(&native::wait(client, "bark-pay-mint")?)?;
    check_payer(&paid, hash, 100_000_000)?;
    context.record("bark-mint-payer.json", &paid)?;
    native::wait(client, "bark-mint-request")?;
    let recognized = quote_states(context, namespace, "mint", quote, &["PAID", "ISSUED"])?;
    ensure!(
        recognized["amount"] == 100_000 && recognized["request"] == invoice,
        "recovered quote terms differ"
    );
    context.record("bark-mint-recovered-paid.json", &recognized)?;
    if recognized["state"] == "PAID" {
        native::execute(
            client,
            INSTANCE,
            RUN,
            "wallet",
            "bark-claim-original-quote",
            &format!("{WALLET} mint {MINT} --quote-id {}", native::quote(quote)),
        )?;
    }
    let issued = quote_state(context, namespace, "mint", quote, "ISSUED")?;
    context.record("bark-mint-issued.json", &issued)?;
    balance(context, client, "bark-wallet-minted", 100_000)?;
    Ok(quote.into())
}

fn check_payer(payment: &Value, hash: &str, amount: u64) -> Result<()> {
    ensure!(
        payment["status"] == "complete"
            && payment["payment_hash"] == hash
            && payment["amount_msat"] == amount,
        "payer did not settle the original mint invoice"
    );
    Ok(())
}

pub(super) fn melt(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    id: &str,
    amount: u64,
    before: u64,
) -> Result<u64> {
    let label = format!("bark-melt-{id}");
    let invoice = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        &format!("{label}-invoice"),
        &format!(
            "{PEER} invoice {}msat {label} 'Bark acceptance'",
            amount * 1000
        ),
    )?;
    let log = format!("/wallet/{label}.log");
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        &label,
        &format!(
            "{WALLET} melt --mint-url {MINT} --invoice {} > {} 2>&1",
            native::quote(expect::string(&invoice, "/bolt11")?),
            native::quote(&log)
        ),
    )?;
    let receipt = native::json_output(
        client,
        INSTANCE,
        RUN,
        "wallet",
        &format!("{label}-receipt"),
        &format!(
            "/opt/proofstorm/driver cdk-melt-receipt {}",
            native::quote(&log)
        ),
    )?;
    let output = context
        .kubectl
        .exec(namespace, "deployment/wallet", &["cat", &log])?;
    let quote = melt_quote_id(&output)?;
    let settled = quote_state(context, namespace, "melt", quote, "PAID")?;
    let received = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        &format!("{label}-recipient"),
        &format!("{PEER} listinvoices {label}"),
    )?;
    let remaining = check_melt(
        &receipt,
        &settled,
        &received,
        expect::string(&invoice, "/payment_hash")?,
        amount,
        before,
    )?;
    for (kind, value) in [
        ("receipt", &receipt),
        ("quote", &settled),
        ("recipient", &received),
    ] {
        context.record(&format!("{label}-{kind}.json"), value)?;
    }
    balance(context, client, &format!("{label}-balance"), remaining)?;
    Ok(remaining)
}

fn melt_quote_id(output: &str) -> Result<&str> {
    let quotes = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Quote ID: "))
        .collect::<Vec<_>>();
    ensure!(
        quotes.len() == 1 && !quotes[0].is_empty(),
        "missing or ambiguous native melt quote"
    );
    Ok(quotes[0])
}

fn check_melt(
    receipt: &Value,
    quote: &Value,
    received: &Value,
    hash: &str,
    amount: u64,
    before: u64,
) -> Result<u64> {
    ensure!(
        receipt["state"] == "PAID"
            && receipt["amount_sat"] == amount
            && quote["state"] == "PAID"
            && quote["amount"] == amount,
        "melt receipt and quote disagree"
    );
    let invoices = expect::array(received, "/invoices")?;
    ensure!(
        invoices.len() == 1
            && invoices[0]["status"] == "paid"
            && invoices[0]["payment_hash"] == hash
            && invoices[0]["amount_received_msat"] == amount * 1000,
        "recipient settlement does not match the melt"
    );
    let fee = expect::integer(receipt, "/fee_paid_sat")?;
    ensure!(
        fee <= expect::integer(quote, "/fee_reserve")?,
        "melt exceeded its fee reserve"
    );
    before
        .checked_sub(amount)
        .and_then(|n| n.checked_sub(fee))
        .context("melt exceeded the wallet balance")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settlement_requires_recipient_hash_amount_and_fee_conservation() {
        let receipt = json!({"state":"PAID","amount_sat":30_000,"fee_paid_sat":7});
        let quote = json!({"state":"PAID","amount":30_000,"fee_reserve":10});
        let recipient = json!({"invoices":[{"status":"paid","payment_hash":"hash","amount_received_msat":30_000_000}]});
        assert_eq!(
            check_melt(&receipt, &quote, &recipient, "hash", 30_000, 100_000).unwrap(),
            69_993
        );
        assert!(check_melt(&receipt, &quote, &recipient, "other", 30_000, 100_000).is_err());
        assert!(check_melt(&receipt, &quote, &recipient, "hash", 30_001, 100_000).is_err());
        assert!(check_melt(&receipt, &quote, &recipient, "hash", 30_000, 30_001).is_err());
        let mut wrong = receipt.clone();
        wrong["fee_paid_sat"] = json!(11);
        assert!(check_melt(&wrong, &quote, &recipient, "hash", 30_000, 100_000).is_err());
        assert!(
            check_melt(
                &receipt,
                &quote,
                &json!({"invoices":[]}),
                "hash",
                30_000,
                100_000
            )
            .is_err()
        );
        assert!(melt_quote_id("Quote ID: a\nQuote ID: b").is_err());
        assert!(melt_quote_id("no quote").is_err());
        assert_eq!(melt_quote_id("Quote ID: a").unwrap(), "a");
    }
    #[test]
    fn mint_payment_must_match_the_original_quote() {
        let payment = json!({"status":"complete","payment_hash":"hash","amount_msat":100_000_000});
        check_payer(&payment, "hash", 100_000_000).unwrap();
        assert!(check_payer(&payment, "other", 100_000_000).is_err());
        assert!(check_payer(&payment, "hash", 100_000).is_err());
        assert!(check_payer(&Value::Null, "hash", 100_000_000).is_err());
    }
}
