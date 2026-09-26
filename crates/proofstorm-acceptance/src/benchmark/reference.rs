//! Live known-good grader control. Never reported as a model attempt.
use super::{Context, events, observer, opencode, read, save};
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
        opencode: "unused".into(),
    };
    let path = config.work.join("benchmark-context.json");
    save(&path, &json!(config))?;
    let mut command = Command::new(std::env::current_exe()?);
    crate::client::clear_runtime_environment(&mut command);
    command.arg("--benchmark-proxy").arg(path);
    let mut client = McpClient::from_command(command, "benchmark-reference-control")?;
    let components = [
        ("chain","bitcoin","bitcoin-core","31.1","bitcoin-core/31/v1","cell"),
        ("mint-lnd","lightning","lnd","0.21.3-beta","lnd/0.20/v1","cell"),
        ("payer-lnd","lightning","lnd","0.21.3-beta","lnd/0.20/v1","cell"),
        ("mint","mint","cdk","0.18.1","cdk-mintd/0.18/v1","target"),
        ("wallet","wallet","nutshell-wallet","0.21.0","nutshell-wallet/0.20/v1","cell")
    ].map(|(id,kind,implementation,version,config_version,control)|json!({"id":id,"kind":kind,"implementation":implementation,"version":version,"config_version":config_version,"control":control,"config":if id=="mint" {json!({"input_fee_ppk":0})} else {json!({})}}));
    let document = json!({"api_version":"proofstorm/v1alpha1","name":"benchmark-o1","components":components,"links":[
        {"id":"mint-chain","kind":"chain_backend","from":"mint-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
        {"id":"payer-chain","kind":"chain_backend","from":"payer-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
        {"id":"mint-backend","kind":"payment_backend","from":"mint","to":"mint-lnd","binding":{"type":"payment","method":"bolt11","unit":"sat"}}
    ],"policy":{"allow":[],"limits":{"max_components":8,"max_links":8,"max_config_bytes":16384}}});
    client.call(
        "cell_up",
        json!({"name":"benchmark-o1","request_id":"reference-create","cell":document}),
    )?;
    cell::wait_ready_recorded(context, &mut client, "benchmark-o1")?;
    native::bootstrap(
        &mut client,
        "benchmark-o1",
        "",
        "bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        2_000_000,
        1_000_000,
        500_000,
    )?;
    let mut session = native::Session::new(&mut client, "benchmark-o1", "");
    session.nutshell_initialize("wallet", "mint", "initialize-wallet")?;
    let quote = session.nutshell_invoice("wallet", "mint", "mint-quote", 1000)?;
    let invoice =
        session.nutshell_invoice_projection("wallet", "mint", "mint-invoice", &quote, 1000)?;
    session.projected(
        "payer-lnd",
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
    session.nutshell_claim("wallet", "mint", "claim", &quote, 1000)?;
    session.client.call(
        "benchmark_checkpoint",
        json!({"stage":"funded","mint_quote_id":quote}),
    )?;
    let invoice = session.projected(
        "payer-lnd",
        "recipient-invoice",
        &format!("{} addinvoice --amt=100", native::LND),
        &json!({"mode":"lnd_invoice"}),
    )?;
    let melt = session.nutshell_melt(
        "wallet",
        "mint",
        "melt",
        invoice["payment_request"]
            .as_str()
            .context("recipient invoice")?,
        100,
    )?;
    let remaining = session.nutshell_balance("wallet", "mint", "remaining")?;
    session.client.call("benchmark_checkpoint",json!({"stage":"paid","mint_quote_id":quote,"melt_quote_id":melt["quote_id"],"payment_hash":invoice["payment_hash"],"minted_sat":1000,"paid_sat":100,"remaining_sat":remaining}))?;
    session
        .client
        .call("cell_remove", json!({"name":"benchmark-o1"}))?;
    cell::wait_closed(session.client, "benchmark-o1")?;
    let report = json!({"success":true,"minted_sat":1000,"paid_sat":100,"remaining_sat":remaining,"cleanup":true});
    opencode::observe_final(
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
        .push(json!({"status":"SUCCEEDED","value_sat":"100","payment_request":"extra"}));
    ensure!(
        observer::assertions(&funded, &paid)["accounting"] == false,
        "offsetting cycle control was accepted"
    );
    let prose = super::report::Report::parse(&format!("Payment completed.\n{report}"));
    ensure!(
        !prose.format_valid && prose.consistent(true, Some(remaining)),
        "report format and claims were conflated"
    );
    let wrong = super::report::Report::parse(
        &report
            .to_string()
            .replace("\"paid_sat\":100", "\"paid_sat\":101"),
    );
    ensure!(
        !wrong.consistent(true, Some(remaining)),
        "incorrect report accepted"
    );
    save(
        &config.work.join("oracle-reference.json"),
        &json!({"kind":"known-good-grader-control","model_attempt":false,"task":super::score::task(),"assertions":assertions,"boundary_calls":super::calls(&events(&config.work)?)?.len(),"controls":{"extra_payment_rejected":true,"prose_preserves_claims_not_format_credit":true,"incorrect_report_rejected":true}}),
    )?;
    Ok(())
}
