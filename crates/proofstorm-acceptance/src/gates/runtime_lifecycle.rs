//! Real stop/start with persistent MCP, funded Bitcoin state, and an owned failure/retry.
use crate::{GateContext, cell, native, process};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    net::{Ipv4Addr, TcpListener},
    process::Command,
};

fn storage(context: &GateContext) -> Result<Value> {
    let claims = context.kubectl.get_json(&["get", "pvc", "-A"])?;
    let mut result = std::collections::BTreeMap::new();
    for claim in claims["items"].as_array().context("PVC items missing")? {
        result.insert(
            format!(
                "{}/{}",
                claim["metadata"]["namespace"], claim["metadata"]["name"]
            ),
            json!([claim["metadata"]["uid"], claim["spec"]["volumeName"]]),
        );
    }
    Ok(json!(result))
}

fn stopped(context: &GateContext) -> Result<()> {
    let receipt: Value = serde_json::from_slice(&fs::read(
        context.installation.home.join("runtime-resources.json"),
    )?)?;
    for id in receipt["containers"]
        .as_object()
        .context("container receipt missing")?
        .values()
    {
        let mut command = Command::new("docker");
        command.args([
            "inspect",
            "--format",
            "{{.State.Running}}",
            id.as_str().context("container ID")?,
        ]);
        let output = process::capture(command, 30)?;
        ensure!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "false",
            "runtime container still running"
        );
    }
    Ok(())
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.managed_session("runtime-lifecycle")?;
    let mut spec: Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json"))?;
    let mut sleeping = spec["components"][0].clone();
    sleeping["id"] = json!("sleeping");
    spec["components"] = json!([spec["components"][0], sleeping]);
    spec["links"] = json!([]);
    client.call(
        "cell_up",
        json!({"name":"suspend-test","request_id":"suspend-create","cell":spec}),
    )?;
    cell::wait_ready(&mut client, "suspend-test")?;
    native::stdout(
        &mut client,
        "suspend-test",
        "",
        "chain",
        "suspend-fund",
        &format!(
            "set -eu; {} -named createwallet wallet_name=default load_on_startup=true >/dev/null; address=$({} getnewaddress); {} generatetoaddress 101 \"$address\" >/dev/null",
            native::BITCOIN_ROOT,
            native::BITCOIN,
            native::BITCOIN
        ),
    )?;
    let before = native::json_output(
        &mut client,
        "suspend-test",
        "",
        "chain",
        "suspend-before",
        &format!("{} getwalletinfo", native::BITCOIN),
    )?;
    let hash = native::stdout(
        &mut client,
        "suspend-test",
        "",
        "chain",
        "suspend-hash",
        &format!("{} getbestblockhash", native::BITCOIN_ROOT),
    )?;
    let inspected = client.call("cell_inspect", json!({"name":"suspend-test"}))?;
    ensure!(
        inspected["instance_key"].is_string() && inspected["desired_generation"].is_number(),
        "cell identity evidence missing"
    );
    client.call(
        "component_stop",
        json!({"name":"suspend-test","component":"sleeping","request_id":"sleeping-stop"}),
    )?;
    cell::wait_succeeded(&mut client, "sleeping-stop")?;
    let volumes = storage(context)?;
    let identity = fs::read(context.installation.home.join("installation.json"))?;
    let resources = fs::read(context.installation.home.join("runtime-resources.json"))?;

    let mut foreign: Value = serde_json::from_slice(&resources)?;
    *foreign["containers"]
        .as_object_mut()
        .context("resource receipt")?
        .values_mut()
        .next()
        .context("container receipt")? = json!("replaced-container");
    fs::write(
        context.installation.home.join("runtime-resources.json"),
        serde_json::to_vec(&foreign)?,
    )?;
    let refused = process::capture(context.command(&["stop"])?, 60);
    fs::write(
        context.installation.home.join("runtime-resources.json"),
        &resources,
    )?;
    ensure!(
        !refused?.status.success()
            && !context
                .installation
                .home
                .join("runtime-lifecycle.json")
                .exists(),
        "changed runtime identity was accepted"
    );

    // A real cross-process lease forces an incomplete stop. Resume cancels the
    // pending shutdown, without changing workload or persistent resource identity.
    let lease = proofstorm_app::bootstrap::lifecycle::access(Some(&context.installation))?;
    let busy = process::capture(context.command(&["stop", "--timeout", "1"])?, 30)?;
    ensure!(!busy.status.success(), "stop ignored an active client call");
    drop(lease);
    ensure!(
        context.cli(&["start"])?["ready"] == true,
        "could not cancel incomplete stop"
    );

    for cycle in 0..3 {
        eprintln!("Runtime suspension cycle {}...", cycle + 1);
        context.cli(&["gui", "start", "--allow-development"])?;
        // This asynchronous command must settle before services stop.
        let operation = format!("before-stop-{cycle}");
        client.call("cell_exec", json!({"name":"suspend-test","component":"chain","request_id":operation,
            "script":"sleep 2; printf 'settled\\n'","timeout_seconds":30,"output":{"mode":"public"}}))?;
        ensure!(
            context.cli(&["stop"])?["state"] == "stopped",
            "stop did not complete"
        );
        stopped(context)?;
        ensure!(
            context.cli(&["stop"])?["state"] == "stopped",
            "repeat stop failed"
        );
        ensure!(
            context.cli(&["doctor"])?["ok"] == true,
            "stopped installation is incorrectly diagnosed as broken"
        );
        ensure!(
            context.cli(&["gui", "status"])?["state"] == "stopped",
            "GUI still running"
        );
        let denied = client.call_error("cell_exec", json!({"name":"suspend-test","component":"chain","request_id":format!("denied-{cycle}"),"argv":["true"]}))?;
        ensure!(
            denied["data"]["code"] == "runtime_suspended",
            "persistent MCP did not report suspension: {denied}"
        );
        client.call("catalog_list", json!({}))?;
        if cycle == 0 {
            // A port conflict interrupts actual startup after the registry starts.
            let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, context.installation.api_port))?;
            let failed = process::capture(context.command(&["start", "--timeout", "5"])?, 60)?;
            ensure!(
                !failed.status.success(),
                "start ignored an occupied runtime port"
            );
            drop(occupied);
        }
        ensure!(
            context.cli(&["start"])?["ready"] == true,
            "start did not restore services"
        );
        ensure!(
            context.cli(&["start"])?["already_running"] == true,
            "repeat start is not idempotent"
        );
        cell::wait_succeeded(&mut client, &operation)?;
        ensure!(
            volumes == storage(context)?,
            "PVC or volume binding changed"
        );
        let after = native::json_output(
            &mut client,
            "suspend-test",
            "",
            "chain",
            &format!("balance-{cycle}"),
            &format!("{} getwalletinfo", native::BITCOIN),
        )?;
        ensure!(
            before["balance"] == after["balance"]
                && before["immature_balance"] == after["immature_balance"]
                && before["txcount"] == after["txcount"],
            "wallet state changed across suspension"
        );
        ensure!(
            hash == native::stdout(
                &mut client,
                "suspend-test",
                "",
                "chain",
                &format!("hash-{cycle}"),
                &format!("{} getbestblockhash", native::BITCOIN_ROOT)
            )?,
            "chain history changed"
        );
        let after = client.call("cell_inspect", json!({"name":"suspend-test"}))?;
        ensure!(
            inspected["instance_key"] == after["instance_key"]
                && inspected["desired_generation"] == after["desired_generation"],
            "cell incarnation changed"
        );
        let sets = context.kubectl.get_json(&[
            "get",
            "statefulsets",
            "-A",
            "-l",
            "proofstorm.dev/component=sleeping",
        ])?;
        let items = sets["items"].as_array().context("statefulsets missing")?;
        ensure!(
            items.len() == 1 && items[0]["spec"]["replicas"] == 0,
            "individually stopped component was restarted"
        );
        ensure!(
            identity == fs::read(context.installation.home.join("installation.json"))?
                && resources == fs::read(context.installation.home.join("runtime-resources.json"))?,
            "installation resources were replaced"
        );
    }
    context.cli(&["rm", "suspend-test"])?;
    context.record("runtime-lifecycle.json", &json!({"passed":true,"cycles":3,"persistent_mcp":true,
        "wallet_and_chain_preserved":true,"stopped_component_preserved":true,"same_volumes_and_identities":true,
        "active_call_fence":true,"partial_start_retry":true,"stopped_containers_verified":true}))
}
