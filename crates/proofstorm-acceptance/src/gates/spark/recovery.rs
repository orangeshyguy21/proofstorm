//! Spark identities and an unpaid quote survive restarts of the whole payment stack.
use super::{
    GateContext, INSTANCE, LDK, McpClient, OPERATORS, RUN, Result, STACK, Value, balance, ensure,
    expect, funding, http, json, native, payments, quote_state, ssp_identity, stack,
};

/// The payment stack in dependency order, each with its pod count.
const COMPONENTS: &[(&str, usize)] = &[
    ("postgres", 1),
    ("operators", OPERATORS),
    ("ssp-node", 1),
    ("ssp", 1),
    ("processor", 1),
    ("mint", 1),
];
const UNPAID_SAT: u64 = 2000;

pub(super) fn restart_stack(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    nodes: &funding::Nodes,
    phase: &str,
) -> Result<()> {
    STACK.restart_stack(
        context,
        client,
        namespace,
        phase,
        COMPONENTS,
        |client, phase| identities(context, client, namespace, phase),
    )?;
    // The restarted node reestablishes its channel and catches up with the chain.
    funding::lightning(context, client, nodes, &format!("{phase}-restarted"))
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
            "operators-identity",
            "ssp-identity",
            "processor-identity",
            "processor-payment-tls",
            "mint-secrets",
        ],
        &[
            ("ssp", "/data/.proofstorm-open-ssp-identity"),
            ("processor", "/data/.proofstorm-spark-processor-identity"),
        ],
    )?;
    let ssp = native::json_output(
        client,
        INSTANCE,
        RUN,
        "ssp",
        &format!("spark-{phase}-ssp-identity"),
        &ssp_identity(),
    )?;
    let node = native::json_output(
        client,
        INSTANCE,
        RUN,
        "ssp-node",
        &format!("spark-{phase}-ssp-node-id"),
        &format!("{LDK} get-node-info"),
    )?;
    let keysets = stack::mint_keysets(context, namespace)?;
    Ok(json!({
        "fingerprints":fingerprints,
        "ssp":expect::string(&ssp,"/identityPublicKey")?,
        "ssp_node":expect::string(&node,"/node_id")?,
        "mint_keysets":keysets
    }))
}

pub(super) fn unpaid(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    nodes: &funding::Nodes,
) -> Result<()> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    let quote = http::post_json(
        &forward.url("/v1/mint/quote/bolt11"),
        &json!({"amount":UNPAID_SAT,"unit":"sat"}),
    )?;
    ensure!(
        quote["state"] == "UNPAID" && quote["amount"] == UNPAID_SAT,
        "fresh quote is not unpaid"
    );
    let id = expect::string(&quote, "/quote")?;
    context.record("spark-unpaid-before.json", &quote)?;
    let hash = payments::decode(
        client,
        "spark-unpaid-decode",
        expect::string(&quote, "/request")?,
        UNPAID_SAT * 1000,
        &nodes.ssp,
    )?;
    let before = unpaid_receive(client, &hash, "before")?;
    drop(forward);
    restart_stack(context, client, namespace, nodes, "unpaid")?;
    let after = quote_state(context, namespace, "mint", id, "UNPAID")?;
    ensure!(
        after["request"] == quote["request"]
            && after["amount"] == quote["amount"]
            && after["unit"] == quote["unit"],
        "unpaid quote terms changed"
    );
    context.record("spark-unpaid-after.json", &after)?;
    let after = unpaid_receive(client, &hash, "after")?;
    ensure!(
        before == after,
        "native unpaid receive changed across backend restarts"
    );
    context.record("spark-unpaid-receive-recovered.json", &after)?;
    balance(context, client, "spark-unpaid-no-credit", 0)
}

/// open-ssp creates the quote's invoice on its own LDK node for the
/// processor's payment hash. That node must not have received it.
fn unpaid_receive(client: &mut McpClient, hash: &str, phase: &str) -> Result<Value> {
    let details = payments::inbound(
        &native::json_output(
            client,
            INSTANCE,
            RUN,
            "ssp-node",
            &format!("spark-unpaid-receive-{phase}"),
            &format!("{LDK} {}", payments::LIST_PAYMENTS),
        )?,
        hash,
    )?;
    ensure!(
        payments::receive_unpaid(&details, hash, UNPAID_SAT * 1000)?,
        "unpaid quote's invoice was paid"
    );
    Ok(details)
}
