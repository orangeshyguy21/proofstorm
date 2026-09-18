//! The ordinary managed connection completes the entire platform workflow in one session.
use crate::{GateContext, McpClient, cell, json as expect, native};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const NAME: &str = "surface-cell";
const RUN: &str = "surface-run";

fn control(client: &mut McpClient, tool: &str, component: &str, id: &str) -> Result<Value> {
    client.call(
        tool,
        json!({"name":NAME,"run_id":RUN,"component":component,"request_id":id}),
    )?;
    Ok(cell::artifact_content(&cell::wait_succeeded(client, id)?)?.clone())
}
fn probe(client: &mut McpClient, id: &str, reachable: bool) -> Result<()> {
    client.call("network_probe",json!({"name":NAME,"run_id":RUN,"request_id":id,"from_component":"chain","to_component":"peer","service":"p2p","timeout_seconds":2,"attempts":2}))?;
    let result = cell::wait_succeeded(client, id)?;
    ensure!(
        cell::artifact_content(&result)?["reachable"] == reachable,
        "fresh network probe has unexpected reachability: {result}"
    );
    Ok(())
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.managed_session("surface")?;
    let discovery = client.response("tools/list", json!({}))?;
    let names = expect::array(&discovery, "/result/tools")?
        .iter()
        .map(|tool| expect::string(tool, "/name"))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        names
            == proofstorm_core::mcp::TOOLS
                .iter()
                .map(|tool| tool.name)
                .collect(),
        "managed connection differs from the public registry"
    );
    ensure!(
        serde_json::to_vec(&discovery)?.len() <= 128 * 1024,
        "discovery exceeds the complete wire budget"
    );
    context.record("surface-discovery.json", &discovery)?;
    let catalog = client.call("catalog_list", json!({"implementations":["bitcoin-core"]}))?;
    let entry = expect::array(&catalog, "/items")?
        .iter()
        .find(|entry| entry["preferred"] == true)
        .ok_or_else(|| anyhow::anyhow!("preferred Bitcoin release missing"))?;
    let component = |id: &str| json!({"id":id,"kind":entry["kind"],"implementation":"bitcoin-core","version":entry["version"],"config_version":entry["config_version"],"control":entry["recommended_control"],"config":{}});
    client.call(
        "catalog_entry_read",
        json!({"id":entry["id"],"version":entry["version"]}),
    )?;
    client.call(
        "catalog_config_schema_read",
        json!({"id":entry["id"],"version":entry["version"],"pointer":"/properties/fallback_fee"}),
    )?;
    let create = json!({"name":NAME,"request_id":"surface-create","cell":{"api_version":"proofstorm/v1alpha1","name":NAME,"components":[component("chain"),component("peer")],"links":[{"id":"peering","kind":"bitcoin_peer","from":"chain","to":"peer"}]}});
    let preview = client.call("cell_plan", create.clone())?;
    let reviewed = cell::review(&mut client, &preview)?;
    ensure!(
        expect::array(&reviewed, "/lock/entries")?
            .iter()
            .all(|entry| entry["image"]
                .as_str()
                .is_some_and(|image| image.contains("@sha256:"))),
        "plan images are not pinned"
    );
    eprintln!("surface: reviewed preview; submitting cell");
    let accepted = cell::apply(&mut client, &preview)?;
    let key = expect::string(&accepted, "/instance_key")?.to_owned();
    let result = (|| -> Result<()> {
        let ready = cell::wait_ready(&mut client, NAME)?;
        let namespace = expect::string(&ready, "/instance_namespace")?;
        client.call(
            "run_start",
            json!({"name":NAME,"run_id":RUN,"request_id":"surface-run-start"}),
        )?;
        native::stdout(
            &mut client,
            NAME,
            RUN,
            "chain",
            "surface-mine",
            &format!(
                "set -eu; {} createwallet default >/dev/null; address=$({} getnewaddress); {} generatetoaddress 1 \"$address\" >/dev/null",
                native::BITCOIN_ROOT,
                native::BITCOIN,
                native::BITCOIN_ROOT
            ),
        )?;
        let height = native::stdout(
            &mut client,
            NAME,
            RUN,
            "chain",
            "surface-height",
            &format!("{} getblockcount", native::BITCOIN_ROOT),
        )?;
        ensure!(height == "1", "native block height differs: {height}");
        eprintln!("surface: native Bitcoin verified; exercising lifecycle and edits");
        let pvc = context
            .kubectl
            .get_json(&["get", "pvc/data-peer-0", "-n", namespace])?;
        ensure!(
            control(&mut client, "component_stop", "peer", "surface-stop")?["state"] == "stopped",
            "stop not observed"
        );
        let mut edited = component("chain");
        edited["config"]["fallback_fee"] = json!(0.0003);
        let patch = json!({"name":NAME,"request_id":"surface-edit","expected_generation":1,"expected_instance_key":key,"patch":[{"op":"update_component","component":edited}]});
        let patch_plan = client.call("cell_plan", patch.clone())?;
        cell::apply(&mut client, &patch_plan)?;
        let retry = client.call("cell_up", patch)?;
        ensure!(
            retry["accepted_generation"] == 2,
            "retry changed accepted generation"
        );
        let original = client.call("cell_up", create)?;
        ensure!(
            original["desired_generation"] == 2,
            "old creation rolled back configuration"
        );
        let stopped = context
            .kubectl
            .get_json(&["get", "statefulset/peer", "-n", namespace])?;
        ensure!(
            stopped["spec"]["replicas"] == 0,
            "cell edit restarted an intentionally stopped component"
        );
        control(&mut client, "component_start", "peer", "surface-start")?;
        cell::wait_ready(&mut client, NAME)?;
        let before = context
            .kubectl
            .get_json(&["get", "pod/peer-0", "-n", namespace])?;
        let restart = control(&mut client, "component_restart", "peer", "surface-restart")?;
        ensure!(
            restart["restarted"] == true,
            "restart receipt missing observed effect"
        );
        let after = context
            .kubectl
            .get_json(&["get", "pod/peer-0", "-n", namespace])?;
        ensure!(
            before["metadata"]["uid"] != after["metadata"]["uid"],
            "restart did not replace the serving pod"
        );
        ensure!(
            pvc["metadata"]["uid"]
                == context
                    .kubectl
                    .get_json(&["get", "pvc/data-peer-0", "-n", namespace])?["metadata"]["uid"],
            "lifecycle operation replaced retained storage"
        );
        cell::wait_ready(&mut client, NAME)?;
        eprintln!("surface: lifecycle verified; exercising network faults");
        probe(&mut client, "surface-before-fault", true)?;
        client.call("network_partition",json!({"name":NAME,"run_id":RUN,"request_id":"surface-partition","from_component":"chain","to_component":"peer"}))?;
        cell::wait_succeeded(&mut client, "surface-partition")?;
        probe(&mut client, "surface-during-fault", false)?;
        client.call("network_heal",json!({"name":NAME,"run_id":RUN,"request_id":"surface-heal","partition_operation_id":"surface-partition"}))?;
        cell::wait_succeeded(&mut client, "surface-heal")?;
        probe(&mut client, "surface-after-fault", true)?;
        client.call("cell_exec",json!({"name":NAME,"run_id":RUN,"component":"chain","request_id":"surface-exit-seven","argv":["sh","-c","exit 7"]}))?;
        let failed = cell::wait_succeeded(&mut client, "surface-exit-seven")?;
        ensure!(
            failed["native_result"]["exit_code"] == 7
                && failed["native_result"]["cleanup_verified"] == true,
            "native exit and cleanup facts lost: {failed}"
        );
        client.call("cell_sync", json!({"name":NAME}))?;
        let searched=client.call("activity_search",json!({"name":NAME,"run_id":RUN,"native_exit_code":7,"fields":["/artifact/content/exit_code"]}))?;
        let hit = &expect::array(&searched, "/items")?[0];
        let read=client.call("operation_read",json!({"operation_id":hit["operation_id"],"expected_digest":hit["operation_digest"],"pointer":"/artifact/content/exit_code"}))?;
        ensure!(read["value"] == 7, "selected receipt read differs");
        client.call(
            "cell_search",
            json!({"name":NAME,"id":"chain","fields":["/config/fallback_fee"]}),
        )?;
        client.call("cell_read", json!({"name":NAME,"pointer":"/policy"}))?;
        eprintln!("surface: network and selected evidence verified; sealing run");
        client.call(
            "session_list",
            json!({"instance_id":ready["instance_id"],"run_id":RUN,"scan":true,"limit":1}),
        )?;
        client.call("environment_read", json!({"runs":{"id":RUN,"scan":true}}))?;
        client.call(
            "run_finish",
            json!({"run_id":RUN,"request_id":"surface-finish"}),
        )?;
        let export = client.call("evidence_export", json!({"run_id":RUN}))?;
        let journal = client.call(
            "evidence_section_read",
            json!({"run_id":RUN,"section":"journal","limit":2}),
        )?;
        ensure!(
            export["digest"] == journal["evidence_digest"],
            "sealed manifest and journal differ"
        );
        client.call("cell_exec",json!({"name":NAME,"component":"chain","request_id":"surface-continued","script":format!("{} getblockcount",native::BITCOIN_ROOT),"output":{"mode":"public"}}))?;
        let continued = cell::wait_succeeded(&mut client, "surface-continued")?;
        ensure!(
            continued["run_id"] != RUN,
            "new default work stayed in a sealed run"
        );
        ensure!(
            client.call("evidence_export", json!({"run_id":RUN}))?["digest"] == export["digest"],
            "continued work changed sealed evidence"
        );
        context.record("surface-evidence.json", &export)?;
        Ok(())
    })();
    let removed = client.call(
        "cell_remove",
        json!({"name":NAME,"expected_instance_key":key,"timeout_seconds":120}),
    );
    if let Ok(receipt) = &removed {
        context.record("surface-removal.json", receipt)?;
    }
    result?;
    removed?;
    cell::wait_closed(&mut client, NAME)?;
    context.record(
        "surface-result.json",
        &json!({"passed":true,"connection":"managed","tool_count":names.len()}),
    )?;
    Ok(())
}
