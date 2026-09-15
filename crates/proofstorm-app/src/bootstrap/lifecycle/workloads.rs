//! Preserve controller identities and replica intent while giving pods normal termination grace.
use super::super::{healthy, kube};
use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Workload {
    kind: String,
    namespace: String,
    name: String,
    uid: String,
    pub replicas: u32,
    pub persistent: bool,
    selector: BTreeMap<String, String>,
}

fn get(installation: &Installation, args: &[&str]) -> Result<Value> {
    let mut args = args.to_vec();
    args.extend(["-o", "json"]);
    serde_json::from_str(&kube(installation, &args)?).map_err(Into::into)
}

fn items(value: &Value) -> Result<&Vec<Value>> {
    value["items"]
        .as_array()
        .context("runtime inventory has no items")
}

fn snapshot(value: &Value) -> Result<Workload> {
    let field = |pointer| -> Result<String> {
        Ok(value
            .pointer(pointer)
            .and_then(Value::as_str)
            .context("workload identity missing")?
            .into())
    };
    ensure!(
        matches!(value["kind"].as_str(), Some("Deployment" | "StatefulSet")),
        "unsupported workload kind"
    );
    ensure!(
        value
            .pointer("/spec/persistentVolumeClaimRetentionPolicy/whenScaled")
            .is_none_or(|v| v != "Delete"),
        "workload deletes storage when scaled down; refusing suspension"
    );
    let selector: BTreeMap<String, String> =
        serde_json::from_value(value["spec"]["selector"]["matchLabels"].clone())?;
    ensure!(
        !selector.is_empty()
            && value["spec"]["selector"]["matchExpressions"]
                .as_array()
                .is_none_or(Vec::is_empty),
        "unsupported workload selector"
    );
    Ok(Workload {
        kind: field("/kind")?,
        namespace: field("/metadata/namespace")?,
        name: field("/metadata/name")?,
        uid: field("/metadata/uid")?,
        replicas: value["spec"]["replicas"].as_u64().unwrap_or(1).try_into()?,
        persistent: value["spec"]["volumeClaimTemplates"]
            .as_array()
            .is_some_and(|claims| !claims.is_empty())
            || value["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .is_some_and(|volumes| {
                    volumes
                        .iter()
                        .any(|volume| !volume["persistentVolumeClaim"].is_null())
                }),
        selector,
    })
}

pub(super) fn controller(installation: &Installation) -> Result<Workload> {
    snapshot(&get(
        installation,
        &[
            "get",
            "deployment/proofstormd",
            "-n",
            crate::config::DEFAULT_NAMESPACE,
        ],
    )?)
}

pub(super) fn inventory(installation: &Installation) -> Result<Vec<Workload>> {
    let value = get(
        installation,
        &[
            "get",
            "deployments,statefulsets",
            "-A",
            "-l",
            proofstorm_kube::INSTANCE_LABEL,
        ],
    )?;
    items(&value)?.iter().map(snapshot).collect()
}

fn object(installation: &Installation, workload: &Workload) -> Result<Value> {
    let value = get(
        installation,
        &[
            "get",
            &format!("{}/{}", workload.kind, workload.name),
            "-n",
            &workload.namespace,
        ],
    )?;
    ensure!(
        value["metadata"]["uid"] == workload.uid,
        "workload {} was replaced; refusing to change it",
        workload.name
    );
    Ok(value)
}

pub(super) fn scale(installation: &Installation, workload: &Workload, replicas: u32) -> Result<()> {
    let value = object(installation, workload)?;
    let current = value["spec"]["replicas"].as_u64().unwrap_or(1);
    ensure!(
        current == 0 || current == u64::from(workload.replicas),
        "workload replicas changed outside this transition"
    );
    if current == u64::from(replicas) {
        return Ok(());
    }
    // Test immutable identity and prior replicas in the same API operation as the write.
    let patch = json!([
        {"op":"test","path":"/metadata/uid","value":workload.uid},
        {"op":"test","path":"/spec/replicas","value":current},
        {"op":"replace","path":"/spec/replicas","value":replicas}
    ])
    .to_string();
    kube(
        installation,
        &[
            "patch",
            &format!("{}/{}", workload.kind, workload.name),
            "-n",
            &workload.namespace,
            "--type=json",
            "-p",
            &patch,
        ],
    )?;
    Ok(())
}

