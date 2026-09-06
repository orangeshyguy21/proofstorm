//! First CDK wallet checkpoint. Native money operations, passive observations,
//! isolated state, restart persistence, retained evidence and verified cleanup.
//! The operator relays BOLT11 requests privately via stdin; no ecash relay is
//! implemented or claimed by this gate.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, McpClient, json as expect, lab};

const INSTANCE: &str = "cdk-wallet-instance";
const EXPERIMENT: &str = "cdk-wallet-experiment";
const LEASE: &str = "cdk-wallet-session";
const CLI: &str = "timeout -k 2 45 cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive";
const LN: &str = "lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=127.0.0.1:10009";

fn document(input_fee_ppk: u64) -> Value {
    json!({
        "api_version":"proofstorm/v1alpha1", "name":"cdk-wallet-checkpoint",
        "components":[
            {"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"30.0","config_version":"bitcoin-core/30/v1","control":"laboratory","config":{}},
            {"id":"mint-lnd","kind":"lightning","implementation":"lnd","version":"0.20.0-beta","config_version":"lnd/0.20/v1","control":"laboratory","config":{}},
            {"id":"payer-lnd","kind":"lightning","implementation":"lnd","version":"0.20.0-beta","config_version":"lnd/0.20/v1","control":"laboratory","config":{}},
            {"id":"mint","kind":"mint","implementation":"cdk","version":"0.18.0","config_version":"cdk-mintd/0.18/v1","control":"target","config":{"input_fee_ppk":input_fee_ppk}},
            {"id":"wallet-a","kind":"wallet","implementation":"cdk-cli-wallet","version":"0.18.0","config_version":"cdk-cli-wallet/0.18/v1","control":"laboratory","config":{}},
            {"id":"wallet-b","kind":"wallet","implementation":"cdk-cli-wallet","version":"0.18.0","config_version":"cdk-cli-wallet/0.18/v1","control":"laboratory","config":{}}
        ],
        "links":[
            {"id":"mint-chain","kind":"chain_backend","from":"mint-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"payer-chain","kind":"chain_backend","from":"payer-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"mint-backend","kind":"payment_backend","from":"mint","to":"mint-lnd","binding":{"type":"payment","method":"bolt11","unit":"sat"}}
        ],
        "policy":{"allow":["component.exec_live","component.control"],"limits":{"max_components":8,"max_links":16,"max_config_bytes":16384}}
    })
}

fn scoped(operation: &str, parameters: Value) -> Value {
    let Value::Object(mut request) = parameters else {
        panic!("parameters must be an object")
    };
    request.extend(
        json!({"instance_id":INSTANCE,"experiment_id":EXPERIMENT,"session_id":LEASE,
        "operation_id":operation,"idempotency_key":operation})
        .as_object()
        .expect("scope")
        .clone(),
    );
    Value::Object(request)
}

fn save(directory: &Path, name: &str, value: &Value) -> Result<()> {
    fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value)?,
    )?;
    Ok(())
}

fn operation(
    client: &mut McpClient,
    directory: &Path,
    tool: &str,
    id: &str,
    args: Value,
) -> Result<Value> {
    client.call(tool, scoped(id, args))?;
    let result = match lab::wait_operation(client, id, 60) {
        Ok(result) => result,
        Err(error) => {
            if let Ok(failed) =
                client.call("proofstorm_operation_status", json!({"operation_id": id}))
            {
                save(directory, id, &failed)?;
            }
            return Err(error);
        }
    };
    save(directory, id, &result)?;
    Ok(lab::artifact_content(&result)?.clone())
}

fn native(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    wallet: &str,
    script: &str,
) -> Result<String> {
    let result = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        id,
        json!({"component":wallet,"script":format!("umask 077; {script}"),"timeout_seconds":60,"output":{"mode":"public"}}),
    )?;
    if result.get("exit_code") != Some(&json!(0))
        || result.get("timed_out") != Some(&json!(false))
        || result.get("cleanup_verified") != Some(&json!(true))
        || result.get("streams_complete") != Some(&json!(true))
        || result.get("output_truncated") != Some(&json!(false))
    {
        bail!("native command {id} failed: {result}");
    }
    Ok(expect::string(&result, "/stdout")?.trim().into())
}

