//! Quote source-of-truth composition: a receive quote created through native
//! wallet authority can be paid by the typed operation, and an externally
//! paid typed invoice can be completed through the explicit claim operation.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::{GateContext, json as expect, lab};

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
    "lease.acquire",
    "lease.release",
    "wallet.create",
    "wallet.control",
    "wallet.fund",
    "chain.mine",
    "peer.connect",
    "channel.open",
    "component.forensics",
    "artifact.read",
];

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "quote-composition",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "quote-mint"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "quote-payer"}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.0", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Quote Composition Mint"}},
            {"id": "payer-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
            {"id": "recipient-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}}
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

fn scoped(instance: &str, experiment: &str, lease: &str, operation: &str, extra: Value) -> Value {
    let mut request = json!({
        "instance_id": instance,
        "experiment_id": experiment,
        "lease_id": lease,
        "operation_id": operation
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
    let draft = format!("quote-composition-{run}");
    let instance = format!("quote-composition-instance-{run}");
    let experiment = format!("quote-composition-experiment-{run}");
    let lease = format!("quote-composition-lease-{run}");
    let mut client = context.session(&workspace, "quote-agent", CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": draft, "lab": lab_document(), "idempotency_key": format!("create-{run}")}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": draft, "expected_version": 1, "idempotency_key": format!("publish-{run}"), "include_revision": true}),
    )?;
    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": instance, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": format!("materialize-{run}")}),
    )?;
    lab::wait_phase(&mut client, &instance, "ready", 200, Duration::from_secs(3))?;
    let status = client.call("proofstorm_lab_status", json!({"instance_id": instance}))?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_owned();

    client.call(
        "proofstorm_experiment_create",
        json!({"experiment_id": experiment, "instance_id": instance, "idempotency_key": format!("experiment-{run}")}),
    )?;
    client.call(
        "proofstorm_lease_acquire",
        json!({"experiment_id": experiment, "lease_id": lease, "duration_seconds": 1200, "max_actions": 12, "idempotency_key": format!("lease-{run}")}),
    )?;
    client.call(
        "proofstorm_liquidity_bootstrap",
        scoped(
            &instance,
            &experiment,
            &lease,
            "bootstrap",
            json!({
                "chain": "chain", "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
                "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000,
                "idempotency_key": format!("bootstrap-{run}")
            }),
        ),
    )?;
    lab::wait_operation(&mut client, "bootstrap", 180)?;

    for (operation, wallet) in [
        ("initialize-payer", "payer-wallet"),
        ("initialize-recipient", "recipient-wallet"),
    ] {
        client.call(
            "proofstorm_wallet_initialize",
            scoped(&instance, &experiment, &lease, operation, json!({
                "wallet": wallet, "mint": "mint", "idempotency_key": format!("{operation}-{run}")
            })),
        )?;
        lab::wait_operation(&mut client, operation, 120)?;
    }
    client.call(
        "proofstorm_wallet_fund",
        scoped(
            &instance,
            &experiment,
            &lease,
            "fund-payer",
            json!({
                "wallet": "payer-wallet", "mint": "mint", "payer_lightning": "payer-lnd",
                "amount_sat": 1_000, "idempotency_key": format!("fund-{run}")
            }),
        ),
    )?;
    lab::wait_operation(&mut client, "fund-payer", 160)?;

    let compose_script = r#"set -eu; cd /app; output=$(mktemp /tmp/quote.XXXXXX); trap 'rm -f "$output"' EXIT; python3 -c 'from cashu.wallet.cli.cli import cli; cli()' -h http://mint:3338 -u sat -w recipient-wallet -t -y invoice 100 --no-check >"$output" 2>&1; sed -n 's/.*--id \([0-9a-f-][0-9a-f-]*\).*/\1/p' "$output" | head -1"#;
    client.call(
        "proofstorm_component_forensics",
        scoped(&instance, &experiment, &lease, "compose-invoice", json!({
            "component": "recipient-wallet", "target_component": "mint", "script": compose_script,
            "timeout_seconds": 60, "idempotency_key": format!("compose-{run}")
        })),
    )?;
    let composed = lab::wait_operation(&mut client, "compose-invoice", 120)?;
    let composed_quote = uuid_from(native_output(&composed)?)?;

    let pay_request = scoped(
        &instance,
        &experiment,
        &lease,
        "compose-pay",
        json!({
            "wallet": "payer-wallet", "mint": "mint", "recipient_wallet": "recipient-wallet",
            "recipient_mint": "mint", "mint_quote_id": composed_quote,
            "idempotency_key": format!("compose-pay-{run}")
        }),
    );
    let accepted_pay = client.call("proofstorm_wallet_pay", pay_request)?;
    client.call_refused(
        "proofstorm_wallet_pay",
        scoped(
            &instance,
            &experiment,
            &lease,
            "compose-pay-racer",
            json!({
                "wallet": "payer-wallet", "mint": "mint", "recipient_wallet": "recipient-wallet",
                "recipient_mint": "mint", "mint_quote_id": composed_quote,
                "idempotency_key": format!("compose-pay-racer-{run}")
            }),
        ),
        "quote_payment_already_claimed",
    )?;
    let paid = lab::wait_operation(&mut client, "compose-pay", 160)?;
    let paid_content = lab::artifact_content(&paid)?;
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

    let accepted_invoice = client.call(
        "proofstorm_wallet_invoice",
        scoped(
            &instance,
            &experiment,
            &lease,
            "external-invoice",
            json!({
                "wallet": "recipient-wallet", "mint": "mint", "amount_sat": 200,
                "timeout_seconds": 300, "idempotency_key": format!("external-invoice-{run}")
            }),
        ),
    )?;
    let invoice_operation = lab::wait_operation(&mut client, "external-invoice", 120)?;
    let invoice_content = lab::artifact_content(&invoice_operation)?;
    let external_quote = expect::string(invoice_content, "/mint_quote_id")?.to_owned();
    assert_no_invoice(invoice_content, "typed invoice artifact")?;

    let read_script = format!(
        "python3 -c 'import glob,sqlite3; print(next(r[0] for p in glob.glob(\"/wallet/.cashu/recipient-wallet/*.sqlite3\") for r in [sqlite3.connect(p).execute(\"SELECT request FROM bolt11_mint_quotes WHERE quote = ?\", (\"{external_quote}\",)).fetchone()] if r))'"
    );
    client.call(
        "proofstorm_component_forensics",
        scoped(
            &instance,
            &experiment,
            &lease,
            "read-private-invoice",
            json!({
                "component": "recipient-wallet", "script": read_script, "timeout_seconds": 30,
                "idempotency_key": format!("read-private-{run}")
            }),
        ),
    )?;
    let private_read = lab::wait_operation(&mut client, "read-private-invoice", 90)?;
    if expect::integer(lab::artifact_content(&private_read)?, "/exit_code")? != 0 {
        bail!("native private invoice lookup failed: {private_read}");
    }
    let external_invoice = invoice_from(native_output(&private_read)?)?;
    let pay_invoice_script = format!(
        "set -eu; attempt=0; until lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=payer-lnd:10009 getinfo >/dev/null 2>&1; do attempt=$((attempt+1)); test \"$attempt\" -lt 30; sleep 1; done; lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=payer-lnd:10009 payinvoice --force '{external_invoice}'"
    );
    client.call(
        "proofstorm_component_forensics",
        scoped(
            &instance,
            &experiment,
            &lease,
            "external-lightning-pay",
            json!({
                "component": "payer-lnd", "script": pay_invoice_script, "timeout_seconds": 60,
                "idempotency_key": format!("external-pay-{run}")
            }),
        ),
    )?;
    let external_payment = lab::wait_operation(&mut client, "external-lightning-pay", 120)?;
    if expect::integer(lab::artifact_content(&external_payment)?, "/exit_code")? != 0 {
        bail!("external Lightning payment failed: {external_payment}");
    }

    let accepted_claim = client.call(
        "proofstorm_wallet_quote_claim",
        scoped(
            &instance,
            &experiment,
            &lease,
            "external-claim",
            json!({
                "wallet": "recipient-wallet", "mint": "mint", "mint_quote_id": external_quote,
                "timeout_seconds": 30, "idempotency_key": format!("external-claim-{run}")
            }),
        ),
    )?;
    let claimed = lab::wait_operation(&mut client, "external-claim", 120)?;
    let claim_content = lab::artifact_content(&claimed)?;
    if expect::string(claim_content, "/quote_observations/0/state")? != "ISSUED" {
        bail!("externally paid quote was not issued by explicit claim: {claimed}");
    }

    let quote_status = client.call(
        "proofstorm_wallet_quote_status",
        json!({"instance_id": instance, "wallet": "recipient-wallet", "mint": "mint", "direction": "receive", "quote_id": external_quote}),
    )?;
    let quote_list = client.call(
        "proofstorm_wallet_quote_list",
        json!({"experiment_id": experiment, "limit": 20}),
    )?;
    let journal = client.call(
        "proofstorm_action_list",
        json!({"experiment_id": experiment, "after_sequence": 0, "limit": 100}),
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
            "proofstormlabaction",
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
        "proofstorm_lease_release",
        json!({"lease_id": lease, "idempotency_key": format!("release-{run}")}),
    )?;
    client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id": experiment, "idempotency_key": format!("close-experiment-{run}")}),
    )?;
    let evidence = client.call(
        "proofstorm_artifact_export",
        json!({
            "experiment_id": experiment, "include_oracle_artifacts": false, "include_content": true,
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

    client.call("proofstorm_lab_close", json!({"instance_id": instance}))?;
    let closed = lab::wait_phase(
        &mut client,
        &instance,
        "closed",
        100,
        Duration::from_secs(3),
    )?;
    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("quote composition lab teardown was not verified: {closed}");
    }
    println!(
        "Quote composition acceptance passed: CLI-created typed pay, single-flight job admission, external payment claim, and typed non-disclosure are verified"
    );
    Ok(())
}
