//! One bounded sampler per server. HTTP reads only return its cached snapshot.
mod balances;
mod channels;
mod holdings;
mod retention;
use crate::{Error, lab::Labs};
use futures::{StreamExt, stream};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, ResourceExt,
    api::{ApiResource, DynamicObject, ListParams},
};
use proofstorm_core::Capability;
use proofstorm_kube::{COMPONENT_LABEL, INSTANCE_LABEL, ProofstormLab, instance_namespace};
use proofstorm_view::{LabUsage, ProcessUsage, SystemView, UsageTotals};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{sync::watch, task::JoinHandle};

pub struct Telemetry {
    pub receiver: watch::Receiver<SystemView>,
    task: JoinHandle<()>,
}

impl Telemetry {
    #[must_use]
    pub fn start(labs: Labs) -> Self {
        let (sender, receiver) = watch::channel(SystemView::default());
        let task = tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(5));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let mut snapshot = match labs.system().await {
                    Ok(snapshot) => snapshot,
                    Err(_) => SystemView {
                        error: Some("System measurements unavailable.".into()),
                        ..SystemView::default()
                    },
                };
                retention::retain(&mut snapshot, &sender.borrow());
                sender.send_replace(snapshot);
            }
        });
        Self { receiver, task }
    }
}
impl Drop for Telemetry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Labs {
    pub async fn system(&self) -> Result<SystemView, Error> {
        for capability in [
            Capability::LabRead,
            Capability::LabStatus,
            Capability::ExperimentRead,
        ] {
            self.store
                .authorize(&self.workspace, &self.principal, capability)?;
        }
        let api = Api::<ProofstormLab>::namespaced(
            self.runtime.client.clone(),
            &self.runtime.control_namespace,
        );
        let resources =
            tokio::time::timeout(Duration::from_secs(3), api.list(&ListParams::default()))
                .await
                .map_err(|_| Error::failure("system inventory timed out", None))??;
        let mut scoped = Vec::new();
        for lab in resources
            .items
            .into_iter()
            .filter(|lab| lab.spec.workspace_id == self.workspace)
        {
            match self.store.environment_entry(
                &self.workspace,
                &self.principal,
                &lab.spec.instance_id,
            ) {
                Ok(entry) => scoped.push((lab, entry.handle.map(|handle| handle.name))),
                Err(proofstorm_store::StoreError::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut labs = stream::iter(scoped)
            .map(|(lab, name)| {
                let service = (*self).clone();
                async move { service.sample_lab(&lab, name).await }
            })
            .buffer_unordered(2)
            .collect::<Vec<_>>()
            .await;
        labs.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        let mut totals =
            UsageTotals::from_processes(labs.iter().flat_map(|lab| lab.processes.iter()));
        if labs.iter().any(|lab| lab.error.is_some()) {
            totals.cpu_millicores = None;
            totals.memory_bytes = None;
        }
        Ok(SystemView {
            sampled_at_unix: now(),
            error: None,
            labs,
            totals,
        })
    }

    async fn sample_lab(&self, lab: &ProofstormLab, name: Option<String>) -> LabUsage {
        let mut usage = LabUsage {
            incarnation: format!("{}:{}", lab.spec.workspace_id, lab.spec.instance_key),
            id: lab.spec.instance_id.clone(),
            name: name.unwrap_or_else(|| lab.spec.instance_id.clone()),
            ..LabUsage::default()
        };
        if self
            .store
            .instance(&self.workspace, &self.principal, &lab.spec.instance_id)
            .ok()
            .is_none_or(|instance| {
                instance.instance_key != lab.spec.instance_key
                    || instance.resource_name != lab.name_any()
            })
        {
            usage.error = Some("Runtime identity unavailable.".into());
            return usage;
        }
        let namespace = instance_namespace(&lab.spec.instance_key);
        let pods = Api::<Pod>::namespaced(self.runtime.client.clone(), &namespace);
        let params =
            ListParams::default().labels(&format!("{INSTANCE_LABEL}={}", lab.spec.instance_key));
        let resource = ApiResource {
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            api_version: "metrics.k8s.io/v1beta1".into(),
            kind: "PodMetrics".into(),
            plural: "pods".into(),
        };
        let metrics = Api::<DynamicObject>::namespaced_with(
            self.runtime.client.clone(),
            &namespace,
            &resource,
        );
        let metric_params = ListParams::default();
        let (pod_list, metric_list) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(3), pods.list(&params)),
            tokio::time::timeout(Duration::from_secs(3), metrics.list(&metric_params))
        );
        let Ok(Ok(pod_list)) = pod_list else {
            usage.error = Some("Processes unavailable.".into());
            return usage;
        };
        let measurements = metric_list.ok().and_then(Result::ok);
        if measurements.is_none() {
            usage.metrics_error = Some("CPU and memory metrics unavailable.".into());
        }
        for pod in &pod_list.items {
            let measurement = measurements.as_ref().and_then(|list| {
                list.items
                    .iter()
                    .find(|metric| metric.name_any() == pod.name_any())
            });
            usage.processes.extend(processes(pod, measurement));
        }
        usage
            .processes
            .sort_by(|a, b| (&a.pod, &a.container).cmp(&(&b.pod, &b.container)));
        usage.totals = UsageTotals::from_processes(usage.processes.iter());
        // A fixed allowlist of passive readers. Never submit managed actions or run wallet SDKs.
        if self
            .store
            .authorize(
                &self.workspace,
                &self.principal,
                Capability::ComponentExecLive,
            )
            .is_ok()
        {
            usage.balances = balances::sample(lab, &pods, &pod_list.items).await;
        }
        usage
    }
}

fn processes(pod: &Pod, metric: Option<&DynamicObject>) -> Vec<ProcessUsage> {
    let Some(spec) = &pod.spec else {
        return vec![];
    };
    let statuses = pod
        .status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref());
    let init_statuses = pod
        .status
        .as_ref()
        .and_then(|status| status.init_container_statuses.as_ref());
    spec.containers
        .iter()
        .chain(spec.init_containers.iter().flatten())
        .map(|container| {
            let status = statuses
                .into_iter()
                .flatten()
                .chain(init_statuses.into_iter().flatten())
                .find(|s| s.name == container.name);
            let state = status.and_then(|s| s.state.as_ref());
            let running = state.is_some_and(|s| s.running.is_some());
            let terminated = state.is_some_and(|s| s.terminated.is_some());
            let state = state.map_or_else(
                || "Pending".into(),
                |s| {
                    if s.running.is_some() {
                        "Running".into()
                    } else if let Some(waiting) = &s.waiting {
                        waiting.reason.clone().unwrap_or_else(|| "Waiting".into())
                    } else {
                        s.terminated
                            .as_ref()
                            .and_then(|t| t.reason.clone())
                            .unwrap_or_else(|| "Stopped".into())
                    }
                },
            );
            let fresh_metric = metric.filter(|metric| metric_is_current(metric, pod));
            let measured = fresh_metric
                .and_then(|m| m.data["containers"].as_array())
                .and_then(|containers| containers.iter().find(|c| c["name"] == container.name))
                .filter(|_| running);
            let requests = container
                .resources
                .as_ref()
                .and_then(|r| r.requests.as_ref());
            let limits = container.resources.as_ref().and_then(|r| r.limits.as_ref());
            ProcessUsage {
                pod: pod.name_any(),
                container: container.name.clone(),
                component: pod.labels().get(COMPONENT_LABEL).cloned(),
                state,
                running,
                terminated,
                ready: status.is_some_and(|s| s.ready),
                restarts: status.map_or(0, |s| s.restart_count),
                cpu_millicores: measured
                    .and_then(|m| quantity(m["usage"]["cpu"].as_str()?))
                    .map(|cpu| cpu * 1000.0),
                memory_bytes: measured.and_then(|m| quantity(m["usage"]["memory"].as_str()?)),
                metrics_timestamp: metric
                    .and_then(|m| m.data["timestamp"].as_str().map(str::to_owned)),
                cpu_request_millicores: requests
                    .and_then(|r| quantity(&r.get("cpu")?.0))
                    .map(|cpu| cpu * 1000.0),
                memory_request_bytes: requests.and_then(|r| quantity(&r.get("memory")?.0)),
                cpu_limit_millicores: limits
                    .and_then(|r| quantity(&r.get("cpu")?.0))
                    .map(|cpu| cpu * 1000.0),
                memory_limit_bytes: limits.and_then(|r| quantity(&r.get("memory")?.0)),
            }
        })
        .collect()
}