async fn pause(deadline: Instant, message: &str) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "{message}; increase --timeout or retry the transition"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

fn selector(workload: &Workload) -> String {
    workload
        .selector
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) async fn wait_scheduled(
    installation: &Installation,
    workloads: &[Workload],
    deadline: Instant,
) -> Result<()> {
    loop {
        let mut scheduled = true;
        for workload in workloads.iter().filter(|workload| workload.replicas > 0) {
            object(installation, workload)?;
            let pods = get(
                installation,
                &[
                    "get",
                    "pods",
                    "-n",
                    &workload.namespace,
                    "-l",
                    &selector(workload),
                ],
            )?;
            scheduled &= items(&pods)?
                .iter()
                .filter(|pod| {
                    pod["metadata"]["deletionTimestamp"].is_null()
                        && pod["spec"]["nodeName"]
                            .as_str()
                            .is_some_and(|node| !node.is_empty())
                })
                .count()
                == workload.replicas as usize;
        }
        if scheduled {
            return Ok(());
        }
        pause(
            deadline,
            "persistent workloads have not been assigned to their storage nodes",
        )
        .await?;
    }
}

pub(super) async fn wait_api(installation: &Installation, deadline: Instant) -> Result<()> {
    loop {
        if kube(installation, &["get", "--raw=/readyz"]).is_ok() {
            return Ok(());
        }
        pause(deadline, "Kubernetes is not ready").await?;
    }
}

pub(super) async fn wait_stopped(
    installation: &Installation,
    workloads: &[Workload],
    deadline: Instant,
) -> Result<()> {
    loop {
        let mut stopped = true;
        for workload in workloads {
            object(installation, workload)?;
            let selector = selector(workload);
            let pods = get(
                installation,
                &["get", "pods", "-n", &workload.namespace, "-l", &selector],
            )?;
            stopped &= items(&pods)?.is_empty();
        }
        if stopped {
            return Ok(());
        }
        pause(
            deadline,
            "services are still shutting down; no forced pod deletion was performed",
        )
        .await?;
    }
}

fn idle(value: &Value) -> bool {
    matches!(
        value["status"]["phase"].as_str(),
        Some("Succeeded" | "Failed" | "Cancelled" | "succeeded" | "failed" | "cancelled")
    )
}

fn cell_settled(cell: &Value) -> bool {
    let desired = cell["metadata"]["annotations"][crate::updates::GENERATION]
        .as_str()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    cell["metadata"]["deletionTimestamp"].is_null()
        && !matches!(
            cell["status"]["phase"].as_str(),
            Some("Closing" | "CleanupBlocked")
        )
        && cell["status"]["observedDesiredGeneration"].as_u64() == Some(desired)
}

