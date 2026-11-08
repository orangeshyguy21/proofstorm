//! Prober gate: `redis` TCP-probed components plus three Keycloaks, the
//! lightest HTTP-probed kind, each with its own Postgres.
//!
//! The ready cell's idle writes are counted, since this cell has a heartbeat.
//! Then two Keycloaks are frozen with `SIGSTOP`, so their HTTP checks time out
//! rather than fail fast, and every other probed component must stay fresh.
//! Results go to `prober-<redis>.json`.
use super::measure::{self, Report};
use crate::{GateContext, McpClient, client::clear_runtime_environment, json as expect, process};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const NAME: &str = "prober-cell";
const WORKSPACE: &str = "acceptance-prober";
const PRINCIPAL: &str = "prober-designer";
const KEYCLOAKS: usize = 3;
const STALLED: [&str; 2] = ["kc0", "kc1"];
const IDLE: Duration = Duration::from_secs(120);
const STALL: Duration = Duration::from_secs(90);
const RECOVERY: Duration = Duration::from_secs(120);

pub fn run(context: &GateContext, redis: usize) -> Result<()> {
    measure::measured(
        context,
        &format!("prober-{redis}"),
        vec![
            measure::control_pods(),
            format!(
                "/apis/metrics.k8s.io/v1beta1/pods?labelSelector={}",
                proofstorm_kube::PROTOCOL_PROBER_LABEL
            ),
        ],
        |report| scenario(context, redis, report),
    )
}

fn scenario(context: &GateContext, redis: usize, report: &mut Report) -> Result<()> {
    let mut client = context.default_session(WORKSPACE, PRINCIPAL)?;
    let mut components = Vec::new();
    let mut links = Vec::new();
    let entry = preferred(&mut client, "redis")?;
    components.extend((0..redis).map(|index| component(&entry, "redis", &format!("r{index:04}"))));
    let (keycloak, postgres) = (
        preferred(&mut client, "keycloak")?,
        preferred(&mut client, "postgresql")?,
    );
    for index in 0..KEYCLOAKS {
        let (id, database) = (format!("kc{index}"), format!("kc{index}-db"));
        components.push(component(&postgres, "postgresql", &database));
        components.push(component(&keycloak, "keycloak", &id));
        links.push(
            json!({"id":format!("{id}-database"),"kind":"database_backend","from":id,
            "to":database,"binding":{"type":"database","role":"primary"}}),
        );
    }
    report
        .facts
        .insert("components".into(), json!(components.len()));
    let spec = json!({"api_version":"proofstorm/v1alpha1","name":NAME,"components":components,"links":links});

    report.phase("create");
    let preview = report.call(
        &mut client,
        "plan",
        "cell_plan",
        json!({"name":NAME,"request_id":"prober-create","cell":spec}),
    )?;
    let started = Instant::now();
    crate::cell::apply(&mut client, &preview)?;
    measure::wait_ready(&mut client, NAME, None, 20)?;
    report.step("ready", started);
    let status = crate::cell::status(&mut client, NAME)?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_owned();
    let baseline = freshness(&mut client)?;
    let stale = stale_ids(&baseline, &[]);
    ensure!(
        stale.is_empty(),
        "ready cell has stale components {stale:?}"
    );

    report.phase("idle");
    let (writes, idle) = measure::idle_writes(&context.kubectl, IDLE)?;
    report.facts.insert("idle_writes".into(), idle);
    // One heartbeat write a second would already be far more than the checks change.
    ensure!(
        writes <= IDLE.as_secs(),
        "{writes} Proofstorm writes in {}s on an idle cell",
        IDLE.as_secs()
    );

    report.phase("stall");
    let mut samples = Vec::new();
    let stalled = super::with_cleanup(
        || {
            for id in STALLED {
                signal(context, &namespace, id, "STOP")?;
            }
            let started = Instant::now();
            while started.elapsed() < STALL {
                let sample = freshness(&mut client)?;
                samples.push(json!({"seconds":started.elapsed().as_secs(),
                    "stale":stale_ids(&sample, &STALLED),"oldest_seconds":oldest(&sample, &STALLED),
                    "stalled":STALLED.iter().map(|id| (id.to_string(), sample.get(*id).cloned().unwrap_or_default())).collect::<BTreeMap<_, _>>()}));
                thread::sleep(Duration::from_secs(2));
            }
            Ok(())
        },
        || {
            STALLED
                .iter()
                .try_for_each(|id| signal(context, &namespace, id, "CONT"))
        },
    );
    report.facts.insert("stall_samples".into(), json!(samples));
    stalled?;
    let stale: Vec<&Value> = samples
        .iter()
        .filter(|sample| {
            sample["stale"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty())
        })
        .collect();
    ensure!(
        stale.is_empty(),
        "healthy components went stale while two checks timed out: {stale:?}"
    );

    report.phase("recover");
    let started = Instant::now();
    // A timed-out check is still fresh evidence, so readiness shows recovery.
    measure::wait_ready(&mut client, NAME, None, 5)?;
    loop {
        let sample = freshness(&mut client)?;
        if stale_ids(&sample, &[]).is_empty() {
            break;
        }
        ensure!(
            started.elapsed() < RECOVERY,
            "stalled components did not recover: {sample:?}"
        );
        thread::sleep(Duration::from_secs(2));
    }
    report.step("recover", started);

    report.phase("remove");
    let started = Instant::now();
    client.call("cell_remove", json!({"name":NAME}))?;
    crate::cell::wait_closed(&mut client, NAME)?;
    report.step("remove", started);
    context.kubectl.assert_no_instance_namespaces()?;
    report.phase("done");
    Ok(())
}

