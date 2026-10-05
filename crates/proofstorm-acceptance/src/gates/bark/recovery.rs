use super::{
    CLN, Context, Duration, GateContext, INSTANCE, McpClient, PEER, RUN, Result, Value, balance,
    cell, control, ensure, expect, http, json, native, quote_state, sleep,
};
use std::collections::BTreeMap;

pub(super) fn restart_stack(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    phase: &str,
) -> Result<()> {
    let before = identities(context, client, namespace, &format!("{phase}-before"))?;
    for component in ["cln", "postgres", "ark", "processor", "mint"] {
        restart(
            context,
            client,
            namespace,
            component,
            &format!("bark-{phase}-restart-{component}"),
        )?;
    }
    let after = identities(context, client, namespace, &format!("{phase}-after"))?;
    ensure!(
        before == after,
        "Bark identities or retained state seals changed on restart"
    );
    context.record(
        &format!("bark-{phase}-identities.json"),
        &json!({"before":before,"after":after}),
    )
}

fn identities(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    phase: &str,
) -> Result<Value> {
    let mut fingerprints = BTreeMap::new();
    for name in [
        "processor-identity",
        "processor-payment-tls",
        "cln-cln-tls",
        "cln-hold-tls",
        "mint-secrets",
    ] {
        let secret =
            context
                .kubectl
                .get_json(&["get", &format!("secret/{name}"), "-n", namespace])?;
        ensure!(secret["data"].is_object(), "missing identity Secret data");
        fingerprints.insert(name, proofstorm_core::digest_json(&secret["data"]));
    }
    for (component, marker) in [
        ("ark", "/data/.proofstorm-bark-server"),
        ("processor", "/data/.proofstorm-bark-identity"),
        ("cln", "/data/.proofstorm-cln-hold"),
    ] {
        fingerprints.insert(
            component,
            native::stdout(
                client,
                INSTANCE,
                RUN,
                component,
                &format!("bark-{phase}-{component}-seal"),
                &format!("sha256sum {marker}"),
            )?,
        );
    }
    let node = native::json_output(
        client,
        INSTANCE,
        RUN,
        "cln",
        &format!("bark-{phase}-cln-id"),
        &format!("{CLN} getinfo"),
    )?;
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let keysets = http::get_json_retrying(&mut forward, "/v1/keysets", 30)?;
    ensure!(
        !expect::array(&keysets, "/keysets")?.is_empty(),
        "missing mint keysets"
    );
    Ok(
        json!({"fingerprints":fingerprints,"cln":expect::string(&node,"/id")?,"mint_keysets":keysets}),
    )
}

pub(super) fn restart(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    component: &str,
    id: &str,
) -> Result<()> {
    let before =
        ready_pod(context, namespace, component)?.context("component has no single ready pod")?;
    control(client, "component_restart", component, id)?;
    for _ in 0..120 {
        if let Some(after) = ready_pod(context, namespace, component)?
            && before != after
        {
            cell::wait_ready_recorded(context, client, INSTANCE)?;
            return context.record(
                &format!("{id}.json"),
                &json!({"component":component,"previous_pod_uid":before,"ready_pod_uid":after}),
            );
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("{component} did not replace its pod and become ready")
}

fn ready_pod(context: &GateContext, namespace: &str, component: &str) -> Result<Option<String>> {
    let pods = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        &format!("{}={component}", proofstorm_kube::COMPONENT_LABEL),
    ])?;
    let ready = expect::array(&pods, "/items")?
        .iter()
        .filter(|pod| {
            pod["metadata"]["deletionTimestamp"].is_null()
                && pod["status"]["conditions"]
                    .as_array()
                    .is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|c| c["type"] == "Ready" && c["status"] == "True")
                    })
        })
        .collect::<Vec<_>>();
    if ready.len() != 1 {
        return Ok(None);
    }
    Ok(Some(expect::string(ready[0], "/metadata/uid")?.into()))
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
