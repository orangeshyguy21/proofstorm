use super::{
    CLN, GateContext, INSTANCE, McpClient, PEER, RUN, Result, STACK, Value, balance, ensure,
    expect, http, json, native, quote_state, stack,
};

/// Bark's payment stack in restart order. Each component runs one pod.
const COMPONENTS: &[(&str, usize)] = &[
    ("cln", 1),
    ("postgres", 1),
    ("ark", 1),
    ("processor", 1),
    ("mint", 1),
];

pub(super) fn restart_stack(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    phase: &str,
) -> Result<()> {
    STACK.restart_stack(
        context,
        client,
        namespace,
        phase,
        COMPONENTS,
        |client, phase| identities(context, client, namespace, phase),
    )
}

fn identities(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    phase: &str,
) -> Result<Value> {
    let fingerprints = STACK.fingerprints(
        context,
        client,
        namespace,
        phase,
        &[
            "processor-identity",
            "processor-payment-tls",
            "cln-cln-tls",
            "cln-hold-tls",
            "mint-secrets",
        ],
        &[
            ("ark", "/data/.proofstorm-bark-server"),
            ("processor", "/data/.proofstorm-bark-identity"),
            ("cln", "/data/.proofstorm-cln-hold"),
        ],
    )?;
    let node = native::json_output(
        client,
        INSTANCE,
        RUN,
        "cln",
        &format!("bark-{phase}-cln-id"),
        &format!("{CLN} getinfo"),
    )?;
    let keysets = stack::mint_keysets(context, namespace)?;
    Ok(
        json!({"fingerprints":fingerprints,"cln":expect::string(&node,"/id")?,"mint_keysets":keysets}),
    )
}

pub(super) fn unpaid(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    let quote = http::post_json(
        &forward.url("/v1/mint/quote/bolt11"),
        &json!({"amount":2000,"unit":"sat"}),
    )?;
    ensure!(
        quote["state"] == "UNPAID" && quote["amount"] == 2000,
        "fresh quote is not unpaid"
    );
    let id = expect::string(&quote, "/quote")?;
    context.record("bark-unpaid-before.json", &quote)?;
    let decoded = native::json_output(
        client,
        INSTANCE,
        RUN,
        "peer",
        "bark-unpaid-decode",
        &format!(
            "{PEER} decode {}",
            native::quote(expect::string(&quote, "/request")?)
        ),
    )?;
    ensure!(
        decoded["valid"] == true
            && decoded["type"] == "bolt11 invoice"
            && expect::integer(&decoded, "/amount_msat")? == 2_000_000,
        "unpaid quote invoice is invalid or its amount differs"
    );
    let hash = expect::string(&decoded, "/payment_hash")?;
    let before = unpaid_hold(client, hash, "before")?;
    drop(forward);
    restart_stack(context, client, namespace, "unpaid")?;
    let after = quote_state(context, namespace, "mint", id, "UNPAID")?;
    ensure!(
        after["request"] == quote["request"]
            && after["amount"] == quote["amount"]
            && after["unit"] == quote["unit"],
        "unpaid quote terms changed"
    );
    context.record("bark-unpaid-after.json", &after)?;
    let after = unpaid_hold(client, hash, "after")?;
    ensure!(
        before == after,
        "native hold invoice changed across backend restarts"
    );
    context.record("bark-unpaid-hold-recovered.json", &after)?;
    balance(context, client, "bark-unpaid-no-credit", 0)
}

fn unpaid_hold(client: &mut McpClient, hash: &str, phase: &str) -> Result<Value> {
    let result = native::json_output(
        client,
        INSTANCE,
        RUN,
        "cln",
        &format!("bark-unpaid-hold-{phase}"),
        &format!("{CLN} listholdinvoices {}", native::quote(hash)),
    )?;
    let invoices = expect::array(&result, "/holdinvoices")?;
    ensure!(
        invoices.len() == 1
            && invoices[0]["payment_hash"] == hash
            && invoices[0]["state"] == "unpaid"
            && expect::array(&invoices[0], "/htlcs")?.is_empty(),
        "native unpaid hold invoice differs"
    );
    Ok(result)
}