pub(super) fn preferred(client: &mut McpClient, implementation: &str) -> Result<Value> {
    let catalog = client.call("catalog_list", json!({"implementations":[implementation]}))?;
    expect::array(&catalog, "/items")?
        .iter()
        .find(|entry| entry["preferred"] == true)
        .cloned()
        .with_context(|| format!("preferred {implementation} release missing"))
}

pub(super) fn component(entry: &Value, implementation: &str, id: &str) -> Value {
    json!({"id":id,"kind":entry["kind"],"implementation":implementation,"version":entry["version"],
        "config_version":entry["config_version"],"control":entry["recommended_control"],"config":{}})
}

/// Each component's protocol evidence as an agent reads it: age in seconds,
/// whether it is fresh now, and readiness.
fn freshness(client: &mut McpClient) -> Result<BTreeMap<String, Value>> {
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    let mut result = BTreeMap::new();
    let mut cursor = Value::Null;
    loop {
        let mut request = json!({"name":NAME,"limit":50});
        if !cursor.is_null() {
            request["cursor"] = cursor;
        }
        let page = client.call("cell_component_status_list", request)?;
        for component in expect::array(&page, "/components")? {
            let observation = &component["protocol_observation"];
            let (observed, expires) = (
                observation["observed_at_unix"].as_i64(),
                observation["expires_at_unix"].as_i64(),
            );
            result.insert(
                expect::string(component, "/id")?.to_owned(),
                json!({"ready":component["ready"],"age_seconds":observed.map(|at| now - at),
                    // Allow two seconds of clock difference between the Mac and the cluster.
                    "fresh":observed.zip(expires).is_some_and(|(at, until)| at <= now + 2 && now < until)}),
            );
        }
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            return Ok(result);
        }
    }
}

fn stale_ids(sample: &BTreeMap<String, Value>, except: &[&str]) -> Vec<String> {
    sample
        .iter()
        .filter(|(id, state)| !except.contains(&id.as_str()) && state["fresh"] != true)
        .map(|(id, _)| id.clone())
        .collect()
}

fn oldest(sample: &BTreeMap<String, Value>, except: &[&str]) -> Option<i64> {
    sample
        .iter()
        .filter(|(id, _)| !except.contains(&id.as_str()))
        .filter_map(|(_, state)| state["age_seconds"].as_i64())
        .max()
}

/// Send `signal` to every process of a component's main container, from its
/// k3d node: a process can't stop its own PID namespace's init.
fn signal(context: &GateContext, namespace: &str, component: &str, signal: &str) -> Result<()> {
    let pods = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        &format!("proofstorm.dev/component={component}"),
    ])?;
    let pod = expect::array(&pods, "/items")?
        .first()
        .with_context(|| format!("{component} has no pod"))?;
    let node = expect::string(pod, "/spec/nodeName")?;
    ensure!(
        node.starts_with(&format!("k3d-{}-", context.installation.cluster_name())),
        "{component} runs on a node outside this installation: {node}"
    );
    let containers = expect::array(pod, "/status/containerStatuses")?;
    let container = containers
        .iter()
        .find(|container| container["name"] == "component")
        .or_else(|| containers.first())
        .with_context(|| format!("{component} has no container"))?;
    let id = expect::string(container, "/containerID")?
        .split_once("://")
        .map(|(_, id)| id)
        .filter(|id| id.chars().all(|c| c.is_ascii_hexdigit()))
        .with_context(|| format!("{component} has an unexpected container ID"))?;
    let script = format!(
        "pid=$(crictl inspect --output go-template --template '{{{{.info.pid}}}}' {id}) && \
         ns=$(readlink /proc/$pid/ns/pid) && n=0 && \
         for p in /proc/[0-9]*; do [ \"$(readlink $p/ns/pid 2>/dev/null)\" = \"$ns\" ] && \
         kill -{signal} \"${{p#/proc/}}\" && n=$((n+1)); done; echo $n"
    );
    proofstorm_app::bootstrap::verify_runtime_identity(&context.installation)?;
    let mut command = Command::new("docker");
    clear_runtime_environment(&mut command);
    command.args(["exec", node, "sh", "-c", &script]);
    let output = process::capture(command, 60)?;
    let signalled: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or_default();
    ensure!(
        output.status.success() && signalled > 0,
        "SIG{signal} reached no process of {component}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("prober: SIG{signal} {component} ({signalled} processes)");
    Ok(())
}
