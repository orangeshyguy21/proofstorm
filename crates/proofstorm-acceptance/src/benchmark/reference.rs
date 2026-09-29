//! Live known-good grader control. Never reported as a model attempt.
use super::{Context, events, observe_final, observer, read, save};
use crate::{GateContext, McpClient, cell, native};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::process::Command;

pub fn run(context: &GateContext, selected_task: &super::task::Task) -> Result<()> {
    let config = Context {
        root: context.root.clone(),
        work: context.work().into(),
        home: context.installation.home.clone(),
        mcp: context.artifacts.mcp.clone(),
        model: "reference-control".into(),
        harness: super::harness::Harness::Reference,
        task: selected_task.clone(),
    };
    let path = config.work.join("benchmark-context.json");
    save(&path, &json!(config))?;
    let mut command = Command::new(std::env::current_exe()?);
    crate::client::clear_runtime_environment(&mut command);
    command.arg("--benchmark-proxy").arg(path);
    let mut client = McpClient::from_command(command, "benchmark-reference-control")?;
    let task = &config.task;
    let document = task.document();
    client.call(
        "cell_up",
        json!({"name":&task.cell_name,"request_id":"reference-create","cell":document}),
    )?;
    cell::wait_ready_recorded(context, &mut client, &task.cell_name)?;
    native::bootstrap(
        &mut client,
        &task.cell_name,
        "",
        "bootstrap",
        task.role("chain"),
        task.role("backend"),
        task.role("payer"),
        2_000_000,
        1_000_000,
        500_000,
    )?;
    let mut session = native::Session::new(&mut client, &task.cell_name, "");
    session.nutshell_initialize(task.role("wallet"), task.role("mint"), "initialize-wallet")?;
    let quote = session.nutshell_invoice(
        task.role("wallet"),
        task.role("mint"),
        "mint-quote",
        task.amounts.mint_sat,
    )?;
    let invoice = session.nutshell_invoice_projection(
        task.role("wallet"),
        task.role("mint"),
        "mint-invoice",
        &quote,
        task.amounts.mint_sat,
    )?;
    session.projected(
        task.role("payer"),
        "fund-mint",
        &format!(
            "{} payinvoice --force --json {}",
            native::LND,
            native::quote(
                invoice["payment_request"]
                    .as_str()
                    .context("funding invoice")?
            )
        ),
        &json!({"mode":"json_fields","fields":["status","value_sat"]}),
    )?;
    session.nutshell_claim(
        task.role("wallet"),
        task.role("mint"),
        "claim",
        &quote,
        task.amounts.mint_sat,
    )?;
    session.client.call(
        "benchmark_checkpoint",
        json!({"stage":"funded","mint_quote_id":quote}),
    )?;
    let invoice = session.projected(
        task.role("recipient"),
        "recipient-invoice",
        &format!("{} addinvoice --amt={}", native::LND, task.amounts.melt_sat),
        &json!({"mode":"lnd_invoice"}),
    )?;
    let melt = if task.payment_expectation == super::task::PaymentExpectation::Settled {
        session.nutshell_melt(
            task.role("wallet"),
            task.role("mint"),
            "melt",
            invoice["payment_request"]
                .as_str()
                .context("recipient invoice")?,
            task.amounts.melt_sat,
        )?
    } else {
        let first = negative_melt(&config, &mut session, &invoice, "melt", &json!([]))?;
        let second = negative_melt(
            &config,
            &mut session,
            &invoice,
            "melt-retry",
            &json!([first["quote_id"]]),
        )?;
        ensure!(
            first["quote_id"] != second["quote_id"],
            "retry did not create a distinct quote"
        );
        save(
            &config.work.join("reference-retry-quotes.json"),
            &json!({"first":first,"second":second,"selected":first["quote_id"]}),
        )?;
        // Select the older quote deliberately. Neither recency nor invoice-only
        // correlation may replace the caller's exact quote identity.
        first
    };
    let remaining =
        session.nutshell_balance(task.role("wallet"), task.role("mint"), "remaining")?;
    let paid_sat = if task.payment_expectation == super::task::PaymentExpectation::Settled {
        task.amounts.melt_sat
    } else {
        0
    };
    session.client.call("benchmark_checkpoint",json!({"stage":task.final_checkpoint,"mint_quote_id":quote,"melt_quote_id":melt["quote_id"],"payment_hash":invoice["payment_hash"],"minted_sat":task.amounts.mint_sat,"paid_sat":paid_sat,"remaining_sat":remaining}))?;
    session
        .client
        .call("cell_remove", json!({"name":&task.cell_name}))?;
    cell::wait_closed(session.client, &task.cell_name)?;
    let mut report = json!({"success":true,"minted_sat":task.amounts.mint_sat,"paid_sat":paid_sat,"remaining_sat":remaining,"cleanup":true});
    if task.payment_expectation == super::task::PaymentExpectation::UnpaidNoRoute {
        report["payment_occurred"] = json!(false);
        report["diagnosis"] = json!("no_route");
    }
    observe_final(
        &config,
        &super::report::Report::parse(&report.to_string()),
        false,
    )?;
    let assertions = read(&config.work.join("benchmark-observations.json"))?;
    ensure!(
        assertions
            .as_object()
            .is_some_and(|values| values.values().all(|value| value == true)),
        "known-good grader control failed: {assertions}"
    );
    // Counterexample uses the same real payment observations: another successful
    // funding payment must fail accounting even if net holdings are unchanged.
    let funded = read(&config.work.join("funded.json"))?;
    let mut paid = read(&config.work.join(format!("{}.json", task.final_checkpoint)))?;
    let truth = observer::report_truth(task, &funded, &paid, &assertions);
    paid["payer_payments"]["payments"]
        .as_array_mut()
        .context("payment list")?
        .push(json!({"status":"SUCCEEDED","value_sat":task.amounts.melt_sat.to_string(),"payment_request":"extra"}));
    ensure!(
        observer::assertions(task, &funded, &paid)["accounting"] == false,
        "offsetting cycle control was accepted"
    );
    let negative_controls =
        if task.payment_expectation == super::task::PaymentExpectation::UnpaidNoRoute {
            super::negative::controls(
                task,
                &funded,
                &read(&config.work.join(format!("{}.json", task.final_checkpoint)))?,
            )?
        } else {
            json!({})
        };
    let prose = super::report::Report::parse(&format!("Payment completed.\n{report}"));
    ensure!(
        !prose.format_valid && prose.consistent(task, &truth),
        "report format and claims were conflated"
    );
    let mut wrong = report.clone();
    wrong["paid_sat"] = json!(task.amounts.melt_sat + 1);
    let wrong = super::report::Report::parse(&wrong.to_string());
    ensure!(!wrong.consistent(task, &truth), "incorrect report accepted");
    let mut failed_observations = assertions.clone();
    failed_observations["agent_cleanup"] = json!(false);
    let failed_truth = observer::report_truth(task, &funded, &paid, &failed_observations);
    let mut honest = report.clone();
    honest["success"] = json!(false);
    honest["cleanup"] = json!(false);
    ensure!(
        super::report::Report::parse(&honest.to_string()).consistent(task, &failed_truth)
            && !super::report::Report::parse(&report.to_string()).consistent(task, &failed_truth),
        "honest failure and false completion were not distinguished"
    );
    save(
        &config.work.join("oracle-reference.json"),
        &json!({"kind":"known-good-grader-control","model_attempt":false,"task":task,"assertions":assertions,"negative_controls":negative_controls,"boundary_calls":super::calls(&events(&config.work)?)?.len(),"controls":{"extra_payment_rejected":true,"prose_preserves_claims_not_format_credit":true,"incorrect_report_rejected":true,"honest_failure_accepted_false_completion_rejected":true}}),
    )?;
    Ok(())
}