pub(super) async fn drain(installation: &Installation, deadline: Instant) -> Result<()> {
    loop {
        let actions = get(
            installation,
            &[
                "get",
                "proofstormcellactions,proofstormcandidatebuilds",
                "-n",
                crate::config::DEFAULT_NAMESPACE,
            ],
        )?;
        let cells = get(
            installation,
            &[
                "get",
                "proofstormcells",
                "-n",
                crate::config::DEFAULT_NAMESPACE,
            ],
        )?;
        let settled = items(&cells)?.iter().all(cell_settled);
        let pods = get(installation, &["get", "pods", "-A"])?;
        let jobs_idle = items(&pods)?.iter().all(|pod| {
            !pod["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners.iter().any(|owner| owner["kind"] == "Job"))
                || matches!(
                    pod["status"]["phase"].as_str(),
                    Some("Succeeded" | "Failed")
                )
        });
        if settled && jobs_idle && items(&actions)?.iter().all(idle) {
            return Ok(());
        }
        pause(
            deadline,
            "active operations or cell edits have not settled; they were not cancelled",
        )
        .await?;
    }
}

fn ready(workload: &Workload, value: &Value) -> bool {
    value["spec"]["replicas"].as_u64() == Some(u64::from(workload.replicas))
        && (workload.replicas == 0
            || (value["status"]["observedGeneration"] == value["metadata"]["generation"]
                && value["status"]["readyReplicas"].as_u64() == Some(u64::from(workload.replicas))))
}

pub(super) async fn wait_ready(
    installation: &Installation,
    saved: Option<&[Workload]>,
    deadline: Instant,
) -> Result<Vec<String>> {
    let expected = serde_json::from_slice::<Value>(&std::fs::read(
        installation.home.join("deployment-inputs.json"),
    )?)?;
    ensure!(
        expected["installation_id"] == installation.id && expected["image"].is_string(),
        "deployment identity missing or changed"
    );
    let current;
    let workloads = if let Some(saved) = saved {
        saved
    } else {
        current = inventory(installation)?;
        &current
    };
    loop {
        let controller_health = healthy(installation, &expected);
        let mut unready = Vec::new();
        for workload in workloads {
            if !ready(workload, &object(installation, workload)?) {
                unready.push(format!("{}/{}", workload.namespace, workload.name));
            }
        }
        if controller_health.is_ok() && (unready.is_empty() || Instant::now() >= deadline) {
            // Restoration succeeded. Let clients diagnose unhealthy services instead
            // of trapping the operator behind the suspension admission gate.
            return Ok(unready);
        }
        if Instant::now() >= deadline {
            controller_health?;
        }
        pause(deadline, "controller or restored workloads are not ready").await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_uses_generation_annotation_and_both_action_phase_encodings() {
        let mut cell =
            json!({"metadata":{},"status":{"phase":"Ready","observedDesiredGeneration":1}});
        assert!(cell_settled(&cell));
        cell["metadata"]["annotations"] = json!({crate::updates::GENERATION:"2"});
        assert!(!cell_settled(&cell));
        cell["status"]["observedDesiredGeneration"] = json!(2);
        assert!(cell_settled(&cell));
        cell["metadata"]["deletionTimestamp"] = json!("now");
        assert!(!cell_settled(&cell));
        for phase in [
            "Succeeded",
            "succeeded",
            "Failed",
            "failed",
            "Cancelled",
            "cancelled",
        ] {
            assert!(idle(&json!({"status":{"phase":phase}})));
        }
        for phase in ["Pending", "Running", "building", "pushing"] {
            assert!(!idle(&json!({"status":{"phase":phase}})));
        }
        assert!(!idle(&json!({})));
    }

    #[test]
    fn retained_claims_establish_placement_before_stateless_services() {
        let mut value = json!({"kind":"StatefulSet","metadata":{"name":"btc","namespace":"cell","uid":"original"},
            "spec":{"replicas":1,"selector":{"matchLabels":{"component":"btc"}},
                "volumeClaimTemplates":[{"metadata":{"name":"data"}}]}});
        assert!(snapshot(&value).unwrap().persistent);
        value["spec"]
            .as_object_mut()
            .unwrap()
            .remove("volumeClaimTemplates");
        assert!(!snapshot(&value).unwrap().persistent);
        value["kind"] = json!("Deployment");
        value["spec"]["template"] =
            json!({"spec":{"volumes":[{"persistentVolumeClaim":{"claimName":"saved"}}]}});
        assert!(snapshot(&value).unwrap().persistent);
        value["spec"]["template"]["spec"]["volumes"] = json!([{"emptyDir":{}}]);
        assert!(!snapshot(&value).unwrap().persistent);
    }

    #[test]
    fn suspension_refuses_storage_deletion_and_preserves_stopped_components() {
        let mut value = json!({"kind":"StatefulSet","metadata":{"name":"btc","namespace":"cell","uid":"original","generation":2},
            "spec":{"replicas":0,"selector":{"matchLabels":{"component":"btc"}}}});
        let workload = snapshot(&value).unwrap();
        assert_eq!(workload.replicas, 0);
        assert!(ready(&workload, &value));
        value["spec"]["persistentVolumeClaimRetentionPolicy"] = json!({"whenScaled":"Delete"});
        assert!(snapshot(&value).is_err());
        value["spec"]["persistentVolumeClaimRetentionPolicy"] = json!({"whenScaled":"Retain"});
        value["spec"]["replicas"] = json!(1);
        let workload = snapshot(&value).unwrap();
        assert!(!ready(&workload, &value));
        value["status"] = json!({"observedGeneration":2,"readyReplicas":1});
        assert!(ready(&workload, &value));
    }
}
