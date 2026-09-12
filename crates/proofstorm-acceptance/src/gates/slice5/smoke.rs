use super::common::{
    EXPERIMENT, INSTANCE, action_kinds, kinds_by_operation, scoped, submit_idempotent,
};
use crate::{GateContext, McpClient, cell, json as expect};
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub(super) fn run(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    instance_key: &str,
) -> Result<()> {
    let kubectl = &context.kubectl;
    submit_idempotent(
        client,
        "wallet_initialize",
        scoped(
            "wallet-initialize",
            json!({"wallet": "wallet", "mint": "mint", "idempotency_key": "wallet-initialize-slice5"}),
        ),
        "wallet-initialize",
    )?;
    let initialized = cell::wait_operation(client, "wallet-initialize", 120)?;
    if !expect::boolean(cell::artifact_content(&initialized)?, "/initialized")? {
        bail!("wallet-initialize artifact is invalid: {initialized}");
    }

    client.call(
        "wallet_balance",
        scoped(
            "wallet-balance",
            json!({"wallet": "wallet", "mint": "mint", "idempotency_key": "wallet-balance-slice5"}),
        ),
    )?;
    let balance = cell::wait_operation(client, "wallet-balance", 120)?;
    if expect::integer(cell::artifact_content(&balance)?, "/balance_sat")? != 0 {
        bail!("new wallet did not have a zero sanitized balance: {balance}");
    }

    submit_idempotent(
        client,
        "wallet_fund",
        scoped(
            "wallet-fund",
            json!({"wallet": "wallet", "mint": "mint", "payer_lightning": "payer-lnd", "amount_sat": 1000, "idempotency_key": "wallet-fund-slice5"}),
        ),
        "wallet-fund",
    )?;
    let funded = cell::wait_operation(client, "wallet-fund", 120)?;
    let fund_result = cell::artifact_content(&funded)?;
    if expect::integer(fund_result, "/funded_sat")? != 1000
        || expect::integer(fund_result, "/balance_sat")? != 1000
    {
        bail!("wallet-fund artifact is invalid: {funded}");
    }

    let accepted_wallet = submit_idempotent(
        client,
        "wallet_round_trip",
        scoped(
            "round-trip",
            json!({"wallet": "wallet", "mint": "mint", "payer_lightning": "payer-lnd", "amount_sat": 1000, "tolerance_sat": 100, "idempotency_key": "round-trip-slice5"}),
        ),
        "wallet",
    )?;
    let wallet_resource = expect::string(&accepted_wallet, "/resource_name")?.to_string();
    let kinds = kinds_by_operation(&action_kinds(context, instance_key)?)?;
    if kinds.get("round-trip").map(String::as_str) != Some("wallet_round_trip") {
        bail!("wallet request did not create a typed runtime action: {kinds:?}");
    }
    let round_trip = cell::wait_operation(client, "round-trip", 120)?;
    let wallet_result = cell::artifact_content(&round_trip)?;
    if expect::boolean(wallet_result, "/inflation")? {
        bail!("round-trip artifact is invalid: {round_trip}");
    }
    expect::integer(wallet_result, "/balance_after_swap_sat")?;
    let wallet_jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        namespace,
        "-l",
        &format!("proofstorm.dev/action={wallet_resource}"),
    ])?;
    if expect::array(&wallet_jobs, "/items")?.len() != 1 {
        bail!("caller retry duplicated the controller-owned wallet Job");
    }

    // --- private invoice and pay -------------------------------------------
    client.call(
        "wallet_initialize",
        scoped(
            "receiver-initialize",
            json!({"wallet": "receiver-wallet", "mint": "mint", "idempotency_key": "receiver-initialize-slice5"}),
        ),
    )?;
    let receiver_initialized = cell::wait_operation(client, "receiver-initialize", 120)?;
    if expect::integer(
        cell::artifact_content(&receiver_initialized)?,
        "/balance_sat",
    )? != 0
    {
        bail!("receiver wallet did not initialize empty: {receiver_initialized}");
    }

    submit_idempotent(
        client,
        "wallet_invoice",
        scoped(
            "wallet-invoice",
            json!({"wallet": "receiver-wallet", "mint": "mint", "amount_sat": 100, "timeout_seconds": 300, "idempotency_key": "wallet-invoice-slice5"}),
        ),
        "invoice",
    )?;
    let invoice = cell::wait_operation(client, "wallet-invoice", 120)?;
    let invoice_content = cell::artifact_content(&invoice)?.clone();
    let mint_quote_id = expect::string(&invoice_content, "/mint_quote_id")?.to_string();
    if expect::string(&invoice_content, "/quote_observations/0/role")? != "invoice_receive"
        || expect::string(&invoice_content, "/quote_observations/0/direction")? != "receive"
        || expect::string(&invoice_content, "/quote_observations/0/state")? != "UNPAID"
        || expect::integer(&invoice_content, "/quote_observations/0/amount_sat")? != 100
    {
        bail!("non-blocking wallet invoice artifact is invalid: {invoice}");
    }
    let quote = client.call(
        "wallet_quote_status",
        json!({"instance_id": INSTANCE, "wallet": "receiver-wallet", "mint": "mint", "direction": "receive", "quote_id": mint_quote_id}),
    )?;
    if expect::string(&quote, "/last_observation/state")? != "UNPAID"
        || expect::integer(&quote, "/last_observation/amount_sat")? != 100
    {
        bail!("initial receive observation was not stored: {quote}");
    }

    client.call(
        "wallet_balance",
        scoped(
            "wallet-balance-before-pay",
            json!({"wallet": "wallet", "mint": "mint", "idempotency_key": "wallet-balance-before-pay-slice5"}),
        ),
    )?;
    let baseline = cell::wait_operation(client, "wallet-balance-before-pay", 120)?;
    if expect::integer(cell::artifact_content(&baseline)?, "/balance_sat")? < 100 {
        bail!("wallet baseline is invalid: {baseline}");
    }

    submit_idempotent(
        client,
        "wallet_pay",
        scoped(
            "wallet-pay",
            json!({"wallet": "wallet", "mint": "mint", "recipient_wallet": "receiver-wallet", "recipient_mint": "mint", "mint_quote_id": mint_quote_id, "idempotency_key": "wallet-pay-slice5"}),
        ),
        "pay",
    )?;
    let paid = cell::wait_operation(client, "wallet-pay", 120)?;
    let paid_content = cell::artifact_content(&paid)?.clone();
    if expect::string(&paid_content, "/quote_observations/0/role")? != "payment_melt"
        || expect::string(&paid_content, "/quote_observations/0/state")? != "PAID"
        || expect::string(&paid_content, "/quote_observations/1/role")? != "payment_receive"
        || expect::string(&paid_content, "/quote_observations/1/state")? != "ISSUED"
        || expect::integer(&paid_content, "/recipient_balance_sat")? != 100
    {
        bail!("wallet pay artifact is invalid: {paid}");
    }
    let quote = client.call(
        "wallet_quote_status",
        json!({"instance_id": INSTANCE, "wallet": "receiver-wallet", "mint": "mint", "direction": "receive", "quote_id": mint_quote_id}),
    )?;
    if expect::string(&quote, "/last_observation/state")? != "ISSUED" {
        bail!("receive quote was not observed as issued: {quote}");
    }
    let quote_list = client.call(
        "wallet_quote_list",
        json!({"experiment_id": EXPERIMENT, "limit": 10}),
    )?;
    let listed = expect::array(&quote_list, "/last_observations")?;
    if listed.len() != 2
        || !listed.iter().any(|observation| {
            observation.get("direction").and_then(Value::as_str) == Some("pay")
                && observation.get("state").and_then(Value::as_str) == Some("PAID")
        })
        || !listed.iter().any(|observation| {
            observation.get("direction").and_then(Value::as_str) == Some("receive")
                && observation.get("state").and_then(Value::as_str) == Some("ISSUED")
        })
    {
        bail!("quote observation list is not canonical: {quote_list}");
    }
    let serialized_flow = serde_json::to_string(
        &json!({"quote": quote, "pay": paid_content, "invoice": invoice_content}),
    )?
    .to_lowercase();
    for forbidden in ["lnbcrt", "payment_request", "adapter_quote", "mnemonic"] {
        if serialized_flow.contains(forbidden) {
            bail!("private payment material crossed MCP in quote flow: {forbidden}");
        }
    }

    // --- conservation oracle ------------------------------------------------
    let accepted_oracle = submit_idempotent(
        client,
        "conservation_oracle",
        scoped(
            "conservation",
            json!({"wallet": "wallet", "mint": "mint", "baseline_operation_id": "wallet-balance-before-pay", "treatment_operation_id": "wallet-pay", "idempotency_key": "conservation-slice5"}),
        ),
        "oracle",
    )?;
    let oracle_resource = expect::string(&accepted_oracle, "/resource_name")?.to_string();
    let kinds = kinds_by_operation(&action_kinds(context, instance_key)?)?;
    if kinds.contains_key("conservation") {
        bail!("receipt-only conservation unexpectedly created a runtime action: {kinds:?}");
    }
    let oracle = cell::wait_operation(client, "conservation", 120)?;
    if !expect::boolean(cell::artifact_content(&oracle)?, "/conserved")? {
        bail!("oracle artifact is invalid: {oracle}");
    }
    let oracle_jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        namespace,
        "-l",
        &format!("proofstorm.dev/action={oracle_resource}"),
    ])?;
    if !expect::array(&oracle_jobs, "/items")?.is_empty() {
        bail!("receipt-only conservation unexpectedly created a Job");
    }

    Ok(())
}
