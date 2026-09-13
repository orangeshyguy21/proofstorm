//! Deliberately prevent the lost Job from starting: test replay fencing, not a race.
use super::{common::scoped, support::ControllerPause};
use crate::{GateContext, McpClient, cell, gate::CONTROL_NAMESPACE, json as expect};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

fn request(operation: &str) -> Value {
    scoped(
        operation,
        json!({"from_component":"wallet","to_component":"mint","service":"http",
        "timeout_seconds":2,"attempts":1,"idempotency_key":operation}),
    )
}

pub(super) fn run(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    _instance_key: &str,
) -> Result<()> {
    let kubectl = &context.kubectl;
    let pods = kubectl.get_json(&["get", "pods", "-n", namespace])?;
    let count = expect::array(&pods, "/items")?
        .iter()
        .filter(|pod| {
            !matches!(
                pod.pointer("/status/phase").and_then(Value::as_str),
                Some("Succeeded" | "Failed")
            )
        })
        .count()
        .to_string();
    kubectl.apply_stdin(
        &json!({"apiVersion":"v1","kind":"ResourceQuota",
        "metadata":{"name":"recovery-hold","namespace":namespace},
        "spec":{"hard":{"pods":count.to_string()}}})
        .to_string(),
    )?;
    // Wait for quota accounting before admitting the Job; existing pods remain.
    let mut accounted = false;
    for _ in 0..30 {
        let quota = kubectl.get_json(&["get", "resourcequota/recovery-hold", "-n", namespace])?;
        if quota.pointer("/status/hard/pods").and_then(Value::as_str) == Some(count.as_str())
            && quota.pointer("/status/used/pods").and_then(Value::as_str) == Some(count.as_str())
        {
            accounted = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    ensure!(accounted, "recovery quota did not become active");
    let lost = client.call("reachability_oracle", request("lost-probe"))?;
    let resource = expect::string(&lost, "/resource_name")?;
    let mut fenced = false;
    for _ in 0..60 {
        let action = kubectl.get_json(&[
            "get",
            "proofstormcellaction",
            resource,
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        if action.pointer("/status/phase").and_then(Value::as_str) == Some("Running")
            && !kubectl
                .run(&[
                    "get",
                    "job",
                    resource,
                    "-n",
                    namespace,
                    "--ignore-not-found",
                    "-o",
                    "name",
                ])?
                .is_empty()
        {
            fenced = true;
            break;
        }
        sleep(Duration::from_millis(250));
    }
    ensure!(fenced, "lost Job never recorded its execution fence");
    let selector = format!("job-name={resource}");
    ensure!(
        kubectl
            .run(&[
                "get", "pods", "-n", namespace, "-l", &selector, "-o", "name"
            ])?
            .is_empty(),
        "recovery quota failed to hold the lost Job before execution"
    );
    let pause = ControllerPause::stop(kubectl)?;
    if std::env::var("PROOFSTORM_ACCEPTANCE_INJECT_FAILURE").as_deref() == Ok("controller-stopped")
    {
        bail!("injected failure while controller stopped");
    }
    kubectl.run(&[
        "delete",
        "job",
        resource,
        "-n",
        namespace,
        "--wait=true",
        "--timeout=30s",
    ])?;
    pause.resume()?;
    kubectl.run(&["delete", "resourcequota/recovery-hold", "-n", namespace])?;
    let failed = cell::wait_operation_phase(client, "lost-probe", "failed", 120)?;
    ensure!(
        cell::artifact_content(&failed)?["code"] == "action_job_lost",
        "lost Job had wrong terminal error: {failed}"
    );
    ensure!(
        kubectl
            .run(&[
                "get",
                "job",
                resource,
                "-n",
                namespace,
                "--ignore-not-found",
                "-o",
                "name"
            ])?
            .is_empty(),
        "controller replayed a lost Job"
    );

    let pause = ControllerPause::stop(kubectl)?;
    let accepted = client.call("reachability_oracle", request("cancelled-probe"))?;
    let cancel = json!({"operation_id":"cancelled-probe","idempotency_key":"cancel-probe"});
    let first = client.call("action_cancel", cancel.clone())?;
    let retry = client.call("action_cancel", cancel)?;
    ensure!(
        first["resource_name"] == accepted["resource_name"]
            && retry["resource_name"] == accepted["resource_name"]
            && retry["sequence"] == accepted["sequence"],
        "cancellation retry changed identity"
    );
    pause.resume()?;
    let cancelled = cell::wait_operation_phase(client, "cancelled-probe", "cancelled", 120)?;
    ensure!(
        cell::artifact_content(&cancelled)?["code"] == "action_cancelled",
        "wrong cancellation error: {cancelled}"
    );
    let selector = format!(
        "proofstorm.dev/action={}",
        expect::string(&accepted, "/resource_name")?
    );
    ensure!(
        kubectl
            .run(&[
                "get", "jobs", "-n", namespace, "-l", &selector, "-o", "name"
            ])?
            .is_empty(),
        "cancelled action created a Job across controller restart"
    );
    Ok(())
}
