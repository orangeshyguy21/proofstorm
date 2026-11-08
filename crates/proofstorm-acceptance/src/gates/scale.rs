//! Scale gate: one cell of `size` busybox workspaces through the ordinary
//! lifecycle (create, ready, patch, exec, partition, inspect, remove).
//!
//! It fails only on broken behaviour. Timings, controller usage, object sizes,
//! writes while the cell is idle and API errors go to `scale-<size>.json`.
use super::measure::{self, Report, succeeded};
use crate::{
    GateContext, cell, json as expect,
    kubectl::{CONTROL_NAMESPACE, Kubectl},
    native,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const NAME: &str = "scale-cell";
const RUN: &str = "scale-run";
const WORKSPACE: &str = "acceptance-scale";
const PRINCIPAL: &str = "scale-designer";
/// How long the ready cell sits untouched while its API writes are counted.
const IDLE: Duration = Duration::from_secs(120);

pub fn run(context: &GateContext, size: usize) -> Result<()> {
    measure::measured(
        context,
        &format!("scale-{size}"),
        vec![measure::control_pods()],
        |report| scenario(context, size, report),
    )
}

fn scenario(context: &GateContext, size: usize, report: &mut Report) -> Result<()> {
    let kubectl = &context.kubectl;
    let mut client = context.default_session(WORKSPACE, PRINCIPAL)?;
    let catalog = client.call("catalog_list", json!({"implementations":["workspace"]}))?;
    let entry = expect::array(&catalog, "/items")?
        .iter()
        .find(|entry| entry["preferred"] == true)
        .ok_or_else(|| anyhow::anyhow!("preferred workspace release missing"))?
        .clone();
    let component = |id: &str| json!({"id":id,"kind":entry["kind"],"implementation":"workspace","version":entry["version"],"config_version":entry["config_version"],"control":entry["recommended_control"],"config":{}});
    let ids: Vec<String> = (0..size).map(|index| format!("w{index:04}")).collect();
    report.facts.insert("components".into(), json!(size));
    let spec = json!({"api_version":"proofstorm/v1alpha1","name":NAME,
        "components":ids.iter().map(|id| component(id)).collect::<Vec<_>>(),"links":[]});

    report.phase("create");
    let preview = report.call(
        &mut client,
        "plan",
        "cell_plan",
        json!({"name":NAME,"request_id":"scale-create","cell":spec}),
    )?;
    let started = Instant::now();
    report.facts.insert(
        "apply_started_unix".into(),
        json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()),
    );
    cell::apply(&mut client, &preview)?;
    report.step("apply", started);
    measure::wait_ready(&mut client, NAME, None, ready_minutes(size))?;
    report.step("ready", started);
    let status = cell::status(&mut client, NAME)?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_owned();
    context.record(
        &format!("scale-{size}-startup.json"),
        &startup(kubectl, &namespace)?,
    )?;
    let pods = pod_uids(kubectl, &namespace)?;
    ensure!(
        pods.len() == size,
        "expected {size} component pods, found {}",
        pods.len()
    );

    report.phase("inspect");
    report.call(&mut client, "inspect", "cell_inspect", json!({"name":NAME}))?;
    report.call(
        &mut client,
        "inspect-runtime",
        "cell_inspect",
        json!({"name":NAME,"fields":["/runtime"]}),
    )?;
    report.call(
        &mut client,
        "component-status-page",
        "cell_component_status_list",
        json!({"name":NAME,"limit":50}),
    )?;
    let waiting = report.call(
        &mut client,
        "component-status-not-ready",
        "cell_component_status_list",
        json!({"name":NAME,"ready":false,"limit":50}),
    )?;
    ensure!(
        expect::array(&waiting, "/components")?.is_empty(),
        "ready cell lists components that are not ready: {waiting}"
    );
    let started = Instant::now();
    let cli = context.inspect_cli(WORKSPACE, PRINCIPAL, NAME)?;
    report.step("cli-status", started);
    ensure!(
        cli["runtime"]["phase"] == "ready",
        "CLI does not read the ready cell"
    );
    report
        .facts
        .insert("objects".into(), objects(kubectl, &namespace)?);

    report.phase("idle");
    let (_, writes) = measure::idle_writes(kubectl, IDLE)?;
    report.facts.insert("idle_writes".into(), writes);

    report.phase("edit");
    let inspected = client.call("cell_inspect", json!({"name":NAME}))?;
    let generation = expect::integer(&inspected, "/desired_generation")?;
    let mut edited = component(&ids[0]);
    edited["resources"] = json!({"memory":{"request":"192Mi"}});
    let plan = report.call(
        &mut client,
        "edit-plan",
        "cell_plan",
        json!({"name":NAME,"request_id":"scale-edit",
        "expected_generation":generation,"expected_instance_key":inspected["instance_key"],
        "patch":[{"op":"update_component","component":edited}]}),
    )?;
    let started = Instant::now();
    cell::apply(&mut client, &plan)?;
    report.step("edit-apply", started);
    measure::wait_ready(&mut client, NAME, Some(generation + 1), ready_minutes(size))?;
    report.step("edit-ready", started);
    let edited_pods = pod_uids(kubectl, &namespace)?;
    let replaced: Vec<&String> = pods
        .iter()
        .filter(|(name, uid)| edited_pods.get(*name) != Some(uid))
        .map(|(name, _)| name)
        .collect();
    ensure!(
        replaced == [&ids[0]],
        "editing one component replaced pods {replaced:?}"
    );

    report.phase("exec");
    client.call(
        "run_start",
        json!({"name":NAME,"run_id":RUN,"request_id":"scale-run-start"}),
    )?;
    let sample: Vec<&String> = [0, size / 4, size / 2, size * 3 / 4, size - 1]
        .iter()
        .map(|index| &ids[*index])
        .collect();
    for (index, id) in sample.iter().enumerate() {
        let request = format!("scale-exec-{index}");
        let started = Instant::now();
        client.call(
            "cell_exec",
            json!({"name":NAME,"run_id":RUN,"request_id":request,"component":id,
            "script":"echo scale-ok","output":{"mode":"public"},"timeout_seconds":30}),
        )?;
        let operation = succeeded(&mut client, &request)?;
        report.step(&format!("exec-{id}"), started);
        let content = cell::artifact_content(&operation)?;
        native::validate_output(content)?;
        ensure!(
            expect::string(content, "/stdout")?.trim() == "scale-ok",
            "exec into {id} returned {content}"
        );
    }

    report.phase("faults");
    let open = policies(kubectl, &namespace)?;
    let started = Instant::now();
    client.call(
        "network_partition",
        json!({"name":NAME,"run_id":RUN,"request_id":"scale-partition",
        "from_component":ids[1],"to_component":ids[2]}),
    )?;
    succeeded(&mut client, "scale-partition")?;
    report.step("partition", started);
    let partitioned = policies(kubectl, &namespace)?;
    ensure!(
        partitioned.len() > open.len(),
        "partition added no network policy: {open:?} -> {partitioned:?}"
    );
    let started = Instant::now();
    client.call(
        "network_heal",
        json!({"name":NAME,"run_id":RUN,"request_id":"scale-heal",
        "partition_operation_id":"scale-partition"}),
    )?;
    succeeded(&mut client, "scale-heal")?;
    report.step("heal", started);
    ensure!(
        policies(kubectl, &namespace)? == open,
        "heal did not restore the open policies"
    );
    client.call(
        "run_finish",
        json!({"run_id":RUN,"request_id":"scale-run-finish"}),
    )?;

    report.phase("remove");
    let started = Instant::now();
    client.call("cell_remove", json!({"name":NAME}))?;
    cell::wait_closed(&mut client, NAME)?;
    report.step("remove", started);
    kubectl.assert_no_instance_namespaces()?;
    report.phase("done");
    Ok(())
}

