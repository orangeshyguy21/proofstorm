//! Live preservation gate. Uses its own database and cell; other cells stay running.
use crate::{GateContext, McpClient, cell};
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
    let mut authored = Vec::new();
    for component in components {
        let listing = client.call(
            "catalog_list",
            json!({"implementations":[component["implementation"]]}),
        )?;
        let items = listing["items"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("catalog items missing"))?;
        let entry = if let Some(version) = component["version"].as_str() {
            items.iter().find(|item| item["version"] == version)
        } else {
            items
                .iter()
                .find(|item| item["preferred"] == true)
                .or_else(|| items.first())
        }
        .ok_or_else(|| anyhow::anyhow!("catalog release missing: {component}"))?;
        authored.push(json!({"id":component["id"],"kind":entry["kind"],"implementation":component["implementation"],"version":entry["version"],"config_version":entry["config_version"],"control":component.get("control").unwrap_or(&entry["recommended_control"]),"config":component.get("config").cloned().unwrap_or(json!({}))}));
    }
    let mut links = Vec::new();
    for link in connections {
        links.push(match link["kind"].as_str() {
            Some("chain_backend")=>json!({"id":link["id"],"kind":"chain_backend","from":link["component"],"to":link["chain"],"binding":{"type":"chain","network":"regtest"}}),
            Some("payment_backend")=>json!({"id":link["id"],"kind":"payment_backend","from":link["mint"],"to":link["lightning"],"binding":{"type":"payment","method":"bolt11","unit":"sat"}}),
            _=>anyhow::bail!("unsupported test fixture connection: {link}"),
        });
    }
    let mut request = json!({"name":INSTANCE,"request_id":id,"cell":{"api_version":"proofstorm/v1alpha1","name":INSTANCE,"components":authored,"links":links,"policy":{"allow":[],"limits":{"max_components":64,"max_links":256,"max_config_bytes":65536}}}});
    if !target.is_null() {
        let inspect = client.call("cell_inspect", json!({"name":INSTANCE}))?;
        request["expected_generation"] = target["expected_generation"].clone();
        request["expected_instance_key"] = inspect["instance_key"].clone();
        for key in ["delete_data", "delete_retained"] {
            if let Some(value) = target.get(key) {
                request[key] = value.clone();
            }
        }
    }
    let preview = client.call("cell_plan", request)?;
    let mut plan = cell::read_document(
        client,
        &json!({"plan_id":preview["plan"]["id"]}),
        "plan",
        "",
    )?;
    plan["plan"] = preview["plan"].clone();
    Ok(plan)
}
fn apply(client: &mut McpClient, plan: &Value, key: &str) -> Result<Value> {
    client.call(
        "cell_up",
        json!({"name":INSTANCE,"request_id":key,"plan":plan["plan"]}),
    )
}
fn ready(client: &mut McpClient, generation: u64) -> Result<Value> {
    println!("Waiting for configuration {generation}");
    let value=client.call("cell_wait",json!({"name":INSTANCE,"target_phase":"ready","expected_generation":generation,"timeout_seconds":120}))?;
    ensure!(value["reached"] == true, "cell did not converge: {value}");
    println!("Configuration {generation} ready");
    Ok(value)
}
fn operation(client: &mut McpClient, tool: &str, id: &str, mut fields: Value) -> Result<Value> {
    let scope = json!({"name":INSTANCE,"run_id":RUN,"request_id":id});
    fields
        .as_object_mut()
        .unwrap()
        .extend(scope.as_object().unwrap().clone());
    client.call(tool, fields)?;
    let result = cell::wait_operation(client, id, 40)?;
    Ok(cell::artifact_content(&result)?.clone())
}
fn driver_operation(
    context: &GateContext,
    client: &mut McpClient,
    submit: fn(&GateContext, &mut McpClient, Value) -> Result<Value>,
    id: &str,
    mut fields: Value,
) -> Result<Value> {
    fields.as_object_mut().unwrap().extend(
        json!({"name":INSTANCE,"run_id":RUN,"request_id":id})
            .as_object()
            .unwrap()
            .clone(),
    );
    submit(context, client, fields)?;
    Ok(cell::artifact_content(&cell::wait_operation(client, id, 120)?)?.clone())
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
    json!({"name":INSTANCE,"expected_generation":generation})
}
fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    directory: &std::path::Path,
) -> Result<()> {
    ready(client, 1)?;
    let status = crate::cell::status(client, INSTANCE)?;
    let namespace = status["instance_namespace"].as_str().unwrap();
    context.kubectl.apply_stdin(&serde_json::to_string(&json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"external-app-state","namespace":namespace,"labels":{"proofstorm.dev/instance":namespace.trim_start_matches("proofstorm-"),"app.kubernetes.io/managed-by":"proofstormd","proofstorm.dev/component":"extra-lnd"}},"data":{"sentinel":"keep-me"}}))?)?;
    client.call(
        "run_start",
        json!({"request_id":"5358","name":INSTANCE,"run_id":RUN}),
    )?;
    driver_operation(
        context,
        client,
        crate::driver::liquidity_bootstrap,
        "bootstrap",
        json!({"chain":"chain","mint_lightning":"mint-lnd","payer_lightning":"payer-lnd","funding_sat":50_000_000,"channel_sat":10_000_000,"push_sat":5_000_000}),
    )?;
    driver_operation(
        context,
        client,
        crate::driver::wallet_initialize,
        "wallet-init",
        json!({"wallet":"wallet","mint":"mint"}),
    )?;
    driver_operation(
        context,
        client,
        crate::driver::wallet_fund,
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
        &json!({"name":INSTANCE,"expected_generation":4,"delete_retained":["extra-lnd","extra-mint"]}),
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
        .join("dev/dynamic-cell-runs")
        .join(&context.run_id);
    fs::create_dir_all(&directory)?;
    let mut client = context.default_session(&format!("dynamic-{}", context.run_id), "agent")?;
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
    client.call("cell_remove", json!({"name":INSTANCE}))?;
    let closed = cell::wait_closed(&mut client, INSTANCE)?;
    ensure!(
        closed["teardown_receipt"]["verified_absent"] == true,
        "gate cell cleanup unverified"
    );
    result?;
    println!(
        "Dynamic cell preservation gate passed: {}",
        directory.display()
    );
    Ok(())
}
