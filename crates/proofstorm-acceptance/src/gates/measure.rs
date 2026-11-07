//! Measurement shared by the scale and prober gates: timed steps, sampled
//! pod usage, API request counts and the controller's log, recorded to
//! `<label>.json` in the run directory.
use crate::{
    GateContext, McpClient, cell,
    kubectl::{CONTROL_NAMESPACE, Kubectl},
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
    time::{Duration, Instant},
};

/// The control namespace's pods, for the controller's usage.
pub(super) fn control_pods() -> String {
    format!("/apis/metrics.k8s.io/v1beta1/namespaces/{CONTROL_NAMESPACE}/pods")
}

/// Run `scenario` while sampling pod usage from the metrics API `paths`, then
/// record everything under `label`, even when the scenario failed.
pub(super) fn measured(
    context: &GateContext,
    label: &str,
    paths: Vec<String>,
    scenario: impl FnOnce(&mut Report) -> Result<()>,
) -> Result<()> {
    let sampler = Sampler::start(context, paths)?;
    let mut report = Report {
        label: label.to_owned(),
        started: Instant::now(),
        phase: sampler.phase.clone(),
        steps: Vec::new(),
        facts: serde_json::Map::new(),
    };
    let before = api_requests(&context.kubectl)?;
    let result = scenario(&mut report);
    let samples = sampler.stop();
    let after = api_requests(&context.kubectl);
    let mut document = json!({
        "passed": result.is_ok(),
        "seconds": report.started.elapsed().as_secs_f64(),
        "steps": report.steps,
        "facts": report.facts,
        "usage": usage_summary(&samples),
    });
    if let Ok(after) = &after {
        document["api_errors"] = json!(
            delta(&before, after)
                .into_iter()
                .filter(|(key, _)| key
                    .rsplit(' ')
                    .next()
                    .is_some_and(|code| code.starts_with(['4', '5'])))
                .collect::<BTreeMap<_, _>>()
        );
    }
    let log = controller_log(&context.kubectl, report.started.elapsed());
    document["controller_log"] = log_summary(&log);
    context.record(&format!("{label}.json"), &document)?;
    context.record(&format!("{label}-controller-log.json"), &json!(log))?;
    context.record(&format!("{label}-usage.json"), &json!(samples))?;
    eprintln!(
        "{label}: steps {}",
        serde_json::to_string(&document["steps"])?
    );
    result?;
    after?;
    Ok(())
}

pub(super) struct Report {
    label: String,
    started: Instant,
    phase: Arc<Mutex<String>>,
    steps: Vec<Value>,
    pub facts: serde_json::Map<String, Value>,
}

impl Report {
    /// Name the current phase in the gate log and in later usage samples.
    pub fn phase(&self, name: &str) {
        eprintln!("{}: {name}", self.label);
        if let Ok(mut phase) = self.phase.lock() {
            name.clone_into(&mut phase);
        }
    }

    pub fn step(&mut self, step: &str, started: Instant) {
        self.steps
            .push(json!({"step":step,"seconds":started.elapsed().as_secs_f64()}));
    }

    /// One timed MCP call, with its result size.
    pub fn call(
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

/// Wait up to `minutes` for readiness, at `generation` when given.
pub(super) fn wait_ready(
    client: &mut McpClient,
    name: &str,
    generation: Option<u64>,
    minutes: usize,
) -> Result<()> {
    for _ in 0..minutes {
        let mut request = json!({"name":name,"target_phase":"ready","timeout_seconds":60});
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
    bail!("{name} did not become ready within {minutes} minutes")
}

/// Long-poll one operation to success, so timings aren't rounded to a poll interval.
pub(super) fn succeeded(client: &mut McpClient, id: &str) -> Result<Value> {
    let operation = cell::wait_one(client, id, 120)?;
    ensure!(
        operation["phase"] == "succeeded",
        "operation {id} did not succeed: {operation}"
    );
    Ok(operation)
}

/// Writes in the next `window`, with the total for Proofstorm's own resources.
pub(super) fn idle_writes(kubectl: &Kubectl, window: Duration) -> Result<(u64, Value)> {
    let before = api_requests(kubectl)?;
    thread::sleep(window);
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
    Ok((
        proofstorm,
        json!({"seconds":window.as_secs(),"proofstorm_writes":proofstorm,"by_request":writes}),
    ))
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
            "--timestamps",
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

/// Samples pod usage from metrics API paths every ten seconds.
struct Sampler {
    stop: Arc<AtomicBool>,
    phase: Arc<Mutex<String>>,
    handle: thread::JoinHandle<Vec<Value>>,
}

impl Sampler {
    fn start(context: &GateContext, paths: Vec<String>) -> Result<Self> {
        let kubectl = Kubectl::for_installation(&context.installation)?;
        let stop = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(Mutex::new("start".to_owned()));
        let (stopped, current) = (stop.clone(), phase.clone());
        let handle = thread::spawn(move || {
            let started = Instant::now();
            let mut samples = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                let phase = current
                    .lock()
                    .map(|phase| phase.clone())
                    .unwrap_or_default();
                for path in &paths {
                    let usage = kubectl
                        .run(&["get", "--raw", path])
                        .ok()
                        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
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