/// Ten minutes plus three seconds per component.
fn ready_minutes(size: usize) -> usize {
    10 + size / 20
}

/// Component ID to its pod's UID.
fn pod_uids(kubectl: &Kubectl, namespace: &str) -> Result<BTreeMap<String, String>> {
    let pods = kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        "proofstorm.dev/component",
    ])?;
    Ok(expect::array(&pods, "/items")?
        .iter()
        .filter_map(|pod| {
            Some((
                pod["metadata"]["labels"]["proofstorm.dev/component"]
                    .as_str()?
                    .to_owned(),
                pod["metadata"]["uid"].as_str()?.to_owned(),
            ))
        })
        .collect())
}

/// Raw startup timestamps per component pod and claim, plus the cell's
/// events by reason, to show which stage paces readiness.
fn startup(kubectl: &Kubectl, namespace: &str) -> Result<Value> {
    let pods = kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        "proofstorm.dev/component",
    ])?;
    let pods: Vec<Value> = expect::array(&pods, "/items")?
        .iter()
        .map(|pod| {
            let mut times = json!({"created":pod["metadata"]["creationTimestamp"],"started":pod["status"]["startTime"]});
            for condition in pod["status"]["conditions"].as_array().into_iter().flatten() {
                if let Some(kind) = condition["type"].as_str() {
                    times[kind] = condition["lastTransitionTime"].clone();
                }
            }
            json!({"component":pod["metadata"]["labels"]["proofstorm.dev/component"],"node":pod["spec"]["nodeName"],"times":times})
        })
        .collect();
    let claims = kubectl.get_json(&["get", "persistentvolumeclaims", "-n", namespace])?;
    let claims: Vec<Value> = expect::array(&claims, "/items")?
        .iter()
        .map(|claim| json!({"name":claim["metadata"]["name"],"created":claim["metadata"]["creationTimestamp"]}))
        .collect();
    let events = kubectl.get_json(&["get", "events", "-n", namespace])?;
    let mut reasons: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for event in expect::array(&events, "/items")? {
        let reason = event["reason"].as_str().unwrap_or_default().to_owned();
        let at = [
            &event["eventTime"],
            &event["firstTimestamp"],
            &event["metadata"]["creationTimestamp"],
        ]
        .into_iter()
        .find(|time| !time.is_null())
        .cloned()
        .unwrap_or_default();
        reasons
            .entry(reason)
            .or_default()
            .push(json!({"object":event["involvedObject"]["name"],"at":at,"count":event["count"]}));
    }
    // When the cell object, its namespace and each field manager's writes
    // happened, to tell controller delays from scheduling and storage.
    let writes = |object: &Value| {
        json!({"created":object["metadata"]["creationTimestamp"],"writes":object["metadata"]["managedFields"]
            .as_array().into_iter().flatten().map(|field| json!({"manager":field["manager"],
            "operation":field["operation"],"subresource":field["subresource"],"time":field["time"]})).collect::<Vec<_>>()})
    };
    let cells = kubectl.get_json(&[
        "get",
        "proofstormcells",
        "-n",
        CONTROL_NAMESPACE,
        "--show-managed-fields",
    ])?;
    let cells: Vec<Value> = expect::array(&cells, "/items")?
        .iter()
        .map(writes)
        .collect();
    let namespace =
        writes(&kubectl.get_json(&["get", "namespace", namespace, "--show-managed-fields"])?);
    Ok(json!({"cells":cells,"namespace":namespace,"pods":pods,"claims":claims,"events":reasons}))
}