fn negative_melt(
    config: &Context,
    session: &mut native::Session<'_>,
    invoice: &Value,
    id: &str,
    before: &Value,
) -> Result<Value> {
    let task = &config.task;
    let request = invoice["payment_request"]
        .as_str()
        .context("recipient invoice")?;
    // A failed native command is an observation, not proof of nonpayment.
    session.start(
        task.role("wallet"),
        id,
        &format!(
            "cd /app && cashu -h {} -u sat -w wallet -t -y pay {}",
            native::quote(&task.mint_url()),
            native::quote(request)
        ),
    )?;
    let mut operation = cell::wait_one(session.client, id, 120)?;
    if operation["terminal"] != true {
        operation = cell::wait_one(session.client, id, 60)?;
    }
    ensure!(
        operation["terminal"] == true && operation["native_result"]["cleanup_verified"] == true,
        "payment attempt did not terminate cleanly: {operation}"
    );
    save(
        &config.work.join(format!("reference-{id}-operation.json")),
        &operation,
    )?;
    session.json(task.role("wallet"), &format!("{id}-observe"), &format!(
        "exec env HOME=/wallet PROOFSTORM_WALLET={} PROOFSTORM_MINT={} PROOFSTORM_EXPECTED_MINT_URL={} PROOFSTORM_INVOICE={} PROOFSTORM_MELT_BEFORE_IDS={} /opt/proofstorm/driver quote observe-melt",
        native::quote(task.role("wallet")), native::quote(task.role("mint")), native::quote(&task.mint_url()), native::quote(request), native::quote(&before.to_string())
    ))
}