fn balance(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    wallet: &str,
    expected: u64,
) -> Result<()> {
    let observed = operation(
        client,
        directory,
        "proofstorm_wallet_balance",
        id,
        json!({"wallet":wallet,"mint":"mint"}),
    )?;
    if expect::integer(&observed, "/balance_sat")? != expected
        || expect::integer(&observed, "/reserved_sat")? != 0
        || expect::integer(&observed, "/pending_sat")? != 0
        || expect::integer(&observed, "/pending_spent_sat")? != 0
    {
        bail!("unexpected wallet balance: {observed}");
    }
    Ok(())
}

fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    directory: &Path,
    namespace: &str,
    input_fee_ppk: u64,
) -> Result<()> {
    native(
        client,
        directory,
        "native-help",
        "wallet-a",
        "cdk-cli --help",
    )?;
    let mut identities = Vec::new();
    for wallet in ["wallet-a", "wallet-b"] {
        identities.push(native(
            client,
            directory,
            &format!("initialize-{wallet}"),
            wallet,
            &format!("{CLI} balance >/dev/null && sha256sum /wallet/cdk/seed"),
        )?);
        balance(client, directory, &format!("empty-{wallet}"), wallet, 0)?;
    }
    if identities[0] == identities[1] {
        bail!("wallets share seed identity");
    }
    client.call_refused(
        "proofstorm_wallet_initialize",
        scoped(
            "unsupported-initialize",
            json!({"wallet":"wallet-a","mint":"mint"}),
        ),
        "runtime_control_unsupported",
    )?;
    client.call_refused("proofstorm_wallet_fund",
        scoped("unsupported-fund",json!({"wallet":"wallet-a","mint":"mint","payer_lightning":"payer-lnd","amount_sat":1000})),
        "runtime_control_unsupported")?;

    operation(
        client,
        directory,
        "proofstorm_liquidity_bootstrap",
        "bootstrap",
        json!({
            "chain":"chain","mint_lightning":"mint-lnd","payer_lightning":"payer-lnd",
            "funding_sat":50_000_000,"channel_sat":10_000_000,"push_sat":5_000_000
        }),
    )?;

    // Interrupt a genuinely started CLI while its real quote is unpaid. Passive
    // observation must work during the command; resumption uses that exact quote.
    client.call("proofstorm_component_exec_live", scoped("interrupted-funding", json!({
        "component":"wallet-a", "argv":["cdk-cli","--work-dir","/wallet/cdk","--unit","sat","--non-interactive","mint","http://mint:3338","5000","--wait-duration","240"],
        "timeout_seconds":300
    })))?;
    native(
        client,
        directory,
        "await-funding-quote",
        "wallet-a",
        "python3 - <<'PY'\nimport sqlite3,time\nfor _ in range(100):\n c=sqlite3.connect('file:/wallet/cdk/cdk-cli.sqlite?mode=ro',uri=True)\n rows=c.execute(\"SELECT id FROM mint_quote WHERE amount=5000 AND state='UNPAID'\").fetchall(); c.close()\n if len(rows)==1: break\n time.sleep(.1)\nelse: raise SystemExit('unpaid quote not observed')\nprint('real unpaid quote observed')\nPY",
    )?;
    balance(client, directory, "passive-during-funding", "wallet-a", 0)?;
    let active = client.call(
        "proofstorm_operation_status",
        json!({"operation_id":"interrupted-funding"}),
    )?;
    save(directory, "funding-before-cancel", &active)?;
    if active.get("phase") != Some(&json!("running")) {
        bail!("funding CLI was not live at interruption");
    }
    client.call(
        "proofstorm_action_cancel",
        json!({"operation_id":"interrupted-funding","idempotency_key":"cancel-interrupted-funding"}),
    )?;
    let cancelled = client.call(
        "proofstorm_operation_wait",
        json!({"operation_id":"interrupted-funding","timeout_seconds":30}),
    )?;
    save(directory, "funding-cancelled", &cancelled)?;
    let cancelled = lab::artifact_content(&cancelled)?;
    if cancelled.get("cancelled") != Some(&json!(true))
        || cancelled.get("cleanup_verified") != Some(&json!(true))
    {
        bail!("funding interruption lacked verified cleanup");
    }
    let invoice = context.kubectl.exec(namespace,"deployment/wallet-a", &["python3","-c",
        "import sqlite3; c=sqlite3.connect('file:/wallet/cdk/cdk-cli.sqlite?mode=ro',uri=True); rows=c.execute(\"SELECT request FROM mint_quote WHERE amount=5000 AND mint_url='http://mint:3338' AND state='UNPAID'\").fetchall(); assert len(rows)==1; print(rows[0][0])"])?;
    context.kubectl.exec_stdin(
        namespace,
        "statefulset/payer-lnd",
        &["sh", "-c", "umask 077; cat > /tmp/funding.invoice"],
        &invoice,
    )?;
    let payment = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        "funding-payment",
        json!({
            "component":"payer-lnd", "script":format!("exec {LN} sendpayment --force --json --timeout=30s --pay_req=\"$(cat /tmp/funding.invoice)\""),
            "timeout_seconds":45, "output":{"mode":"json_fields","fields":["status","failure_reason"]}
        }),
    )?;
    if payment.get("exit_code") != Some(&json!(0))
        || payment.get("cleanup_verified") != Some(&json!(true))
        || payment.pointer("/selected_output/status") != Some(&json!("SUCCEEDED"))
        || payment.get("projection_succeeded") != Some(&json!(true))
        || payment.get("streams_complete") != Some(&json!(true))
    {
        bail!("funding payment did not settle with verified cleanup: {payment}");
    }
    // v0.18.0's mint-pending checks pending proofs, despite its help text.
    // Resume the exact persisted quote through the native mint command.
    native(
        client,
        directory,
        "claim-funding",
        "wallet-a",
        &format!(
            "quote=$(python3 -c \"import sqlite3; c=sqlite3.connect('file:/wallet/cdk/cdk-cli.sqlite?mode=ro',uri=True); rows=c.execute('SELECT id FROM mint_quote WHERE amount=5000').fetchall(); assert len(rows)==1; print(rows[0][0])\") && {CLI} mint http://mint:3338 --quote-id \"$quote\" --wait-duration 10 >/wallet/claim.log 2>&1 && echo claim_command_completed"
        ),
    )?;
    balance(client, directory, "funded-wallet-a", "wallet-a", 5000)?;
    balance(client, directory, "isolated-wallet-b", "wallet-b", 0)?;
    let native_balance = native(
        client,
        directory,
        "independent-native-balance",
        "wallet-a",
        &format!("{CLI} balance"),
    )?;
    if !native_balance
        .lines()
        .any(|line| line.trim() == "0: http://mint:3338 5000 sat")
    {
        bail!("native balance did not corroborate funding: {native_balance}");
    }

    let fee_per_payment = if input_fee_ppk == 0 { 0 } else { 2 };
    let native_melt_fee = u64::from(input_fee_ppk != 0);
    // A fresh melt of an already-paid invoice can complete a preparation swap
    // before rejection. Its input fee is real even though no payment follows.
    let rejected_swap_fee = u64::from(input_fee_ppk != 0);
    for (id, amount, expected) in [
        ("first-payment", 700_u64, 4300_u64 - fee_per_payment),
        (
            "after-restart-payment",
            300,
            4000 - 2 * fee_per_payment - rejected_swap_fee,
        ),
    ] {
        if id == "after-restart-payment" {
            operation(
                client,
                directory,
                "proofstorm_component_restart",
                "restart-wallet-a",
                json!({"component":"wallet-a"}),
            )?;
            lab::wait_ready(client, INSTANCE)?;
            let identity = native(
                client,
                directory,
                "restart-identity",
                "wallet-a",
                "sha256sum /wallet/cdk/seed",
            )?;
            if identity != identities[0] {
                bail!("wallet seed changed through restart");
            }
            balance(
                client,
                directory,
                "restart-balance",
                "wallet-a",
                4300 - fee_per_payment - rejected_swap_fee,
            )?;
        }
        let invoice: Value = serde_json::from_str(&context.kubectl.exec(
            namespace,
            "statefulset/payer-lnd",
            &["sh", "-c", &format!("{LN} addinvoice --amt={amount}")],
        )?)?;
        let request = expect::string(&invoice, "/payment_request")?;
        // Exercise the native-first default without requiring an agent-side
        // parser. Independent settlement and passive debit establish the outcome.
        let private_direct = input_fee_ppk == 0 && id == "after-restart-payment";
        if private_direct {
            let receipt = operation(
                client,
                directory,
                "proofstorm_component_exec_live",
                id,
                json!({"component":"wallet-a",
                    "argv":["cdk-cli","--work-dir","/wallet/cdk","--unit","sat","--non-interactive",
                        "melt","--mint-url","http://mint:3338","--invoice",request],
                    "timeout_seconds":60}),
            )?;
            if receipt.get("exit_code") != Some(&json!(0))
                || receipt.get("exit_scope") != Some(&json!("command"))
                || receipt.get("output_mode") != Some(&json!("private"))
                || receipt.get("stdout") != Some(&json!(""))
                || receipt.get("stderr") != Some(&json!(""))
                || receipt.get("timed_out") != Some(&json!(false))
                || receipt.get("cleanup_verified") != Some(&json!(true))
                || receipt.get("streams_complete") != Some(&json!(true))
                || receipt.get("output_truncated") != Some(&json!(false))
            {
                bail!("private direct payment lacked a clean, non-disclosing receipt");
            }
        } else {
            context.kubectl.exec_stdin(
                namespace,
                "deployment/wallet-a",
                &["sh", "-c", "umask 077; cat > /wallet/recipient.invoice"],
                request,
            )?;
            let native_receipt = native(
                client,
                directory,
                id,
                "wallet-a",
                &format!(
                    "{CLI} melt --mint-url http://mint:3338 --invoice \"$(cat /wallet/recipient.invoice)\" >/wallet/{id}.log 2>&1; code=$?; rm -f /wallet/recipient.invoice; test \"$code\" -eq 0 || exit \"$code\"; python3 - <<'PY'\nimport json,re\nfrom pathlib import Path\nrows=re.findall(r'^Payment successful: state=(\\w+), amount=(\\d+), fee_paid=(\\d+)$',Path('/wallet/{id}.log').read_text(),re.M)\nassert len(rows)==1, 'missing native melt receipt'\nstate,amount,fee=rows[0]\nprint(json.dumps(dict(state=state,amount_sat=int(amount),fee_paid_sat=int(fee))))\nPY"
                ),
            )?;
            let native_receipt: Value = serde_json::from_str(&native_receipt)?;
            // Native fee_paid is the melt receipt, not all preceding swap input
            // fees. Assert the independent full wallet debit separately below.
            if native_receipt
                != json!({"state":"PAID","amount_sat":amount,"fee_paid_sat":native_melt_fee})
            {
                bail!("unexpected native melt receipt: {native_receipt}");
            }
        }
        balance(
            client,
            directory,
            &format!("{id}-balance"),
            "wallet-a",
            expected,
        )?;
        save(
            directory,
            &format!("{id}-accounting"),
            &json!({"input_fee_ppk":input_fee_ppk,
            "recipient_amount_sat":amount,"wallet_debit_sat":amount+fee_per_payment,
            "wallet_input_fees_sat":fee_per_payment,"native_melt_fee_paid_sat":if private_direct { None } else { Some(native_melt_fee) },
            "native_output_mode":if private_direct { "private" } else { "parsed_public" },"remaining_sat":expected}),
        )?;
        let hash = expect::string(&invoice, "/r_hash")?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid recipient payment hash");
        }
        // lncli emits byte fields as hex, unlike the REST/gRPC JSON representation.
        let settled: Value = serde_json::from_str(&context.kubectl.exec(
            namespace,
            "statefulset/payer-lnd",
            &["sh", "-c", &format!("{LN} lookupinvoice --rhash='{hash}'")],
        )?)?;
        if settled.get("settled") != Some(&json!(true))
            || settled.get("amt_paid_sat") != Some(&json!(amount.to_string()))
        {
            bail!("recipient invoice did not settle for the expected amount");
        }
        save(
            directory,
            &format!("{id}-recipient"),
            &json!({"settled":true,"amount_sat":amount}),
        )?;
        if id == "first-payment" {
            context.kubectl.exec_stdin(
                namespace,
                "deployment/wallet-a",
                &["sh", "-c", "umask 077; cat > /wallet/rejected.invoice"],
                request,
            )?;
            rejected_payment(client, directory, "already-paid-invoice")?;
            balance(
                client,
                directory,
                "after-paid-invoice-rejection",
                "wallet-a",
                expected - rejected_swap_fee,
            )?;
            save(
                directory,
                "paid-invoice-rejection-accounting",
                &json!({"input_fee_ppk":input_fee_ppk,
                "additional_payment_sat":0,"preparation_swap_fee_sat":rejected_swap_fee,
                "remaining_sat":expected-rejected_swap_fee,"same_operation_replay_executed_again":false}),
            )?;
        }
    }
    let insufficient_invoice: Value = serde_json::from_str(&context.kubectl.exec(
        namespace,
        "statefulset/payer-lnd",
        &["sh", "-c", &format!("{LN} addinvoice --amt=10000")],
    )?)?;
    context.kubectl.exec_stdin(
        namespace,
        "deployment/wallet-a",
        &["sh", "-c", "umask 077; cat > /wallet/rejected.invoice"],
        expect::string(&insufficient_invoice, "/payment_request")?,
    )?;
    rejected_payment(client, directory, "insufficient-funds")?;
    balance(
        client,
        directory,
        "after-insufficient-funds",
        "wallet-a",
        4000 - 2 * fee_per_payment - rejected_swap_fee,
    )?;
    let unpaid: Value = serde_json::from_str(&context.kubectl.exec(
        namespace,
        "statefulset/payer-lnd",
        &[
            "sh",
            "-c",
            &format!(
                "{LN} lookupinvoice --rhash='{}'",
                expect::string(&insufficient_invoice, "/r_hash")?
            ),
        ],
    )?)?;
    if unpaid.get("settled") != Some(&json!(false)) {
        bail!("insufficient-funds invoice unexpectedly settled");
    }
    save(
        directory,
        "insufficient-funds-recipient",
        &json!({"settled":false,"amount_sat":10000}),
    )?;
    balance(client, directory, "final-isolation", "wallet-b", 0)?;
    native(
        client,
        directory,
        "native-process-cleanup",
        "wallet-a",
        "python3 - <<'PY'\nfrom pathlib import Path\nactive=[]\nfor proc in Path('/proc').iterdir():\n if not proc.name.isdigit(): continue\n try: exe=(proc/'exe').resolve(strict=True).name\n except (OSError,RuntimeError): continue\n if exe=='cdk-cli': active.append(proc.name)\nassert not active, 'native wallet processes still running'\nprint('native wallet processes absent')\nPY",
    )?;
    Ok(())
}

