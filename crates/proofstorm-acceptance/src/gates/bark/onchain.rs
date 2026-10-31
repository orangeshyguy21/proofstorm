//! Bark's advertised rails and an on-chain deposit boarded into Ark by the processor.
use super::{
    Duration, GateContext, INSTANCE, McpClient, RUN, Result, Value, ensure, expect, http, json,
    native, sleep,
};

/// Every rail the default processor configuration advertises.
pub(super) const METHODS: &str = "bolt11,onchain,arkoor";
const PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const DEPOSIT_SAT: u64 = 50_000;

/// The processor advertises exactly its default rails in sat.
pub(super) fn check_settings(settings: &Value) -> Result<()> {
    ensure!(
        settings["unit"] == "sat"
            && settings["bolt11"].is_object()
            && settings["bolt12"].is_null()
            && settings["onchain"]["confirmations"]
                .as_u64()
                .is_some_and(|confirmations| confirmations >= 1)
            && settings["custom"]
                .as_object()
                .is_some_and(|custom| custom.len() == 1 && custom["arkoor"].is_string()),
        "Bark capabilities differ from its {METHODS} sat profile"
    );
    Ok(())
}

fn bitcoin(sat: u64) -> String {
    format!("{}.{:08}", sat / 100_000_000, sat % 100_000_000)
}

/// Pay an on-chain mint quote from Bitcoin Core and require the original quote
/// to be credited once the processor has boarded the deposit. Runs after the
/// Lightning stages, since every observation mines a block.
pub(super) fn mint(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    // A new forward is not listening yet; a POST would be refused immediately.
    http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    let quote = http::post_json(
        &forward.url("/v1/mint/quote/onchain"),
        &json!({"unit":"sat","pubkey":PUBKEY}),
    )?;
    context.record("bark-onchain-quote.json", &quote)?;
    let id = expect::string(&quote, "/quote")?;
    let address = expect::string(&quote, "/request")?;
    ensure!(
        address.starts_with("bcrt1"),
        "processor did not return a regtest deposit address: {quote}"
    );
    let mut session = native::Session::new(client, INSTANCE, RUN);
    session.execute(
        "chain",
        "bark-onchain-deposit",
        &format!(
            "{} sendtoaddress {} {}",
            native::BITCOIN,
            native::quote(address),
            bitcoin(DEPOSIT_SAT)
        ),
    )?;
    // The processor boards after one confirmation; the board then needs the
    // server's confirmations before the quote is credited.
    for attempt in 0..60 {
        session.mine("chain", &format!("bark-onchain-confirm-{attempt}"), 1)?;
        let state =
            http::get_json_retrying(&mut forward, &format!("/v1/mint/quote/onchain/{id}"), 3)?;
        ensure!(state["quote"] == id, "on-chain quote identity changed");
        let paid = state["amount_paid"].as_u64().unwrap_or(0);
        if paid > 0 {
            context.record(
                "bark-onchain-paid.json",
                &json!({"deposit_sat":DEPOSIT_SAT,"quote":state}),
            )?;
            // The board fee is deducted from the credited amount.
            ensure!(
                paid < DEPOSIT_SAT,
                "on-chain mint credited the deposit without its board fee: {state}"
            );
            return Ok(());
        }
        sleep(Duration::from_secs(2));
    }
    anyhow::bail!("boarded on-chain deposit did not credit its original mint quote")
}
