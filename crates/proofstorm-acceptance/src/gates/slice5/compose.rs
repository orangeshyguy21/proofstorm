use super::{
    common::{DRAFT, INSTANCE, components, empty_cell, links, now_unix},
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

    // --- compose the cell one mutation at a time ----------------------------
    let mut draft = client.call(
        "cell_create",
        json!({"draft_id": DRAFT, "cell": empty_cell(), "idempotency_key": "create-slice5"}),
    )?;
    for component in components(scenario) {
        let id = expect::string(&component, "/id")?.to_string();
        let mutation = json!({
            "draft_id": DRAFT,
            "expected_version": expect::integer(&draft, "/version")?,
            "component": component,
            "idempotency_key": format!("add-component-{id}")
        });
        draft = client.call("component_add", mutation.clone())?;
        if id == "chain" {
            let replayed = client.call("component_add", mutation)?;
            if replayed != draft {
                bail!("component mutation replay was not idempotent");
            }
        }
    }
    for link in links() {
        let key = format!(
            "add-link-{}-{}-{}",
            expect::string(&link, "/kind")?,
            expect::string(&link, "/from")?,
            expect::string(&link, "/to")?
        );
        draft = client.call(
            "link_add",
            json!({
                "draft_id": DRAFT,
                "expected_version": expect::integer(&draft, "/version")?,
                "link": link,
                "idempotency_key": key
            }),
        )?;
    }

    let document = client.call("cell_read", json!({"draft_id": DRAFT}))?;
    let composed: Vec<&str> = expect::array(&document, "/cell/components")?
        .iter()
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<_>>()?;
    let mut canonical: Vec<String> = components(scenario)
        .iter()
        .map(|component| expect::string(component, "/id").map(str::to_owned))
        .collect::<Result<_>>()?;
    canonical.sort();
    if composed != canonical {
        bail!("component composer did not produce canonical ordering: {composed:?}");
    }
    let validation = client.call(
        "cell_validate",
        json!({"cell": document.get("cell").cloned().unwrap_or(Value::Null)}),
    )?;
    if !expect::boolean(&validation, "/valid")? {
        bail!("agent-composed draft is invalid: {validation}");
    }

    let published = client.call(
        "cell_publish",
        json!({
            "draft_id": DRAFT,
            "expected_version": expect::integer(&draft, "/version")?,
            "idempotency_key": "publish-slice5",
            "include_revision": true
        }),
    )?;
    for entry in expect::array(&published, "/lock/entries")? {
        if !expect::string(entry, "/image")?.contains("@sha256:") {
            bail!("published lock contains an unpinned image: {entry}");
        }
    }

    client.call(
        "cell_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-slice5"}),
    )?;
    let status = cell::wait_phase(client, INSTANCE, "ready", 180, Duration::from_secs(3))?;
    let instance_key = expect::string(&status, "/instance_key")?.to_string();
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();
    let revision_digest = expect::string(&status, "/revision_digest")?.to_string();
    cleanup.record(&instance_key);
    let lock_digest = expect::string(&status, "/lock_digest")?.to_string();

    let component_status = client.call(
        "cell_component_status_list",
        json!({"instance_id": INSTANCE, "limit": 50}),
    )?;
    let mut ready: Vec<&str> = expect::array(&component_status, "/components")?
        .iter()
        .filter(|component| component.get("ready").and_then(Value::as_bool) == Some(true))
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<_>>()?;
    ready.sort_unstable();
    if ready
        != [
            "attacker-cln",
            "chain",
            "mint",
            "mint-lnd",
            "payer-lnd",
            "receiver-wallet",
            "wallet",
        ]
    {
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
    client.call_refused(
        "network_delay",
        json!({
            "instance_id": INSTANCE, "experiment_id": "unsupported-network-experiment",
            "session_id": "unsupported-network-session", "operation_id": "unsupported-network-delay",
            "from_component": "wallet", "to_component": "mint", "direction": "from_to",
            "delay_ms": 100, "jitter_ms": 10, "idempotency_key": "unsupported-network-delay-slice5"
        }),
        "network_fault_unsupported",
    )?;
    client.call_refused(
        "network_loss",
        json!({
            "instance_id": INSTANCE, "experiment_id": "unsupported-network-experiment",
            "session_id": "unsupported-network-session", "operation_id": "unsupported-network-loss",
            "from_component": "wallet", "to_component": "mint", "direction": "bidirectional",
            "loss_basis_points": 250, "idempotency_key": "unsupported-network-loss-slice5"
        }),
        "network_fault_unsupported",
    )?;

    // --- a hand-written invalid action must fail closed with no Job --------
    let cells = kubectl.get_json(&[
        "get",
        "proofstormcells.proofstorm.dev",
        "-n",
        CONTROL_NAMESPACE,
    ])?;
    let cell_resource = expect::array(&cells, "/items")?
        .iter()
        .find(|item| {
            item.pointer("/spec/instanceId").and_then(Value::as_str) == Some(INSTANCE)
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
            "instanceId": INSTANCE,
            "instanceKey": instance_key,
            "experimentId": "controller-conformance",
            "sessionId": "controller-conformance",
            "principalId": "cluster-operator",
            "sequence": 1,
            "operationId": "invalid-peer-connect",
            "requestDigest": "sha256:controller-conformance",
            "capability": "peer.connect",
            "acceptedAtUnix": now_unix(),
            "action": {
                "kind": "peer_connect",
                "parameters": {"fromLightning": "mint-lnd", "toLightning": "mint-lnd"}
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
        bail!("invalid typed action did not fail closed: {invalid_status}");
    }
    if invalid_status
        .pointer("/error/code")
        .and_then(Value::as_str)
        != Some("invalid_action")
    {
        bail!("invalid typed action has the wrong terminal error: {invalid_status}");
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
        bail!("invalid typed action created a runtime Job");
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
