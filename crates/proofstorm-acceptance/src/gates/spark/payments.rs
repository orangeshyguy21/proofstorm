//! BOLT11 mints and melts through the Spark processor, each confirmed by the
//! counterparty LDK node's own payment records.
use super::{
    Duration, GateContext, INSTANCE, LDK, MINT, McpClient, RUN, Result, Value, WALLET, balance,
    cell, control, ensure, expect, funding, http, json, melt_quote_id, melt_quotes, melt_remaining,
    native, quote_state, quote_states, sleep, ssp_status, stack::capture_log,
};

const MINT_SAT: u64 = 100_000;
const UNREACHABLE_SAT: u64 = 1_000;

/// Decode an invoice on the peer and require it to pay the SSP's node exactly
/// `amount_msat`. Returns its payment hash.
pub(super) fn decode(
    client: &mut McpClient,
    id: &str,
    invoice: &str,
    amount_msat: u64,
    destination: &str,
) -> Result<String> {
    let decoded = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        id,
        &format!("{LDK} decode-invoice {}", native::quote(invoice)),
    )?;
    check_invoice(&decoded, amount_msat, destination).map(str::to_owned)
}

pub(super) fn check_invoice<'a>(
    decoded: &'a Value,
    amount_msat: u64,
    destination: &str,
) -> Result<&'a str> {
    ensure!(
        decoded["destination"] == destination && decoded["amount_msat"] == amount_msat,
        "invoice does not pay the SSP's node the quoted amount"
    );
    expect::string(decoded, "/payment_hash")
}

/// Whether one LDK payment record for `hash` has succeeded in `direction` for
/// exactly `amount_msat`. Pending is `false`; a failure is an error.
pub(super) fn settled(
    details: &Value,
    hash: &str,
    direction: &str,
    amount_msat: u64,
) -> Result<bool> {
    let payment = &details["payment"];
    ensure!(
        payment
            .pointer("/kind/kind/bolt11/hash")
            .and_then(Value::as_str)
            == Some(hash)
            && payment["direction"] == direction,
        "native payment identity differs"
    );
    match expect::string(payment, "/status")? {
        "PENDING" => Ok(false),
        "SUCCEEDED" => {
            ensure!(
                payment["amount_msat"] == amount_msat,
                "native payment amount differs"
            );
            Ok(true)
        }
        status => anyhow::bail!("native payment reached {status}"),
    }
}

/// Poll a node's own record of one payment until it has succeeded.
fn wait_payment(
    client: &mut McpClient,
    id: &str,
    node: &str,
    payment: &str,
    hash: &str,
    direction: &str,
    amount_msat: u64,
) -> Result<Value> {
    native::Session::new(client, INSTANCE, RUN).poll(
        node,
        id,
        &format!("{LDK} get-payment-details {}", native::quote(payment)),
        |v| Ok(settled(v, hash, direction, amount_msat)?.then(|| v.clone())),
    )
}