fn policies(kubectl: &Kubectl, namespace: &str) -> Result<Vec<String>> {
    let list = kubectl.get_json(&["get", "networkpolicies", "-n", namespace])?;
    let mut names: Vec<String> = expect::array(&list, "/items")?
        .iter()
        .filter_map(|item| item["metadata"]["name"].as_str().map(str::to_owned))
        .collect();
    names.sort();
    Ok(names)
}

/// Object counts per kind and the largest objects, in the control and cell
/// namespaces. Secrets are left out so their data is never read.
fn objects(kubectl: &Kubectl, namespace: &str) -> Result<Value> {
    let mut counts = BTreeMap::new();
    let mut sizes = Vec::new();
    for kind in [
        "proofstormcells",
        "proofstormcomponents",
        "proofstormcellactions",
        "configmaps",
        "networkpolicies",
        "statefulsets",
        "services",
        "pods",
        "persistentvolumeclaims",
    ] {
        let list = kubectl.get_json(&["get", kind, "-A"])?;
        for item in expect::array(&list, "/items")? {
            let owner = item["metadata"]["namespace"].as_str().unwrap_or_default();
            if ![CONTROL_NAMESPACE, namespace].contains(&owner) {
                continue;
            }
            *counts.entry(kind).or_insert(0) += 1;
            sizes.push((
                serde_json::to_string(item)?.len(),
                format!(
                    "{kind}/{owner}/{}",
                    item["metadata"]["name"].as_str().unwrap_or_default()
                ),
            ));
        }
    }
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.truncate(8);
    Ok(
        json!({"counts":counts,"largest":sizes.into_iter().map(|(bytes, object)| json!({"object":object,"bytes":bytes})).collect::<Vec<_>>()}),
    )
}