fn rejected_payment(client: &mut McpClient, directory: &Path, id: &str) -> Result<()> {
    let args = json!({
        "component":"wallet-a", "script":format!("cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive melt --mint-url http://mint:3338 --invoice \"$(cat /wallet/rejected.invoice)\" > /wallet/{id}.log 2>&1"),
        "timeout_seconds":45
    });
    let receipt = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        id,
        args.clone(),
    )?;
    if receipt
        .get("exit_code")
        .and_then(Value::as_i64)
        .is_none_or(|code| code == 0)
        || receipt.get("timed_out") != Some(&json!(false))
        || receipt.get("cleanup_verified") != Some(&json!(true))
    {
        bail!("expected definite native rejection with verified cleanup: {receipt}");
    }
    // Idempotent transport replay must preserve the original receipt, not
    // perform another potentially fee-bearing native attempt.
    client.call("proofstorm_component_exec_live", scoped(id, args))?;
    let replay = lab::wait_operation(client, id, 60)?;
    save(directory, &format!("{id}-idempotent-replay"), &replay)?;
    if lab::artifact_content(&replay)? != &receipt {
        bail!("idempotent native replay changed the terminal receipt");
    }
    let expected_reason = if id == "already-paid-invoice" {
        "already paid"
    } else {
        "not enough funds"
    };
    native(
        client,
        directory,
        &format!("{id}-reason"),
        "wallet-a",
        &format!(
            "python3 - <<'PY'\nfrom pathlib import Path\ns=Path('/wallet/{id}.log').read_text().lower()\nassert '{expected_reason}' in s, 'unexpected private rejection reason'\nprint('expected native rejection reason verified')\nPY"
        ),
    )?;
    Ok(())
}