pub(super) fn mint(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    nodes: &funding::Nodes,
) -> Result<String> {
    // Run the native wallet while the peer pays. It may claim on notification
    // or return after creating its quote; reconcile only that original quote.
    client.call("cell_exec", json!({"name":INSTANCE,"run_id":RUN,"component":"wallet","request_id":"spark-mint-request","script":format!("{WALLET} mint {MINT} {MINT_SAT} --wait-duration 240"),"timeout_seconds":300,"output":{"mode":"public"}}))?;
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        "spark-await-quote",
        &format!(
            "/opt/proofstorm/driver cdk-quote await UNPAID /wallet/cdk/cdk-cli.sqlite {MINT} {MINT_SAT}"
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
                &MINT_SAT.to_string(),
            ],
        )?);
    }
    let invoice = parts[0].trim();
    let quote = parts[1].trim();
    let hash = decode(
        client,
        "spark-decode-mint",
        invoice,
        MINT_SAT * 1000,
        &nodes.ssp,
    )?;
    control(
        client,
        "component_stop",
        "processor",
        "spark-stop-before-payment",
    )?;
    // The peer pays while the processor is stopped. The SSP can take the
    // payment on the processor's behalf, so either an in-flight or a settled
    // payment is an interruption; only a failed one is not.
    let sent = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        "spark-pay-mint",
        &format!("{LDK} bolt11-send {}", native::quote(invoice)),
    )?;
    let payment = expect::string(&sent, "/payment_id")?;
    let mut observed = Value::Null;
    for attempt in 0..15 {
        observed = native::json_output(
            client,
            INSTANCE,
            RUN,
            "peer",
            &format!("spark-interrupted-payer-{attempt}"),
            &format!("{LDK} get-payment-details {}", native::quote(payment)),
        )?;
        if settled(&observed, &hash, "OUTBOUND", MINT_SAT * 1000)? {
            break;
        }
        sleep(Duration::from_secs(1));
    }
    context.record(
        "spark-payment-interrupted.json",
        &json!({"processor":"stopped","payer":observed}),
    )?;
    control(
        client,
        "component_start",
        "processor",
        "spark-resume-processor",
    )?;
    cell::wait_ready_recorded(context, client, INSTANCE)?;
    let paid = wait_payment(
        client,
        "spark-mint-payer",
        "peer",
        payment,
        &hash,
        "OUTBOUND",
        MINT_SAT * 1000,
    )?;
    context.record("spark-mint-payer.json", &paid)?;
    // A processor restart can lose the streamed payment notification. CDK's
    // quote-status check reconciles its original quote against the backend and
    // notifies the waiting wallet. Do this after independent settlement and
    // before waiting for the wallet, rather than waiting on the missed event.
    let recognized = quote_states(context, namespace, "mint", quote, &["PAID", "ISSUED"])?;
    ensure!(
        recognized["amount"] == MINT_SAT && recognized["request"] == invoice,
        "recovered quote terms differ"
    );
    context.record("spark-mint-recovered-paid.json", &recognized)?;
    native::wait(client, "spark-mint-request")?;
    let claimed = quote_states(context, namespace, "mint", quote, &["PAID", "ISSUED"])?;
    if claimed["state"] == "PAID" {
        native::execute(
            client,
            INSTANCE,
            RUN,
            "wallet",
            "spark-claim-original-quote",
            &format!("{WALLET} mint {MINT} --quote-id {}", native::quote(quote)),
        )?;
    }
    let issued = quote_state(context, namespace, "mint", quote, "ISSUED")?;
    context.record("spark-mint-issued.json", &issued)?;
    balance(context, client, "spark-wallet-minted", MINT_SAT)?;
    Ok(quote.into())
}

/// A settled melt: the wallet's remaining balance, and whether the mint's
/// quote carried a payment preimage.
pub(super) struct Melted {
    pub(super) remaining: u64,
    pub(super) preimage: bool,
}

pub(super) fn melt(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    id: &str,
    amount: u64,
    before: u64,
) -> Result<Melted> {
    let label = format!("spark-melt-{id}");
    let invoice = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        &format!("{label}-invoice"),
        &format!("{LDK} bolt11-receive {amount}sat -d {label}"),
    )?;
    let log = format!("/wallet/{label}.log");
    context.record(&format!("{label}-invoice.json"), &invoice)?;
    let result = (|| {
        native::submit(
            client,
            INSTANCE,
            RUN,
            "wallet",
            &label,
            &format!(
                "{WALLET} melt --mint-url {MINT} --invoice {} > {} 2>&1",
                native::quote(expect::string(&invoice, "/invoice")?),
                native::quote(&log)
            ),
        )?;
        // LDK identifies an inbound BOLT11 payment by its payment hash.
        let hash = expect::string(&invoice, "/payment_hash")?;
        let received = wait_payment(
            client,
            &format!("{label}-recipient"),
            "peer",
            hash,
            hash,
            "INBOUND",
            amount * 1000,
        )?;
        context.record(&format!("{label}-recipient.json"), &received)?;
        // As in Bark, first prove recipient settlement, then reconcile the
        // existing quote. A lost notification must never trigger a second send.
        let output = context
            .kubectl
            .exec(namespace, "deployment/wallet", &["cat", &log])?;
        let quote = melt_quote_id(&output)?;
        let settled = quote_state(context, namespace, "melt", quote, "PAID")?;
        context.record(&format!("{label}-quote.json"), &settled)?;
        native::wait(client, &label)?;
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
        context.record(&format!("{label}-receipt.json"), &receipt)?;
        let remaining = melt_remaining(&receipt, &settled, amount, before)?;
        balance(context, client, &format!("{label}-balance"), remaining)?;
        // A missing preimage is a documented limitation: record it, never fail on it.
        let preimage = carries_preimage(&settled);
        context.record(
            &format!("{label}-preimage.json"),
            &json!({"quote":quote,"payment_preimage":preimage}),
        )?;
        Ok(Melted {
            remaining,
            preimage,
        })
    })();
    capture_log(context, namespace, &label, &log)?;
    result
}

