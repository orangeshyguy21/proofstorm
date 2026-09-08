//! Live preservation gate. Uses its own database and lab; other labs stay running.
use crate::{GateContext, McpClient, lab};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};
mod external_client;
mod failure;
const INSTANCE: &str = "dynamic-edit-test";
const RUN: &str = "dynamic-edit-run";
fn topology() -> (Vec<Value>, Vec<Value>) {
    (
        vec![
            json!({"id":"chain","implementation":"bitcoin-core"}),
            json!({"id":"mint-lnd","implementation":"lnd"}),
            json!({"id":"payer-lnd","implementation":"lnd"}),
            json!({"id":"mint","implementation":"cdk"}),
            json!({"id":"wallet","implementation":"nutshell-wallet"}),
        ],
        vec![
            json!({"id":"mint-chain","kind":"chain_backend","component":"mint-lnd","chain":"chain"}),
            json!({"id":"payer-chain","kind":"chain_backend","component":"payer-lnd","chain":"chain"}),
            json!({"id":"mint-backend","kind":"payment_backend","mint":"mint","lightning":"mint-lnd"}),
        ],
    )
}
fn plan(
    client: &mut McpClient,
    id: &str,
    components: &[Value],
    connections: &[Value],
    target: &Value,
) -> Result<Value> {
    client.call("lab_plan",json!({"plan_id":id,"components":components,"connections":connections,"runtime_requirements":[],"update":target,"idempotency_key":id}))
}
fn apply(client: &mut McpClient, plan: &Value, key: &str) -> Result<Value> {
    client.call("lab_apply",json!({"plan_id":plan["plan_id"],"expected_plan_digest":plan["plan_digest"],"instance_id":INSTANCE,"idempotency_key":key}))
}
fn ready(client: &mut McpClient, generation: u64) -> Result<Value> {
    println!("Waiting for configuration {generation}");
    let value=client.call("lab_wait",json!({"instance_id":INSTANCE,"target_phase":"ready","expected_generation":generation,"timeout_seconds":120}))?;
    ensure!(value["reached"] == true, "lab did not converge: {value}");
    println!("Configuration {generation} ready");
    Ok(value)
}
fn operation(client: &mut McpClient, tool: &str, id: &str, mut fields: Value) -> Result<Value> {
    let scope =
        json!({"instance_id":INSTANCE,"experiment_id":RUN,"operation_id":id,"idempotency_key":id});
    fields
        .as_object_mut()
        .unwrap()
        .extend(scope.as_object().unwrap().clone());
    client.call(tool, fields)?;
    let result = lab::wait_operation(client, id, 40)?;
    Ok(lab::artifact_content(&result)?.clone())
}
fn balance(client: &mut McpClient, id: &str) -> Result<i64> {
    let result = operation(
        client,
        "wallet_balance",
        id,
        json!({"wallet":"wallet","mint":"mint"}),
    )?;
    Ok(result["balance_sat"].as_i64().unwrap_or(-1))
}
fn snapshot(context: &GateContext, namespace: &str) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for kind in ["pods", "persistentvolumeclaims", "services", "secrets"] {
        let list = context.kubectl.get_json(&["get", kind, "-n", namespace])?;
        for item in list["items"].as_array().unwrap() {
            let Some(component) = item
                .pointer("/metadata/labels/proofstorm.dev~1component")
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !["chain", "mint-lnd", "payer-lnd", "mint", "wallet"].contains(&component) {
                continue;
            }
            let name = item["metadata"]["name"].as_str().unwrap();
            let identity = match kind {
                "secrets" => {
                    json!({"uid":item["metadata"]["uid"],"data_digest":proofstorm_core::digest_json(&item["data"])})
                }
                "services" => {
                    json!({"uid":item["metadata"]["uid"],"ip":item["spec"]["clusterIP"],"ports":item["spec"]["ports"]})
                }
                _ => item["metadata"]["uid"].clone(),
            };
            result.insert(format!("{kind}/{name}"), identity);
        }
    }
    Ok(result)
}
fn channels(context: &GateContext, namespace: &str) -> Result<Value> {
    let output = context.kubectl.exec(
        namespace,
        "statefulset/payer-lnd",
        &[
            "lncli",
            "--lnddir=/home/lnd/.lnd",
            "--network=regtest",
            "--rpcserver=127.0.0.1:10009",
            "listchannels",
        ],
    )?;
    let value: Value = serde_json::from_str(&output)?;
    Ok(json!(
        value["channels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (&c["chan_id"], &c["remote_pubkey"], &c["channel_point"]))
            .collect::<Vec<_>>()
    ))
}
fn target(generation: u64) -> Value {
    json!({"instance_id":INSTANCE,"expected_generation":generation})
}
fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    directory: &std::path::Path,
) -> Result<()> {
    ready(client, 1)?;
    let status = client.call("lab_status", json!({"instance_id":INSTANCE}))?;
    let namespace = status["instance_namespace"].as_str().unwrap();
    context.kubectl.apply_stdin(&serde_json::to_string(&json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"external-app-state","namespace":namespace,"labels":{"proofstorm.dev/instance":namespace.trim_start_matches("proofstorm-"),"app.kubernetes.io/managed-by":"proofstormd","proofstorm.dev/component":"extra-lnd"}},"data":{"sentinel":"keep-me"}}))?)?;
    client.call(
        "experiment_create",
        json!({"instance_id":INSTANCE,"experiment_id":RUN,"idempotency_key":"run"}),
    )?;
    operation(
        client,
        "liquidity_bootstrap",
        "bootstrap",
        json!({"chain":"chain","mint_lightning":"mint-lnd","payer_lightning":"payer-lnd","funding_sat":50_000_000,"channel_sat":10_000_000,"push_sat":5_000_000}),
    )?;
    operation(
        client,
        "wallet_initialize",
        "wallet-init",
        json!({"wallet":"wallet","mint":"mint"}),
    )?;
    operation(
        client,
        "wallet_fund",
        "wallet-fund",
        json!({"wallet":"wallet","mint":"mint","payer_lightning":"payer-lnd","amount_sat":1000}),
    )?;
    ensure!(
        balance(client, "balance-before")? == 1000,
        "wallet was not funded"
    );
    let mut external = external_client::ExternalClient::connect(context, namespace)?;
    external.check()?;
    let before = snapshot(context, namespace)?;
    let channel_before = channels(context, namespace)?;
    ensure!(
        channel_before.as_array().is_some_and(|a| !a.is_empty()),
        "no funded Lightning channel"
    );
    fs::write(
        directory.join("before.json"),
        serde_json::to_vec_pretty(&json!({"resources":before,"channels":channel_before}))?,
    )?;
    let (mut components, mut connections) = topology();
    components.extend([
        json!({"id":"extra-lnd","implementation":"lnd"}),
        json!({"id":"extra-mint","implementation":"cdk"}),
    ]);
    connections.extend([json!({"id":"extra-chain","kind":"chain_backend","component":"extra-lnd","chain":"chain"}),json!({"id":"extra-backend","kind":"payment_backend","mint":"extra-mint","lightning":"extra-lnd"})]);
    let expansion = plan(client, "expand", &components, &connections, &target(1))?;
    ensure!(
        expansion["update"]["changes"]["restarted"] == json!([]),
        "expansion restarts existing components: {expansion}"
    );
    fs::write(
        directory.join("expansion.json"),
        serde_json::to_vec_pretty(&expansion)?,
    )?;
    apply(client, &expansion, "expand-apply")?;
    ready(client, 2)?;
    external.check()?;
    ensure!(
        snapshot(context, namespace)? == before,
        "expansion changed original resource identities"
    );
    ensure!(
        balance(client, "balance-expanded")? == 1000,
        "wallet balance changed during expansion"
    );
    ensure!(
        channels(context, namespace)? == channel_before,
        "Lightning channels changed"
    );
    let original_extra =
        context
            .kubectl
            .get_json(&["get", "pod", "extra-lnd-0", "-n", namespace])?["metadata"]["uid"]
            .clone();
    components[5]["config"] = json!({"alias":"edited-alias"});
    let edit = plan(client, "configure", &components, &connections, &target(2))?;
    ensure!(
        edit["update"]["changes"]["restarted"] == json!(["extra-lnd"]),
        "restart scope was incorrect: {edit}"
    );
    apply(client, &edit, "configure-apply")?;
    ready(client, 3)?;
    external.check()?;
    ensure!(
        context
            .kubectl
            .get_json(&["get", "pod", "extra-lnd-0", "-n", namespace])?["metadata"]["uid"]
            != original_extra,
        "configuration did not roll the affected pod"
    );
    ensure!(
        snapshot(context, namespace)? == before,
        "targeted edit restarted original components"
    );
    let replay = apply(client, &expansion, "expand-apply")?;
    ensure!(
        replay["superseded"] == true && replay["current_generation"] == 3,
        "old replay did not report supersession: {replay}"
    );
    let (base_components, base_connections) = topology();
    let removal = plan(
        client,
        "remove",
        &base_components,
        &base_connections,
        &target(3),
    )?;
    apply(client, &removal, "remove-apply")?;
    let removed = ready(client, 4)?;
    ensure!(
        !removed["retained_storage"].as_object().unwrap().is_empty(),
        "removed volumes were not retained"
    );
    ensure!(
        snapshot(context, namespace)? == before,
        "removal changed original resources"
    );
    let purge = plan(
        client,
        "purge",
        &base_components,
        &base_connections,
        &json!({"instance_id":INSTANCE,"expected_generation":4,"delete_retained":["extra-lnd","extra-mint"]}),
    )?;
    apply(client, &purge, "purge-apply")?;
    let purged = ready(client, 5)?;
    external.check()?;
    ensure!(
        purged["retained_storage"] == json!({}),
        "retained storage was not explicitly deleted"
    );
    ensure!(
        balance(client, "balance-final")? == 1000,
        "wallet balance changed across edits"
    );
    ensure!(
        channels(context, namespace)? == channel_before,
        "channels changed across edits"
    );
    ensure!(
        snapshot(context, namespace)? == before,
        "original identities changed across edits"
    );
    ensure!(
        context
            .kubectl
            .get_json(&["get", "configmap", "external-app-state", "-n", namespace])?["data"]["sentinel"]
            == "keep-me",
        "pruning removed another application's resource"
    );
    failure::check(context, client, directory)?;
    external.check()?;
    ensure!(
        snapshot(context, namespace)? == before,
        "failed addition changed existing identities"
    );
    fs::write(
        directory.join("after.json"),
        serde_json::to_vec_pretty(&purged)?,
    )?;
    Ok(())
}
pub fn run(context: &GateContext) -> Result<()> {
    let directory = context
        .root
        .join("dev/dynamic-lab-runs")
        .join(&context.run_id);
    fs::create_dir_all(&directory)?;
    let mut caps = crate::EXPERIMENT_CAPABILITIES.to_vec();
    caps.extend([
        "lab.edit",
        "component.exec_live",
        "component.control",
        "action.cancel",
    ]);
    let mut client = context.session(&format!("dynamic-{}", context.run_id), "agent", &caps)?;
    let (components, connections) = topology();
    let initial = plan(
        &mut client,
        "initial",
        &components,
        &connections,
        &Value::Null,
    )?;
    apply(&mut client, &initial, "initial-apply")?;
    let result = exercise(context, &mut client, &directory);
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}"))}),
        )?,
    )?;
    client.call("lab_close", json!({"instance_id":INSTANCE}))?;
    let closed = lab::wait_closed(&mut client, INSTANCE)?;
    ensure!(
        closed["teardown_receipt"]["verified_absent"] == true,
        "gate lab cleanup unverified"
    );
    result?;
    println!(
        "Dynamic lab preservation gate passed: {}",
        directory.display()
    );
    Ok(())
}
