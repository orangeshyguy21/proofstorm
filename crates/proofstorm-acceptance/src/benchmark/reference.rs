//! Live known-good grader control. Never reported as a model attempt.
use super::{Context, events, observe_final, observer, read, save};
use crate::{GateContext, McpClient, cell, native};
use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use std::process::Command;

pub fn run(context: &GateContext) -> Result<()> {
    let config = Context {
        root: context.root.clone(),
        work: context.work().into(),
        home: context.installation.home.clone(),
        mcp: context.artifacts.mcp.clone(),
        model: "reference-control".into(),
        harness: super::harness::Harness::Reference,
        task: super::task::o1().clone(),
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
        task.component("bitcoin-core", 0),
        task.component("lnd", 0),
        task.component("lnd", 1),
        2_000_000,
        1_000_000,
        500_000,
    )?;
    let mut session = native::Session::new(&mut client, &task.cell_name, "");
    session.nutshell_initialize(
        task.component("nutshell-wallet", 0),
        task.component("cdk", 0),
        "initialize-wallet",
    )?;
    let quote = session.nutshell_invoice(
        task.component("nutshell-wallet", 0),
        task.component("cdk", 0),
        "mint-quote",
        task.amounts.mint_sat,
    )?;
    let invoice = session.nutshell_invoice_projection(
        task.component("nutshell-wallet", 0),
        task.component("cdk", 0),
        "mint-invoice",
        &quote,
        task.amounts.mint_sat,
    )?;
    session.projected(
        task.component("lnd", 1),
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
        task.component("nutshell-wallet", 0),
        task.component("cdk", 0),
        "claim",
        &quote,
        task.amounts.mint_sat,
    )?;
    session.client.call(
        "benchmark_checkpoint",
        json!({"stage":"funded","mint_quote_id":quote}),
    )?;
    let invoice = session.projected(
        task.component("lnd", 1),
        "recipient-invoice",
        &format!("{} addinvoice --amt={}", native::LND, task.amounts.melt_sat),
        &json!({"mode":"lnd_invoice"}),
    )?;
    let melt = session.nutshell_melt(
        task.component("nutshell-wallet", 0),
        task.component("cdk", 0),
        "melt",
        invoice["payment_request"]
            .as_str()
            .context("recipient invoice")?,
        task.amounts.melt_sat,
    )?;
    let remaining = session.nutshell_balance(
        task.component("nutshell-wallet", 0),
        task.component("cdk", 0),
        "remaining",
    )?;
    session.client.call("benchmark_checkpoint",json!({"stage":"paid","mint_quote_id":quote,"melt_quote_id":melt["quote_id"],"payment_hash":invoice["payment_hash"],"minted_sat":task.amounts.mint_sat,"paid_sat":task.amounts.melt_sat,"remaining_sat":remaining}))?;
    session
        .client
        .call("cell_remove", json!({"name":&task.cell_name}))?;
    cell::wait_closed(session.client, &task.cell_name)?;
    let report = json!({"success":true,"minted_sat":task.amounts.mint_sat,"paid_sat":task.amounts.melt_sat,"remaining_sat":remaining,"cleanup":true});
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
    let mut paid = read(&config.work.join("paid.json"))?;
    paid["payer_payments"]["payments"]
        .as_array_mut()
        .context("payment list")?
        .push(json!({"status":"SUCCEEDED","value_sat":task.amounts.melt_sat.to_string(),"payment_request":"extra"}));
    ensure!(
        observer::assertions(task, &funded, &paid)["accounting"] == false,
        "offsetting cycle control was accepted"
    );
    let prose = super::report::Report::parse(&format!("Payment completed.\n{report}"));
    ensure!(
        !prose.format_valid && prose.consistent(task, true, Some(remaining)),
        "report format and claims were conflated"
    );
    let mut wrong = report.clone();
    wrong["paid_sat"] = json!(task.amounts.melt_sat + 1);
    let wrong = super::report::Report::parse(&wrong.to_string());
    ensure!(
        !wrong.consistent(task, true, Some(remaining)),
        "incorrect report accepted"
    );
    save(
        &config.work.join("oracle-reference.json"),
        &json!({"kind":"known-good-grader-control","model_attempt":false,"task":super::score::task(),"assertions":assertions,"boundary_calls":super::calls(&events(&config.work)?)?.len(),"controls":{"extra_payment_rejected":true,"prose_preserves_claims_not_format_credit":true,"incorrect_report_rejected":true}}),
    )?;
    Ok(())
}