/// Melt to an invoice of the channel-less island. The mint must never report
/// it paid, and the wallet must keep its proofs. A failed Spark send can stay
/// pending for 16 days (docker/payment/SPARK.md); the SSP's status is recorded
/// around the attempt, but its resolution is not required.
pub(super) fn unreachable(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    before: u64,
) -> Result<()> {
    let label = "spark-melt-unreachable";
    let invoice = native::json_output(
        client,
        INSTANCE,
        RUN,
        "island",
        &format!("{label}-invoice"),
        &format!("{LDK} bolt11-receive {UNREACHABLE_SAT}sat -d {label}"),
    )?;
    context.record(&format!("{label}-invoice.json"), &invoice)?;
    let status = native::json_output(
        client,
        INSTANCE,
        RUN,
        "ssp",
        &format!("{label}-ssp-before"),
        &ssp_status(),
    )?;
    context.record(&format!("{label}-ssp-before.json"), &status)?;
    let log = format!("/wallet/{label}.log");
    let result = (|| {
        // The native exit status is not the economic outcome. Keep it in the
        // log and judge settlement from the mint, the island and the wallet.
        native::execute(
            client,
            INSTANCE,
            RUN,
            "wallet",
            label,
            &format!(
                "timeout 100 {WALLET} melt --mint-url {MINT} --invoice {} > {log} 2>&1; echo \"exit status $?\" >> {log}",
                native::quote(expect::string(&invoice, "/invoice")?),
                log = native::quote(&log)
            ),
        )?;
        let output = context
            .kubectl
            .exec(namespace, "deployment/wallet", &["cat", &log])?;
        // The mint may refuse to quote an unroutable invoice at all.
        let quote = match melt_quotes(&output)[..] {
            [] => Value::Null,
            [quote] => refused(context, namespace, quote)?,
            _ => anyhow::bail!("missing or ambiguous native melt quote"),
        };
        context.record(&format!("{label}-quote.json"), &json!({"quote":quote}))?;
        let hash = expect::string(&invoice, "/payment_hash")?;
        let island = native::json_output(
            client,
            INSTANCE,
            RUN,
            "island",
            &format!("{label}-recipient"),
            &format!("{LDK} get-payment-details {}", native::quote(hash)),
        )?;
        ensure!(
            !settled(&island, hash, "INBOUND", UNREACHABLE_SAT * 1000)?,
            "the channel-less island received a payment"
        );
        context.record(&format!("{label}-recipient.json"), &island)?;
        // Reconcile any proofs the wallet still marks pending against the
        // mint, as the manual probe did, before requiring every proof back.
        native::execute(
            client,
            INSTANCE,
            RUN,
            "wallet",
            &format!("{label}-check-pending"),
            &format!("{WALLET} check-pending"),
        )?;
        balance(context, client, &format!("{label}-balance"), before)
    })();
    // Record the SSP's side after the attempt even when a check above failed,
    // without letting this evidence replace that failure.
    let after = native::json_output(
        client,
        INSTANCE,
        RUN,
        "ssp",
        &format!("{label}-ssp-after"),
        &ssp_status(),
    )
    .and_then(|status| context.record(&format!("{label}-ssp-after.json"), &status));
    capture_log(context, namespace, label, &log)?;
    result?;
    after
}

/// Poll an unroutable melt's original quote until the mint settles it unpaid.
fn refused(context: &GateContext, namespace: &str, quote: &str) -> Result<Value> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    for _ in 0..90 {
        let state =
            http::get_json_retrying(&mut forward, &format!("/v1/melt/quote/bolt11/{quote}"), 3)?;
        if unpaid(&state, quote)? {
            return Ok(state);
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("unroutable melt quote was not settled unpaid")
}

/// `true` once the mint reports the original quote unpaid. A paid report fails.
pub(super) fn unpaid(state: &Value, quote: &str) -> Result<bool> {
    ensure!(state["quote"] == quote, "quote identity changed");
    match expect::string(state, "/state")? {
        "UNPAID" => Ok(true),
        "PENDING" => Ok(false),
        other => anyhow::bail!("mint reported an unroutable melt {other}"),
    }
}

pub(super) fn carries_preimage(quote: &Value) -> bool {
    quote["payment_preimage"]
        .as_str()
        .is_some_and(|preimage| !preimage.is_empty())
}