fn metric_is_current(metric: &DynamicObject, pod: &Pod) -> bool {
    let Some(timestamp) = metric.data["timestamp"]
        .as_str()
        .and_then(|value| value.parse::<k8s_openapi::jiff::Timestamp>().ok())
    else {
        return false;
    };
    let age = now().saturating_sub(timestamp.as_second());
    if !(-5..=90).contains(&age) {
        return false;
    }
    pod.status
        .as_ref()
        .and_then(|s| s.start_time.as_ref())
        .is_none_or(|start| timestamp >= start.0)
}

/// Kubernetes decimal and binary quantities, including metrics-server nanocores.
fn quantity(value: &str) -> Option<f64> {
    let suffixes = [
        ("Ki", 1024.0),
        ("Mi", 1024.0_f64.powi(2)),
        ("Gi", 1024.0_f64.powi(3)),
        ("Ti", 1024.0_f64.powi(4)),
        ("Pi", 1024.0_f64.powi(5)),
        ("Ei", 1024.0_f64.powi(6)),
        ("n", 1e-9),
        ("u", 1e-6),
        ("m", 1e-3),
        ("k", 1e3),
        ("K", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    let (number, factor) = suffixes
        .iter()
        .find_map(|(suffix, factor)| value.strip_suffix(suffix).map(|number| (number, *factor)))
        .unwrap_or((value, 1.0));
    let number = number.parse::<f64>().ok()? * factor;
    (number.is_finite() && number >= 0.0).then_some(number)
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_metrics_without_confusing_cpu_and_binary_memory() {
        for (input, expected) in [
            ("125000000n", 0.125),
            ("250m", 0.25),
            ("2", 2.0),
            ("64Mi", 67_108_864.0),
            ("1e3", 1000.0),
        ] {
            assert!((quantity(input).unwrap() - expected).abs() < 1e-9);
        }
        for input in ["NaN", "-1", "1foo", "inf"] {
            assert!(quantity(input).is_none());
        }
    }
    #[test]
    fn missing_and_terminated_metrics_are_never_reported_as_running_usage() {
        let pod: Pod = serde_json::from_value(serde_json::json!({"metadata":{"name":"pod"},"spec":{"containers":[{"name":"a"},{"name":"b"},{"name":"c"}]},"status":{"containerStatuses":[
            {"name":"a","image":"x","imageID":"x","ready":true,"restartCount":2,"state":{"running":{}}},
            {"name":"b","image":"x","imageID":"x","ready":false,"restartCount":0,"state":{"running":{}}},
            {"name":"c","image":"x","imageID":"x","ready":false,"restartCount":0,"state":{"terminated":{"exitCode":0}}}
        ]}})).unwrap();
        let metric: DynamicObject = serde_json::from_value(serde_json::json!({"metadata":{"name":"pod"},"timestamp":k8s_openapi::jiff::Timestamp::now().to_string(),"containers":[{"name":"a","usage":{"cpu":"10m","memory":"1Mi"}},{"name":"c","usage":{"cpu":"500m","memory":"2Gi"}}]})).unwrap();
        let rows = processes(&pod, Some(&metric));
        let totals = UsageTotals::from_processes(rows.iter());
        assert_eq!((totals.running, totals.sampled, totals.restarts), (2, 1, 2));
        assert_eq!(totals.cpu_millicores, Some(10.0));
        assert!(rows[1].cpu_millicores.is_none());
        assert!(rows[2].cpu_millicores.is_none());
    }
}
