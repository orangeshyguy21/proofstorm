use super::{
    common::{INSTANCE, components, empty_cell, links, now_unix},
    support::CellCleanup,
};
use crate::{GateContext, McpClient, cell, gate::CONTROL_NAMESPACE, json as expect};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

pub(super) struct Materialized {
    pub namespace: String,
    pub instance_key: String,
    pub revision_digest: String,
    pub lock_digest: String,
}

pub(super) fn compose(
    client: &mut McpClient,
    cleanup: &mut CellCleanup<'_>,
    scenario: super::Scenario,
) -> Result<Materialized> {
    // --- network backend discovery is explicit and bounded ------------------
    let backend = client.call("network_capabilities", json!({}))?;
    if expect::string(&backend, "/id")? != "kubernetes-network-policy"
        || expect::string(&backend, "/version")? != "networking.k8s.io/v1"
        || backend.get("features") != Some(&json!(["partition", "heal"]))
        || backend.get("directions") != Some(&json!(["bidirectional"]))
        || backend.get("bounds")
            != Some(&json!({
                "max_delay_ms": null,
                "max_jitter_ms": null,
                "max_loss_basis_points": null
            }))
    {
        bail!("network backend discovery is not explicit and bounded: {backend}");
    }

    // Preflight the complete specification once, then inspect its immutable lock.
    // Atomic stable-ID patch/retry coverage lives in the canonical surface gate and MCP tests.
    let mut document = empty_cell();
    document["components"] = json!(components(scenario));
    document["links"] = json!(links(scenario));
    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"request_id":"preview-slice5","cell":document}),
    )?;
    let repeated = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"request_id":"preview-slice5","cell":document}),
    )?;
    if preview != repeated {
        bail!("exact preview retry changed the immutable plan");
    }
    let published = cell::review(client, &preview)?;
    for entry in expect::array(&published, "/lock/entries")? {
        if !expect::string(entry, "/image")?.contains("@sha256:") {
            bail!("published lock contains an unpinned image: {entry}");
        }
    }

    let accepted = cell::apply(client, &preview)?;
    cleanup.record(
        expect::string(&accepted, "/cell/instance_id")?,
        expect::string(&accepted, "/instance_key")?,
    );
    let status = cell::wait_phase(client, INSTANCE, "ready", 180, Duration::from_secs(3))?;
    let instance_key = expect::string(&status, "/instance_key")?.to_string();
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();
    let revision_digest = expect::string(&status, "/revision_digest")?.to_string();
    let lock_digest = expect::string(&status, "/lock_digest")?.to_string();

    let component_status = client.call(
        "cell_component_status_list",
        json!({"name": INSTANCE, "limit": 50}),
    )?;
    let mut ready: Vec<&str> = expect::array(&component_status, "/components")?
        .iter()
        .filter(|component| component.get("ready").and_then(Value::as_bool) == Some(true))
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<_>>()?;
    ready.sort_unstable();
    let mut wanted = expect::array(&document, "/components")?
        .iter()
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<Vec<_>>>()?;
    wanted.sort_unstable();
    if ready != wanted {
        bail!("cell topology is not ready: {component_status}");
    }

    Ok(Materialized {
        namespace,
        instance_key,
        revision_digest,
        lock_digest,
    })
}

pub(super) fn conformance(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    workspace: &str,
) -> Result<()> {
    let kubectl = &context.kubectl;
    let invalid_name = format!(
        "invalid-{}",
        proofstorm_core::digest_json(&workspace).trim_start_matches("sha256:")
    );
    let invalid_name = &invalid_name[..50];
    let invalid_action = invalid_name;
    // --- unsupported fault kinds are refused before any action -------------
    cell::assert_tool_absent(client, "network_delay")?;
    cell::assert_tool_absent(client, "network_loss")?;

    // --- a hand-written invalid action must fail closed with no Job --------
    let cells = kubectl.get_json(&[
        "get",
        "proofstormcells.proofstorm.dev",
        "-n",
        CONTROL_NAMESPACE,
    ])?;
    let status = cell::status(client, INSTANCE)?;
    let instance_id = expect::string(&status, "/instance_id")?;
    let cell_resource = expect::array(&cells, "/items")?
        .iter()
        .find(|item| {
            item.pointer("/spec/instanceId").and_then(Value::as_str) == Some(instance_id)
                && item.pointer("/spec/workspaceId").and_then(Value::as_str) == Some(workspace)
        })
        .ok_or_else(|| anyhow::anyhow!("no cell resource for {INSTANCE}"))?;
    let instance_key = expect::string(cell_resource, "/spec/instanceKey")?;
    let cell_name = expect::string(cell_resource, "/metadata/name")?;
    let invalid = json!({
        "apiVersion": "proofstorm.dev/v1alpha1",
        "kind": "ProofstormCellAction",
        "metadata": {
            "name": invalid_action,
            "namespace": CONTROL_NAMESPACE,
            "labels": {
                "proofstorm.dev/instance": instance_key,
                "proofstorm.dev/cell": cell_name,
                "app.kubernetes.io/managed-by": "proofstorm-controller-conformance"
            }
        },
        "spec": {
            "cellName": cell_name,
            "workspaceId": workspace,
            "instanceId": instance_id,
            "instanceKey": instance_key,
            "experimentId": "controller-conformance",
            "sessionId": "controller-conformance",
            "principalId": "cluster-operator",
            "sequence": 1,
            "operationId": "invalid-native-command",
            "requestDigest": "sha256:controller-conformance",
            "capability": "component.exec_live",
            "acceptedAtUnix": now_unix(),
            "action": {
                "kind": "component_exec_live",
                "parameters": {"component": "missing-component", "script": "true", "timeoutSeconds": 10}
            }
        }
    });
    kubectl.apply_stdin(&serde_json::to_string(&invalid)?)?;

    let mut invalid_status = Value::Null;
    let mut failed_closed = false;
    for _ in 0..30 {
        let runtime = kubectl.get_json(&[
            "get",
            "proofstormcellaction.proofstorm.dev",
            invalid_action,
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        invalid_status = runtime.get("status").cloned().unwrap_or(Value::Null);
        if invalid_status.get("phase").and_then(Value::as_str) == Some("Failed") {
            failed_closed = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !failed_closed {
        bail!("invalid native action did not fail closed: {invalid_status}");
    }
    for (field, expected) in [
        ("code", "action_prerequisite_unsatisfied"),
        ("component", "missing-component"),
        ("operation", "native_exec"),
        ("prerequisite", "accepted_identity"),
    ] {
        if invalid_status["error"][field] != expected {
            bail!("invalid native action has the wrong terminal error: {invalid_status}");
        }
    }
    let leftover = kubectl.run(&[
        "get",
        "job",
        invalid_action,
        "-n",
        namespace,
        "--ignore-not-found",
        "-o",
        "name",
    ])?;
    if !leftover.is_empty() {
        bail!("invalid native action created a runtime Job");
    }
    kubectl.run(&[
        "delete",
        "proofstormcellaction.proofstorm.dev",
        invalid_action,
        "-n",
        CONTROL_NAMESPACE,
    ])?;

    Ok(())
}
