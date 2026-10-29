//! Observe a real build without candidate_wait and inspect its mounted boundaries.
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use crate::{GateContext, McpClient, gate::CONTROL_NAMESPACE, json as expect};

const ID: &str = "acceptance-build-isolation";

/// The candidate's runtime resource name and its first build Pod.
pub(super) fn build_pod(context: &GateContext, id: &str) -> Result<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        let builds = context.kubectl.get_json(&[
            "get",
            "proofstormcandidatebuilds",
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        let build = expect::array(&builds, "/items")?
            .iter()
            .find(|build| build["spec"]["candidateId"] == id);
        if let Some((resource, job)) = build.and_then(|build| {
            Some((
                build["metadata"]["name"].as_str()?,
                build["status"]["jobName"].as_str()?,
            ))
        }) {
            let pods = context.kubectl.get_json(&[
                "get",
                "pods",
                "-n",
                CONTROL_NAMESPACE,
                "-l",
                &format!("job-name={job}"),
            ])?;
            if let Some(pod) = expect::array(&pods, "/items")?.first() {
                return Ok((resource.to_owned(), pod.clone()));
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    anyhow::bail!("candidate build Pod did not appear within 90 seconds");
}

/// Only disposable storage, plus a bundle source's Secret that solely the
/// `source` init container mounts, read-only.
pub(super) fn check_mounts(
    context: &GateContext,
    label: &str,
    pod: &Value,
    bundle_secret: Option<&str>,
) -> Result<()> {
    let spec = &pod["spec"];
    let containers: Vec<&Value> = expect::array(spec, "/initContainers")?
        .iter()
        .chain(expect::array(spec, "/containers")?)
        .collect();
    // Inspect Pod metadata only; never open a mounted credential or host file.
    let observation = json!({"pod":pod["metadata"]["name"],
        "automount":spec["automountServiceAccountToken"],
        "volumes":spec["volumes"],
        "mounts":containers.iter().map(|c| json!({"container":c["name"],"mounts":c["volumeMounts"]})).collect::<Vec<_>>(),
        "host_network":spec["hostNetwork"],"host_pid":spec["hostPID"]});
    context.record(&format!("{label}-pod.json"), &observation)?;
    ensure!(
        spec["automountServiceAccountToken"] == false,
        "build Pod did not disable API credential automount"
    );
    ensure!(
        spec["hostNetwork"] != true && spec["hostPID"] != true,
        "build Pod uses a host namespace"
    );
    let (secrets, disposable): (Vec<&Value>, Vec<&Value>) = expect::array(spec, "/volumes")?
        .iter()
        .partition(|volume| volume.get("secret").is_some());
    ensure!(
        !disposable.is_empty()
            && disposable
                .iter()
                .all(|volume| volume["emptyDir"].is_object()
                    && volume.as_object().is_some_and(|fields| fields.len() == 2)),
        "build Pod mounted something other than disposable build storage: {observation}"
    );
    let Some(bundle_secret) = bundle_secret else {
        ensure!(
            secrets.is_empty(),
            "build Pod mounted a Secret: {observation}"
        );
        return Ok(());
    };
    ensure!(
        secrets.len() == 1 && secrets[0]["secret"]["secretName"] == bundle_secret,
        "build Pod did not mount exactly the bundle Secret: {observation}"
    );
    for container in &containers {
        let mounts: Vec<&Value> = container["volumeMounts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|mount| mount["name"] == secrets[0]["name"])
            .collect();
        let allowed = if container["name"] == "source" {
            mounts.len() == 1 && mounts[0]["readOnly"] == true
        } else {
            mounts.is_empty()
        };
        ensure!(
            allowed,
            "bundle Secret reached {} or was writable: {observation}",
            container["name"]
        );
    }
    Ok(())
}

fn passive_completion(context: &GateContext, client: &mut McpClient) -> Result<()> {
    let deadline = Instant::now() + Duration::from_mins(31);
    let mut previous = Value::Null;
    while Instant::now() < deadline {
        // This directory reads durable state only. The GUI observer must collect
        // the runtime result independently; candidate_wait is deliberately absent.
        let listed = client.call("candidate_list", json!({"id":ID}))?;
        let items = expect::array(&listed, "/items")?;
        if let Some(candidate) = items.first() {
            let phase = &candidate["phase"];
            if phase != &previous {
                eprintln!("GUI background candidate observation: {phase}");
                previous = phase.clone();
                context.record("candidate-build-isolation-passive.json", &listed)?;
            }
            if matches!(phase.as_str(), Some("succeeded" | "failed" | "cancelled")) {
                ensure!(phase == "succeeded", "background build failed: {listed}");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    anyhow::bail!("GUI observer did not record terminal build state without candidate_wait");
}

pub(super) fn run(context: &GateContext) -> Result<()> {
    let mut client = context.managed_session("candidate-build-isolation")?;
    let gui = context.cli(&["gui", "start", "--allow-development"])?;
    context.record("candidate-build-isolation-gui.json", &gui)?;
    let outcome = (|| -> Result<()> {
        client.call(
            "candidate_build",
            json!({"candidate_id":ID,
            "implementation":"cocod-wallet","request_id":ID,
            "source":{"type":"commit","sha":super::candidates::COCO}}),
        )?;
        let (_, pod) = build_pod(context, ID)?;
        check_mounts(context, "candidate-build-isolation", &pod, None)?;
        passive_completion(context, &mut client)?;
        // Read the terminal receipt only after independent observation succeeded.
        let receipt = super::candidates::wait(context, &mut client, ID)?;
        ensure!(
            receipt["phase"] == "succeeded"
                && receipt["commit_sha"] == super::candidates::COCO
                && expect::string(&receipt, "/image")?.contains("@sha256:"),
            "terminal receipt lost immutable image/source identity"
        );
        let catalog = client.call("catalog_list", json!({"origins":["candidate"],"query":ID}))?;
        ensure!(
            expect::array(&catalog, "/items")?
                .iter()
                .any(|entry| entry["version"] == receipt["catalog_entry"]["version"]),
            "observed build did not enter the candidate image catalog"
        );
        context.record("candidate-build-isolation-catalog.json", &catalog)?;
        super::candidates::retained_logs(context, &mut client, &receipt)
    })();
    let stopped = context.cli(&["gui", "stop"]);
    context.record(
        "candidate-build-isolation-outcome.json",
        &json!({"passed":outcome.is_ok(),"error":outcome.as_ref().err().map(|e|format!("{e:#}")),
            "gui_stopped":stopped.is_ok()}),
    )?;
    outcome?;
    stopped?;
    Ok(())
}
