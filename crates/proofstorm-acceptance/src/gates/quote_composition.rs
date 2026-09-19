//! Quote source-of-truth composition: a receive quote created through native
//! wallet authority can be paid by the typed operation, and an externally
//! paid typed invoice can be completed through the explicit claim operation.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, json as expect};

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

fn native_output(operation: &Value) -> Result<&str> {
    operation
        .pointer("/artifact/content/combined_output")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("native operation has no combined output: {operation}"))
}

fn uuid_from(output: &str) -> Result<String> {
    output
        .split(|character: char| !character.is_ascii_hexdigit() && character != '-')
        .find(|token| {
            token.len() == 36 && token.chars().filter(|character| *character == '-').count() == 4
        })
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("native output contains no quote UUID: {output}"))
}

fn invoice_from(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .map(|token| token.trim_matches(|character: char| !character.is_ascii_alphanumeric()))
        .find(|token| token.starts_with("lnbcrt"))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("native output contains no regtest invoice"))
}

fn scoped(instance: &str, experiment: &str, operation: &str, extra: Value) -> Value {
    let mut request = json!({
        "name": instance,
        "run_id": experiment,

        "request_id": operation
    });
    let Value::Object(fields) = extra else {
        panic!("scoped fields must be an object");
    };
    request
        .as_object_mut()
        .expect("scoped request")
        .extend(fields);
    request
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
    let status = crate::cell::status(&mut client, &(instance))?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_owned();

    client.call(
        "run_start",
        json!({"request_id":"5413","run_id": experiment, "name": instance}),
    )?;

    crate::driver::liquidity_bootstrap(
        context,
        &mut client,
        scoped(
            &instance,
            &experiment,
            "bootstrap",
            json!({
                "chain": "chain", "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
                "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000}),
        ),
    )?;
    cell::wait_operation(&mut client, "bootstrap", 180)?;

    for (operation, wallet) in [
        ("initialize-payer", "payer-wallet"),
        ("initialize-recipient", "recipient-wallet"),
    ] {
        crate::driver::wallet_initialize(
            context,
            &mut client,
            scoped(
                &instance,
                &experiment,
                operation,
                json!({
                "wallet": wallet, "mint": "mint"}),
            ),
        )?;
        cell::wait_operation(&mut client, operation, 120)?;
    }
    crate::driver::wallet_fund(
        context,
        &mut client,
        scoped(
            &instance,
            &experiment,
            "fund-payer",
            json!({
                "wallet": "payer-wallet", "mint": "mint", "payer_lightning": "payer-lnd",
                "amount_sat": 1_000}),
        ),
    )?;
    cell::wait_operation(&mut client, "fund-payer", 160)?;

    let compose_script = r#"set -eu; cd /app; output=$(mktemp /tmp/quote.XXXXXX); trap 'rm -f "$output"' EXIT; cashu -h http://mint:3338 -u sat -w wallet -t -y invoice 100 --no-check >"$output" 2>&1; sed -n 's/.*--id \([0-9a-f-][0-9a-f-]*\).*/\1/p' "$output" | head -1"#;
    client.call(
        "component_forensics",
        scoped(
            &instance,
            &experiment,
            "compose-invoice",
            json!({
            "component": "recipient-wallet", "script": compose_script,
            "timeout_seconds": 60}),
        ),
    )?;
    let composed = cell::wait_operation(&mut client, "compose-invoice", 120)?;
    let composed_quote = uuid_from(native_output(&composed)?)?;

    let pay_request = scoped(
        &instance,
        &experiment,
        "compose-pay",
        json!({
            "wallet": "payer-wallet", "mint": "mint", "recipient_wallet": "recipient-wallet",
            "recipient_mint": "mint", "mint_quote_id": composed_quote}),
    );
    let accepted_pay = crate::driver::wallet_pay(context, &mut client, pay_request)?;
    let refusal = crate::driver::wallet_pay(
        context,
        &mut client,
        scoped(
            &instance,
            &experiment,
            "compose-pay-racer",
            json!({
                "wallet": "payer-wallet", "mint": "mint", "recipient_wallet": "recipient-wallet",
                "recipient_mint": "mint", "mint_quote_id": composed_quote}),
        ),
    )
    .expect_err("an already claimed quote must reject a second payer");
    anyhow::ensure!(
        refusal
            .downcast_ref::<proofstorm_store::StoreError>()
            .is_some_and(|error| error.code() == "quote_payment_already_claimed"),
        "unexpected claim refusal: {refusal}"
    );
    let paid = cell::wait_operation(&mut client, "compose-pay", 160)?;
    let paid_content = cell::artifact_content(&paid)?;
    if expect::string(paid_content, "/quote_observations/0/state")? != "PAID"
        || expect::string(paid_content, "/quote_observations/1/state")? != "ISSUED"
        || expect::integer(paid_content, "/recipient_balance_sat")? != 100
    {
        bail!("composed quote did not pay and issue: {paid}");
    }
    let pay_resource = expect::string(&accepted_pay, "/resource_name")?;
    let pay_jobs = context.kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        &namespace,
        "-l",
        &format!("proofstorm.dev/action={pay_resource}"),
    ])?;
    if expect::array(&pay_jobs, "/items")?.len() != 1 {
        bail!("single-flight admission created more than one payment job: {pay_jobs}");
    }

    let accepted_invoice = crate::driver::wallet_invoice(
        context,
        &mut client,
        scoped(
            &instance,
            &experiment,
            "external-invoice",
            json!({
                "wallet": "recipient-wallet", "mint": "mint", "amount_sat": 200,
                "timeout_seconds": 300}),
        ),
    )?;
    let invoice_operation = cell::wait_operation(&mut client, "external-invoice", 120)?;
    let invoice_content = cell::artifact_content(&invoice_operation)?;
    let external_quote = expect::string(invoice_content, "/mint_quote_id")?.to_owned();
    assert_no_invoice(invoice_content, "typed invoice artifact")?;

    let read_script = format!(
        "/opt/proofstorm/driver private-invoice /wallet recipient-wallet http://mint:3338 {external_quote}"
    );
    client.call(
        "component_forensics",
        scoped(
            &instance,
            &experiment,
            "read-private-invoice",
            json!({
                "component": "recipient-wallet", "script": read_script, "timeout_seconds": 30}),
        ),
    )?;
    let private_read = cell::wait_operation(&mut client, "read-private-invoice", 90)?;
    if expect::integer(cell::artifact_content(&private_read)?, "/exit_code")? != 0 {
        bail!("native private invoice lookup failed: {private_read}");
    }
    let external_invoice = invoice_from(native_output(&private_read)?)?;
    let pay_invoice_script = format!(
        "set -eu; attempt=0; until lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=payer-lnd:10009 getinfo >/dev/null 2>&1; do attempt=$((attempt+1)); test \"$attempt\" -lt 30; sleep 1; done; lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=payer-lnd:10009 payinvoice --force '{external_invoice}'"
    );
    client.call(
        "component_forensics",
        scoped(
            &instance,
            &experiment,
            "external-lightning-pay",
            json!({
                "component": "payer-lnd", "script": pay_invoice_script, "timeout_seconds": 60}),
        ),
    )?;
    let external_payment = cell::wait_operation(&mut client, "external-lightning-pay", 120)?;
    if expect::integer(cell::artifact_content(&external_payment)?, "/exit_code")? != 0 {
        bail!("external Lightning payment failed: {external_payment}");
    }

    let accepted_claim = crate::driver::wallet_quote_claim(
        context,
        &mut client,
        scoped(
            &instance,
            &experiment,
            "external-claim",
            json!({
                "wallet": "recipient-wallet", "mint": "mint", "mint_quote_id": external_quote,
                "timeout_seconds": 30}),
        ),
    )?;
    let claimed = cell::wait_operation(&mut client, "external-claim", 120)?;
    let claim_content = cell::artifact_content(&claimed)?;
    if expect::string(claim_content, "/quote_observations/0/state")? != "ISSUED" {
        bail!("externally paid quote was not issued by explicit claim: {claimed}");
    }

    let quote_status = crate::driver::quote_status(
        context,
        &mut client,
        json!({"name": instance, "wallet": "recipient-wallet", "mint": "mint", "direction": "receive", "quote_id": external_quote}),
    )?;
    let quote_list = crate::driver::quote_observations(
        context,
        &mut client,
        json!({"run_id": experiment, "limit": 20}),
    )?;
    let journal = Ok::<_, anyhow::Error>(
        json!({"actions":crate::cell::journal(&mut client, &(experiment))?}),
    )?;
    for (value, label) in [
        (&paid, "typed pay operation"),
        (&invoice_operation, "typed invoice operation"),
        (&claimed, "typed claim operation"),
        (&quote_status, "typed quote status"),
        (&quote_list, "typed quote list"),
        (&journal, "action journal"),
    ] {
        assert_no_invoice(value, label)?;
    }

    for resource in [
        pay_resource,
        expect::string(&accepted_invoice, "/resource_name")?,
        expect::string(&accepted_claim, "/resource_name")?,
    ] {
        let action = context.kubectl.get_json(&[
            "get",
            "proofstormcellaction",
            resource,
            "-n",
            "proofstorm-system",
        ])?;
        assert_no_invoice(&action, "typed action CR")?;
        let (_, logs, stderr) = context.kubectl.try_run(&[
            "logs",
            "-n",
            &namespace,
            &format!("job/{resource}"),
            "--all-containers=true",
        ])?;
        let combined = format!("{logs}\n{stderr}").to_ascii_lowercase();
        if combined.contains("lnbcrt") || combined.contains("payment_request") {
            bail!("typed action pod logs disclosed a Lightning invoice");
        }
    }

    client.call(
        "run_finish",
        json!({"request_id":"13809","run_id": experiment}),
    )?;
    let evidence = crate::cell::evidence(
        &mut client,
        json!({
            "run_id": experiment, "include_oracle_artifacts": false,
            "artifact_operation_ids": ["compose-pay", "external-invoice", "external-claim"]
        }),
    )?;
    let mut typed_evidence = evidence.clone();
    for action in typed_evidence
        .pointer_mut("/content/journal")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if action.get("kind").and_then(Value::as_str) == Some("component_forensics") {
            action["request"] =
                Value::String("component_forensics intentionally secret-bearing".into());
        }
    }
    assert_no_invoice(
        &typed_evidence,
        "typed evidence outside component_forensics requests",
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
        "Quote composition acceptance passed: CLI-created typed pay, single-flight job admission, external payment claim, and typed non-disclosure are verified"
    );
    Ok(())
}
