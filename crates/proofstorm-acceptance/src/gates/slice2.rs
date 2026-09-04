//! Slice 2 security spine, driven entirely through kubectl against a
//! declaratively applied lab: restricted Pod Security admission, the
//! default-deny NetworkPolicy, LimitRange reconciliation after a controller
//! restart, and a teardown that blocks until every owned object is gone.
//!
//! Ported from `tests/kubernetes/slice2-e2e.sh`, the one gate that never used
//! an MCP client.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};

use crate::{GateContext, gate::CONTROL_NAMESPACE, json as expect};

const LAB: &str = "slice2-security-spine";
const NAMESPACE: &str = "proofstorm-01slice2lab00";

pub fn run(context: &GateContext) -> Result<()> {
    let kubectl = &context.kubectl;
    let manifest = |name: &str| context.root.join(name).to_string_lossy().to_string();

    kubectl.run(&["apply", "-f", &manifest("examples/slice2-lab.yaml")])?;
    kubectl.run(&[
        "wait",
        "--for=jsonpath={.status.phase}=Ready",
        &format!("proofstormlab/{LAB}"),
        "-n",
        CONTROL_NAMESPACE,
        "--timeout=60s",
    ])?;

    let actual = kubectl.run(&[
        "get",
        &format!("proofstormlab/{LAB}"),
        "-n",
        CONTROL_NAMESPACE,
        "-o",
        "jsonpath={.status.instanceNamespace}",
    ])?;
    if actual != NAMESPACE {
        bail!("unexpected instance namespace: {actual}");
    }

    let (admitted, _, _) = kubectl.try_run(&[
        "apply",
        "-f",
        &manifest("tests/kubernetes/privileged-pod.yaml"),
    ])?;
    if admitted {
        bail!("restricted Pod Security unexpectedly admitted a privileged pod");
    }

    kubectl.run(&[
        "delete",
        "pod/network-server",
        "pod/network-client",
        "-n",
        NAMESPACE,
        "--ignore-not-found",
        "--wait=true",
    ])?;
    kubectl.run(&[
        "apply",
        "-f",
        &manifest("tests/kubernetes/network-pods.yaml"),
    ])?;
    kubectl.run(&[
        "wait",
        "--for=condition=Ready",
        "pod/network-server",
        "pod/network-client",
        "-n",
        NAMESPACE,
        "--timeout=90s",
    ])?;

    let loopback = kubectl.exec(
        NAMESPACE,
        "network-server",
        &["wget", "-T", "3", "-qO-", "http://127.0.0.1:8080"],
    )?;
    if loopback.trim() != "reachable" {
        bail!("the server pod could not reach itself: {loopback:?}");
    }

    let server_ip = kubectl.run(&[
        "get",
        "pod/network-server",
        "-n",
        NAMESPACE,
        "-o",
        "jsonpath={.status.podIP}",
    ])?;
    let (reached, _, _) = kubectl.try_run(&[
        "exec",
        "-n",
        NAMESPACE,
        "network-client",
        "--",
        "wget",
        "-T",
        "3",
        "-qO-",
        &format!("http://{server_ip}:8080"),
    ])?;
    if reached {
        bail!("default-deny NetworkPolicy unexpectedly allowed pod traffic");
    }

    kubectl.run(&[
        "delete",
        "limitrange/proofstorm-container-limits",
        "-n",
        NAMESPACE,
    ])?;
    kubectl.restart_controller()?;
    let mut restored = false;
    for _ in 0..30 {
        let (found, _, _) = kubectl.try_run(&[
            "get",
            "limitrange/proofstorm-container-limits",
            "-n",
            NAMESPACE,
        ])?;
        if found {
            restored = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !restored {
        bail!("the controller did not reconcile the deleted LimitRange");
    }

    let after_restart = kubectl.run(&[
        "get",
        &format!("proofstormlab/{LAB}"),
        "-n",
        CONTROL_NAMESPACE,
        "-o",
        "jsonpath={.status.instanceNamespace}",
    ])?;
    if after_restart != NAMESPACE {
        bail!("instance namespace changed across a controller restart: {after_restart}");
    }

    // A finalizer-blocked ConfigMap proves close waits for verified absence.
    kubectl.run(&[
        "apply",
        "-f",
        &manifest("tests/kubernetes/cleanup-blocker.yaml"),
    ])?;
    kubectl.run(&[
        "delete",
        &format!("proofstormlab/{LAB}"),
        "-n",
        CONTROL_NAMESPACE,
        "--wait=false",
    ])?;
    sleep(Duration::from_secs(5));

    let phase = kubectl.run(&[
        "get",
        &format!("proofstormlab/{LAB}"),
        "-n",
        CONTROL_NAMESPACE,
        "-o",
        "jsonpath={.status.phase}",
    ])?;
    if phase != "Closing" {
        bail!("blocked teardown did not hold the lab in Closing: {phase}");
    }
    kubectl.run(&["get", "namespace", NAMESPACE])?;

    kubectl.run(&[
        "patch",
        "configmap/cleanup-blocker",
        "-n",
        NAMESPACE,
        "--type=merge",
        "-p",
        r#"{"metadata":{"finalizers":null}}"#,
    ])?;
    kubectl.run(&[
        "wait",
        "--for=delete",
        &format!("namespace/{NAMESPACE}"),
        "--timeout=90s",
    ])?;
    kubectl.run(&[
        "wait",
        "--for=delete",
        &format!("proofstormlab/{LAB}"),
        "-n",
        CONTROL_NAMESPACE,
        "--timeout=90s",
    ])?;

    let receipt = kubectl.get_json(&[
        "get",
        "configmap/proofstorm-teardown-01slice2lab00",
        "-n",
        CONTROL_NAMESPACE,
    ])?;
    expect::equals(
        &receipt,
        "/data/verifiedAbsent",
        &serde_json::Value::from("true"),
    )?;

    println!("Slice 2 live security and lifecycle acceptance passed");
    Ok(())
}