pub fn run(context: &GateContext) -> Result<()> {
    run_with_fee(context, 0)
}

pub fn run_with_fee(context: &GateContext, input_fee_ppk: u64) -> Result<()> {
    let directory = context
        .root
        .join("dev/wallet-integration-runs")
        .join(&context.run_id);
    fs::create_dir_all(&directory)?;
    let mut capabilities = crate::EXPERIMENT_CAPABILITIES.to_vec();
    capabilities.extend(["component.exec_live", "component.control", "action.cancel"]);
    let mut client = context.session(
        &format!("cdk-wallet-{}", context.run_id),
        "experiment-agent",
        &capabilities,
    )?;
    client.call(
        "proofstorm_lab_create",
        json!({"draft_id":"cdk-wallet","lab":document(input_fee_ppk),"idempotency_key":"create"}),
    )?;
    let published = client.call("proofstorm_lab_publish",json!({"draft_id":"cdk-wallet","expected_version":1,"idempotency_key":"publish","include_revision":true}))?;
    save(&directory, "published", &published)?;
    let locked = lab::lock_entry(&published, "cdk-cli-wallet")?;
    if locked.pointer("/build_provenance/commit_sha")
        != Some(&json!("d3dec24c784e8fec1fd65f853241c7a2261c7abd"))
    {
        bail!("wallet lock omitted source provenance");
    }
    client.call("proofstorm_lab_materialize",json!({"instance_id":INSTANCE,"revision_digest":expect::string(&published,"/digest")?,"idempotency_key":"materialize"}))?;
    // Always attempt normal cleanup after materialization, including failed gates.
    let result = (|| -> Result<()> {
        let ready = lab::wait_ready(&mut client, INSTANCE)?;
        save(&directory, "ready", &ready)?;
        client.call("proofstorm_experiment_create",json!({"experiment_id":EXPERIMENT,"instance_id":INSTANCE,"idempotency_key":"experiment"}))?;
        client.call(
            "proofstorm_session_start",
            json!({"experiment_id":EXPERIMENT,"session_id":LEASE,"idempotency_key":"session"}),
        )?;
        exercise(
            context,
            &mut client,
            &directory,
            expect::string(&ready, "/instance_namespace")?,
            input_fee_ppk,
        )
    })();
    let _ = client.call(
        "proofstorm_session_finish",
        json!({"session_id":LEASE,"idempotency_key":"release"}),
    );
    let _ = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id":EXPERIMENT,"idempotency_key":"close-experiment"}),
    );
    let evidence = client.call(
        "proofstorm_artifact_export",
        json!({"experiment_id":EXPERIMENT,"include_content":true}),
    );
    if let Ok(export) = &evidence {
        save(&directory, "evidence", export)?;
    }
    save(
        &directory,
        "outcome",
        &json!({"passed": result.is_ok(),
        "error": result.as_ref().err().map(|error| format!("{error:#}"))}),
    )?;
    client.call("proofstorm_lab_close", json!({"instance_id":INSTANCE}))?;
    let closed = lab::wait_closed(&mut client, INSTANCE)?;
    save(&directory, "closed", &closed)?;
    if closed.pointer("/teardown_receipt/verified_absent") != Some(&json!(true)) {
        bail!("cleanup did not prove absence: {closed}");
    }
    result.context(format!(
        "CDK wallet gate failed; evidence: {}",
        directory.display()
    ))?;
    evidence.context("required evidence export failed")?;
    println!(
        "CDK wallet checkpoint passed; evidence: {}",
        directory.display()
    );
    Ok(())
}
