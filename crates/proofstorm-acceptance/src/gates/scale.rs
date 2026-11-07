//! Scale gate: one cell of `size` busybox workspaces through the ordinary
//! lifecycle (create, ready, patch, exec, partition, inspect, remove).
//!
//! It fails only on broken behaviour. Timings, controller usage, object sizes,
//! writes while the cell is idle and API errors go to `scale-<size>.json`.
use crate::{
    GateContext, McpClient, cell, json as expect,
    kubectl::{CONTROL_NAMESPACE, Kubectl},
    native,
};
use anyhow::{Result, bail, ensure};
use proofstorm_app::telemetry::quantity;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const NAME: &str = "scale-cell";
const RUN: &str = "scale-run";
const WORKSPACE: &str = "acceptance-scale";
const PRINCIPAL: &str = "scale-designer";
/// How long the ready cell sits untouched while its API writes are counted.
const IDLE: Duration = Duration::from_secs(120);

pub fn run(context: &GateContext, size: usize) -> Result<()> {
    let sampler = Sampler::start(context)?;
    let mut report = Report {
        started: Instant::now(),
        phase: sampler.phase.clone(),
        steps: Vec::new(),
        facts: serde_json::Map::new(),
    };
    report.facts.insert("components".into(), json!(size));
    let before = api_requests(&context.kubectl)?;
    let result = scenario(context, size, &mut report);
    let samples = sampler.stop();
    let after = api_requests(&context.kubectl);
    let mut document = json!({
        "components": size,
        "passed": result.is_ok(),
        "seconds": report.started.elapsed().as_secs_f64(),
        "steps": report.steps,
        "facts": report.facts,
        "controller_usage": usage_summary(&samples),
    });
    if let Ok(after) = &after {
        let changed = delta(&before, after);
        document["api_errors"] = json!(
            changed
                .iter()
                .filter(|(key, _)| key
                    .rsplit(' ')
                    .next()
                    .is_some_and(|code| code.starts_with(['4', '5'])))
                .collect::<BTreeMap<_, _>>()
        );
    }
    let log = controller_log(&context.kubectl, report.started.elapsed());
    document["controller_log"] = log_summary(&log);
    context.record(&format!("scale-{size}.json"), &document)?;
    context.record(&format!("scale-{size}-controller-log.json"), &json!(log))?;
    context.record(&format!("scale-{size}-usage.json"), &json!(samples))?;
    eprintln!(
        "scale: {size} components, steps {}",
        serde_json::to_string(&document["steps"])?
    );
    result?;
    after?;
    Ok(())
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
    wait_ready(&mut client, None, size)?;
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
    let before = api_requests(kubectl)?;
    thread::sleep(IDLE);
    let writes: BTreeMap<String, u64> = delta(&before, &api_requests(kubectl)?)
        .into_iter()
        .filter(|(key, _)| {
            ["POST ", "PUT ", "PATCH ", "APPLY ", "DELETE "]
                .iter()
                .any(|verb| key.starts_with(verb))
        })
        .collect();
    let proofstorm: u64 = writes
        .iter()
        .filter(|(key, _)| key.contains("proofstorm.dev"))
        .map(|(_, count)| count)
        .sum();
    report.facts.insert(
        "idle_writes".into(),
        json!({"seconds":IDLE.as_secs(),"proofstorm_writes":proofstorm,"by_request":writes}),
    );

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
    wait_ready(&mut client, Some(generation + 1), size)?;
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

struct Report {
    started: Instant,
    phase: Arc<Mutex<String>>,
    steps: Vec<Value>,
    facts: serde_json::Map<String, Value>,
}

impl Report {
    fn phase(&self, name: &str) {
        eprintln!("scale: {name}");
        if let Ok(mut phase) = self.phase.lock() {
            name.clone_into(&mut phase);
        }
    }

    fn step(&mut self, step: &str, started: Instant) {
        self.steps
            .push(json!({"step":step,"seconds":started.elapsed().as_secs_f64()}));
    }

    /// One timed MCP call, with its result size.
    fn call(
        &mut self,
        client: &mut McpClient,
        step: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value> {
        let started = Instant::now();
        let value = client.call(tool, arguments)?;
        self.steps.push(
            json!({"step":step,"tool":tool,"seconds":started.elapsed().as_secs_f64(),
            "bytes":serde_json::to_string(&value)?.len()}),
        );
        Ok(value)
    }
}

/// Wait for readiness, allowing ten minutes plus three seconds per component.
fn wait_ready(client: &mut McpClient, generation: Option<u64>, size: usize) -> Result<()> {
    for _ in 0..10 + size / 20 {
        let mut request = json!({"name":NAME,"target_phase":"ready","timeout_seconds":60});
        if let Some(generation) = generation {
            request["expected_generation"] = json!(generation);
        }
        let waited = client.call("cell_wait", request)?;
        if waited["reached"] == true {
            return Ok(());
        }
        ensure!(
            waited["timed_out"] == true,
            "cell readiness blocked or superseded: {waited}"
        );
    }
    bail!("{size}-component cell did not become ready")
}

/// Long-poll one operation to success, so timings aren't rounded to a poll interval.
fn succeeded(client: &mut McpClient, id: &str) -> Result<Value> {
    let operation = cell::wait_one(client, id, 120)?;
    ensure!(
        operation["phase"] == "succeeded",
        "operation {id} did not succeed: {operation}"
    );
    Ok(operation)
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
    Ok(json!({"pods":pods,"claims":claims,"events":reasons}))
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

/// `apiserver_request_total` keyed by "VERB group/resource/subresource CODE".
fn api_requests(kubectl: &Kubectl) -> Result<BTreeMap<String, u64>> {
    let text = kubectl.run(&["get", "--raw", "/metrics"])?;
    let mut counts = BTreeMap::new();
    for line in text.lines() {
        let Some((labels, value)) = line
            .strip_prefix("apiserver_request_total{")
            .and_then(|rest| rest.rsplit_once("} "))
        else {
            continue;
        };
        let label = |key: &str| {
            labels
                .split(',')
                .find_map(|pair| {
                    pair.strip_prefix(key)?
                        .strip_prefix("=\"")?
                        .strip_suffix('"')
                })
                .unwrap_or_default()
        };
        let resource: Vec<&str> = [label("group"), label("resource"), label("subresource")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect();
        let key = format!("{} {} {}", label("verb"), resource.join("/"), label("code"));
        *counts.entry(key).or_default() += value.parse::<u64>().unwrap_or_default();
    }
    Ok(counts)
}

fn delta(before: &BTreeMap<String, u64>, after: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    after
        .iter()
        .map(|(key, count)| {
            (
                key.clone(),
                count.saturating_sub(before.get(key).copied().unwrap_or_default()),
            )
        })
        .filter(|(_, change)| *change > 0)
        .collect()
}

/// The controller's log lines since the gate started; empty if unreadable.
fn controller_log(kubectl: &Kubectl, elapsed: Duration) -> Vec<String> {
    let since = format!("--since={}s", elapsed.as_secs() + 60);
    kubectl
        .run(&[
            "logs",
            "deployment/proofstormd",
            "-n",
            CONTROL_NAMESPACE,
            &since,
        ])
        .map(|log| log.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Count the controller's error and warning lines.
fn log_summary(log: &[String]) -> Value {
    let mut result = json!({"lines":log.len()});
    for level in ["ERROR", "WARN"] {
        let matching: Vec<&String> = log.iter().filter(|line| line.contains(level)).collect();
        result[level] = json!({"count":matching.len(),
            "first":matching.iter().take(20).map(|line| line.chars().take(400).collect::<String>()).collect::<Vec<_>>()});
    }
    result
}

/// Samples the control namespace's pod usage every ten seconds.
struct Sampler {
    stop: Arc<AtomicBool>,
    phase: Arc<Mutex<String>>,
    handle: thread::JoinHandle<Vec<Value>>,
}

impl Sampler {
    fn start(context: &GateContext) -> Result<Self> {
        let kubectl = Kubectl::for_installation(&context.installation)?;
        let stop = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(Mutex::new("start".to_owned()));
        let (stopped, current) = (stop.clone(), phase.clone());
        let path = format!("/apis/metrics.k8s.io/v1beta1/namespaces/{CONTROL_NAMESPACE}/pods");
        let handle = thread::spawn(move || {
            let started = Instant::now();
            let mut samples = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                let usage = kubectl
                    .run(&["get", "--raw", &path])
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok());
                let phase = current
                    .lock()
                    .map(|phase| phase.clone())
                    .unwrap_or_default();
                for pod in usage
                    .as_ref()
                    .and_then(|usage| usage["items"].as_array())
                    .into_iter()
                    .flatten()
                {
                    let total = |resource: &str| -> f64 {
                        pod["containers"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|container| container["usage"][resource].as_str())
                            .filter_map(quantity)
                            .sum()
                    };
                    samples.push(json!({"seconds":started.elapsed().as_secs(),"phase":phase,
                        "pod":pod["metadata"]["name"],"cpu_millicores":(total("cpu") * 1000.0).round(),
                        "memory_mib":(total("memory") / 1_048_576.0).round()}));
                }
                for _ in 0..10 {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
            samples
        });
        Ok(Self {
            stop,
            phase,
            handle,
        })
    }

    fn stop(self) -> Vec<Value> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

/// Peak and mean usage per phase and pod.
fn usage_summary(samples: &[Value]) -> Value {
    let mut grouped: BTreeMap<(String, String), Vec<(f64, f64)>> = BTreeMap::new();
    for sample in samples {
        let key = (
            sample["phase"].as_str().unwrap_or_default().to_owned(),
            sample["pod"].as_str().unwrap_or_default().to_owned(),
        );
        grouped.entry(key).or_default().push((
            sample["cpu_millicores"].as_f64().unwrap_or_default(),
            sample["memory_mib"].as_f64().unwrap_or_default(),
        ));
    }
    let mut summary = serde_json::Map::new();
    for ((phase, pod), values) in grouped {
        #[allow(clippy::cast_precision_loss)]
        let mean = values.iter().map(|(cpu, _)| cpu).sum::<f64>() / values.len() as f64;
        summary.entry(phase).or_insert_with(|| json!({}))[pod] = json!({
            "samples":values.len(),
            "cpu_millicores_max":values.iter().map(|(cpu, _)| *cpu).fold(0.0, f64::max),
            "cpu_millicores_mean":mean.round(),
            "memory_mib_max":values.iter().map(|(_, memory)| *memory).fold(0.0, f64::max),
        });
    }
    Value::Object(summary)
}
