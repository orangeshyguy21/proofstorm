//! Slice 5 end-to-end: agent-composed lab, controller conformance against a
//! hand-written invalid action, bootstrap and channels, the wallet round trip,
//! the lost-Job and cancellation fences, private invoice and pay, node
//! lifecycle, network partition and heal with reachability oracles, channel
//! rebalance, topology teardown, the action journal, evidence export, and
//! verified close.
//!
//! Ported from `tests/kubernetes/slice5_mcp_client.py`, the largest gate. The
//! statement order mirrors the Python exactly so the two can be diffed.

use std::{
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, McpClient, gate::CONTROL_NAMESPACE, json as expect, lab};

const INSTANCE: &str = "slice5-instance";
const EXPERIMENT: &str = "slice5-experiment";
const LEASE: &str = "slice5-lease";
const DRAFT: &str = "slice5";
const WORKSPACE: &str = "slice5";
const INVALID_ACTION: &str = "slice5-invalid-peer-action";
/// `ch-` plus a 64-character digest.
const HANDLE_LENGTH: usize = 67;

const CAPABILITIES: &[&str] = &[
    "catalog.read",
    "lab.read",
    "lab.create",
    "lab.edit",
    "lab.validate",
    "lab.publish",
    "lab.materialize",
    "lab.status",
    "lab.close",
    "experiment.create",
    "experiment.read",
    "experiment.close",
    "lease.acquire",
    "lease.release",
    "action.cancel",
    "topology.mutate",
    "node.control",
    "chain.mine",
    "wallet.create",
    "wallet.control",
    "wallet.fund",
    "peer.connect",
    "peer.disconnect",
    "channel.open",
    "channel.close",
    "channel.force_close",
    "channel.rebalance",
    "network.delay",
    "network.drop",
    "network.partition",
    "network.heal",
    "oracle.run",
    "artifact.read",
];

fn components() -> Vec<Value> {
    vec![
        json!({"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {"txindex": true, "fallback_fee": 0.0002}}),
        json!({"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-mint"}}),
        json!({"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-payer"}}),
        json!({"id": "attacker-cln", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "attacker", "config": {"alias": "proofstorm-attacker"}}),
        json!({"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.0", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm Slice 5", "description": "Agent-created Cashu lab"}}),
        json!({"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}}),
        json!({"id": "receiver-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}}),
    ]
}

fn links() -> Vec<Value> {
    vec![
        json!({"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}),
        json!({"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}),
        json!({"id": "attacker-cln-chain", "kind": "chain_backend", "from": "attacker-cln", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}),
        json!({"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}),
    ]
}

/// An empty draft the composer then fills one mutation at a time.
fn empty_lab() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "slice5-cashu-round-trip",
        "components": [],
        "links": [],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

/// The instance, experiment and lease triple every runtime action carries.
fn scoped(operation: &str, extra: Value) -> Value {
    let mut base = json!({
        "instance_id": INSTANCE,
        "experiment_id": EXPERIMENT,
        "lease_id": LEASE,
        "operation_id": operation
    });
    if let (Some(target), Value::Object(source)) = (base.as_object_mut(), extra) {
        for (key, value) in source {
            target.insert(key, value);
        }
    }
    base
}

/// Submit an action twice and prove the retry did not change its identity.
fn submit_idempotent(
    client: &mut McpClient,
    tool: &str,
    request: Value,
    label: &str,
) -> Result<Value> {
    let accepted = client.call(tool, request.clone())?;
    let retried = client.call(tool, request)?;
    if expect::string(&retried, "/resource_name")? != expect::string(&accepted, "/resource_name")?
        || expect::integer(&retried, "/sequence")? != expect::integer(&accepted, "/sequence")?
    {
        bail!("{label} retry changed the accepted action identity: {accepted} {retried}");
    }
    Ok(accepted)
}

fn assert_handle(content: &Value, label: &str) -> Result<String> {
    let handle = content
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !handle.starts_with("ch-") || handle.len() != HANDLE_LENGTH {
        bail!("{label} did not return an opaque channel handle: {content}");
    }
    Ok(handle.to_string())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// TCP reachability from one pod to the mint, used as the CNI ground truth
/// alongside the MCP reachability oracle.
fn pod_can_reach_mint(context: &GateContext, namespace: &str, pod: &str) -> Result<bool> {
    let (ok, _, _) = context.kubectl.try_run(&[
        "exec",
        "-n",
        namespace,
        pod,
        "--",
        "python3",
        "-c",
        "import socket; s=socket.create_connection((\"mint\",3338),3); s.close()",
    ])?;
    Ok(ok)
}

fn wait_reachability(
    context: &GateContext,
    namespace: &str,
    pod: &str,
    want: bool,
    label: &str,
) -> Result<()> {
    for _ in 0..30 {
        if pod_can_reach_mint(context, namespace, pod)? == want {
            return Ok(());
        }
        sleep(Duration::from_secs(1));
    }
    bail!("{label}");
}

fn component_pod(context: &GateContext, namespace: &str, component: &str) -> Result<String> {
    context.kubectl.run(&[
        "get",
        "pod",
        "-n",
        namespace,
        "-l",
        &format!("proofstorm.dev/component={component}"),
        "-o",
        "jsonpath={.items[0].metadata.name}",
    ])
}

/// Run one MCP reachability observation and check its sanitized artifact.
fn observe_mint_reachability(
    client: &mut McpClient,
    operation: &str,
    component: &str,
    expected: bool,
    observations: &mut Vec<String>,
) -> Result<()> {
    client.call(
        "proofstorm_reachability_oracle",
        scoped(
            operation,
            json!({
                "from_component": component,
                "to_component": "mint",
                "service": "http",
                "timeout_seconds": 2,
                "attempts": 3,
                "idempotency_key": format!("{operation}-slice5")
            }),
        ),
    )?;
    let observed = lab::wait_operation(client, operation, 120)?;
    let content = lab::artifact_content(&observed)?;
    let attempts = expect::integer(content, "/attempts")?;
    if expect::string(content, "/from_component")? != component
        || expect::string(content, "/to_component")? != "mint"
        || expect::string(content, "/service")? != "http"
        || expect::integer(content, "/port")? != 3338
        || expect::boolean(content, "/reachable")? != expected
        || !(1..=3).contains(&attempts)
        || expect::integer(content, "/timeout_seconds")? != 2
    {
        bail!("invalid MCP reachability observation: {observed}");
    }
    observations.push(operation.to_string());
    Ok(())
}

fn action_kinds(context: &GateContext) -> Result<Value> {
    context.kubectl.get_json(&[
        "get",
        "proofstormlabactions.proofstorm.dev",
        "-n",
        CONTROL_NAMESPACE,
        "-l",
        "proofstorm.dev/instance",
    ])
}

/// Map runtime action `operationId` to its typed action kind.
fn kinds_by_operation(items: &Value) -> Result<std::collections::BTreeMap<String, String>> {
    let mut map = std::collections::BTreeMap::new();
    for item in expect::array(items, "/items")? {
        map.insert(
            expect::string(item, "/spec/operationId")?.to_string(),
            expect::string(item, "/spec/action/kind")?.to_string(),
        );
    }
    Ok(map)
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session(WORKSPACE, "experiment-agent", CAPABILITIES)?;
    let kubectl = &context.kubectl;

    // --- network backend discovery is explicit and bounded ------------------
    let backend = client.call("proofstorm_network_capabilities", json!({}))?;
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

    // --- compose the lab one mutation at a time ----------------------------
    let mut draft = client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": empty_lab(), "idempotency_key": "create-slice5"}),
    )?;
    for component in components() {
        let id = expect::string(&component, "/id")?.to_string();
        let mutation = json!({
            "draft_id": DRAFT,
            "expected_version": expect::integer(&draft, "/version")?,
            "component": component,
            "idempotency_key": format!("add-component-{id}")
        });
        draft = client.call("proofstorm_component_add", mutation.clone())?;
        if id == "chain" {
            let replayed = client.call("proofstorm_component_add", mutation)?;
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
            "proofstorm_link_add",
            json!({
                "draft_id": DRAFT,
                "expected_version": expect::integer(&draft, "/version")?,
                "link": link,
                "idempotency_key": key
            }),
        )?;
    }

    let document = client.call("proofstorm_lab_read", json!({"draft_id": DRAFT}))?;
    let composed: Vec<&str> = expect::array(&document, "/lab/components")?
        .iter()
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<_>>()?;
    let mut canonical: Vec<String> = components()
        .iter()
        .map(|component| expect::string(component, "/id").map(str::to_owned))
        .collect::<Result<_>>()?;
    canonical.sort();
    if composed != canonical {
        bail!("component composer did not produce canonical ordering: {composed:?}");
    }
    let validation = client.call(
        "proofstorm_lab_validate",
        json!({"lab": document.get("lab").cloned().unwrap_or(Value::Null)}),
    )?;
    if !expect::boolean(&validation, "/valid")? {
        bail!("agent-composed draft is invalid: {validation}");
    }

    let published = client.call(
        "proofstorm_lab_publish",
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
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-slice5"}),
    )?;
    let status = lab::wait_phase(&mut client, INSTANCE, "ready", 180, Duration::from_secs(3))?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();
    let revision_digest = expect::string(&status, "/revision_digest")?.to_string();
    let lock_digest = expect::string(&status, "/lock_digest")?.to_string();

    let component_status = client.call(
        "proofstorm_lab_component_status_list",
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
        bail!("lab topology is not ready: {component_status}");
    }

    // --- unsupported fault kinds are refused before any action -------------
    client.call_refused(
        "proofstorm_network_delay",
        json!({
            "instance_id": INSTANCE, "experiment_id": "unsupported-network-experiment",
            "lease_id": "unsupported-network-lease", "operation_id": "unsupported-network-delay",
            "from_component": "wallet", "to_component": "mint", "direction": "from_to",
            "delay_ms": 100, "jitter_ms": 10, "idempotency_key": "unsupported-network-delay-slice5"
        }),
        "network_fault_unsupported",
    )?;
    client.call_refused(
        "proofstorm_network_loss",
        json!({
            "instance_id": INSTANCE, "experiment_id": "unsupported-network-experiment",
            "lease_id": "unsupported-network-lease", "operation_id": "unsupported-network-loss",
            "from_component": "wallet", "to_component": "mint", "direction": "bidirectional",
            "loss_basis_points": 250, "idempotency_key": "unsupported-network-loss-slice5"
        }),
        "network_fault_unsupported",
    )?;

    // --- a hand-written invalid action must fail closed with no Job --------
    let labs = kubectl.get_json(&[
        "get",
        "proofstormlabs.proofstorm.dev",
        "-n",
        CONTROL_NAMESPACE,
    ])?;
    let lab_resource = expect::array(&labs, "/items")?
        .iter()
        .find(|item| item.pointer("/spec/instanceId").and_then(Value::as_str) == Some(INSTANCE))
        .ok_or_else(|| anyhow::anyhow!("no lab resource for {INSTANCE}"))?;
    let instance_key = expect::string(lab_resource, "/spec/instanceKey")?;
    let lab_name = expect::string(lab_resource, "/metadata/name")?;
    let invalid = json!({
        "apiVersion": "proofstorm.dev/v1alpha1",
        "kind": "ProofstormLabAction",
        "metadata": {
            "name": INVALID_ACTION,
            "namespace": CONTROL_NAMESPACE,
            "labels": {
                "proofstorm.dev/instance": instance_key,
                "proofstorm.dev/lab": lab_name,
                "app.kubernetes.io/managed-by": "proofstorm-controller-conformance"
            }
        },
        "spec": {
            "labName": lab_name,
            "workspaceId": WORKSPACE,
            "instanceId": INSTANCE,
            "instanceKey": instance_key,
            "experimentId": "controller-conformance",
            "leaseId": "controller-conformance",
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
            "proofstormlabaction.proofstorm.dev",
            INVALID_ACTION,
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
        INVALID_ACTION,
        "-n",
        &namespace,
        "--ignore-not-found",
        "-o",
        "name",
    ])?;
    if !leftover.is_empty() {
        bail!("invalid typed action created a runtime Job");
    }
    kubectl.run(&[
        "delete",
        "proofstormlabaction.proofstorm.dev",
        INVALID_ACTION,
        "-n",
        CONTROL_NAMESPACE,
    ])?;

    // --- experiment and lease ----------------------------------------------
    client.call(
        "proofstorm_experiment_create",
        json!({"experiment_id": EXPERIMENT, "instance_id": INSTANCE, "idempotency_key": "create-slice5-experiment"}),
    )?;
    client.call(
        "proofstorm_lease_acquire",
        json!({"experiment_id": EXPERIMENT, "lease_id": LEASE, "duration_seconds": 900, "max_actions": 47, "idempotency_key": "acquire-slice5-lease"}),
    )?;
    client.call_refused(
        "proofstorm_lab_close",
        json!({"instance_id": INSTANCE}),
        "instance_leased",
    )?;

    // --- bootstrap survives a caller retry and a controller restart --------
    let bootstrap_request = scoped(
        "bootstrap",
        json!({
            "chain": "chain", "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
            "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000,
            "idempotency_key": "bootstrap-slice5"
        }),
    );
    let accepted_bootstrap =
        client.call("proofstorm_liquidity_bootstrap", bootstrap_request.clone())?;
    let mut items = Value::Null;
    let mut created = false;
    for _ in 0..30 {
        items = action_kinds(context)?;
        if !expect::array(&items, "/items")?.is_empty() {
            created = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !created {
        bail!("controller-owned ProofstormLabAction was not created");
    }
    let entries = expect::array(&items, "/items")?;
    if entries.len() != 1
        || expect::string(&entries[0], "/spec/action/kind")? != "bootstrap_liquidity"
    {
        bail!("unexpected typed runtime action: {items}");
    }
    let retried_bootstrap = client.call("proofstorm_liquidity_bootstrap", bootstrap_request)?;
    if expect::string(&retried_bootstrap, "/resource_name")?
        != expect::string(&accepted_bootstrap, "/resource_name")?
        || expect::integer(&retried_bootstrap, "/sequence")?
            != expect::integer(&accepted_bootstrap, "/sequence")?
    {
        bail!("caller retry changed the accepted action identity");
    }
    kubectl.rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    let jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        &namespace,
        "-l",
        "proofstorm.dev/action",
    ])?;
    if expect::array(&jobs, "/items")?.len() != 1 {
        bail!("caller retry or controller restart duplicated the bootstrap Job");
    }
    let bootstrap = lab::wait_operation(&mut client, "bootstrap", 120)?;
    let bootstrap_content = lab::artifact_content(&bootstrap)?;
    if !expect::boolean(bootstrap_content, "/ready")? {
        bail!("bootstrap artifact is invalid: {bootstrap}");
    }
    let bootstrap_channel_id = assert_handle(bootstrap_content, "bootstrap")?;

    // --- peer, channel, wallet ---------------------------------------------
    submit_idempotent(
        &mut client,
        "proofstorm_peer_connect",
        scoped(
            "peer-connect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "idempotency_key": "peer-connect-slice5"}),
        ),
        "peer",
    )?;
    let peer = lab::wait_operation(&mut client, "peer-connect", 120)?;
    if !expect::boolean(lab::artifact_content(&peer)?, "/connected")? {
        bail!("peer-connect artifact is invalid: {peer}");
    }

    submit_idempotent(
        &mut client,
        "proofstorm_channel_open",
        scoped(
            "channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_sat": 2_000_000, "push_sat": 0, "idempotency_key": "channel-open-slice5"}),
        ),
        "channel",
    )?;
    let channel = lab::wait_operation(&mut client, "channel-open", 120)?;
    let channel_content = lab::artifact_content(&channel)?;
    if !expect::boolean(channel_content, "/active")? {
        bail!("channel-open artifact is invalid: {channel}");
    }
    let channel_id = assert_handle(channel_content, "channel open")?;

    submit_idempotent(
        &mut client,
        "proofstorm_wallet_initialize",
        scoped(
            "wallet-initialize",
            json!({"wallet": "wallet", "mint": "mint", "idempotency_key": "wallet-initialize-slice5"}),
        ),
        "wallet-initialize",
    )?;
    let initialized = lab::wait_operation(&mut client, "wallet-initialize", 120)?;
    if !expect::boolean(lab::artifact_content(&initialized)?, "/initialized")? {
        bail!("wallet-initialize artifact is invalid: {initialized}");
    }

    client.call(
        "proofstorm_wallet_balance",
        scoped(
            "wallet-balance",
            json!({"wallet": "wallet", "mint": "mint", "idempotency_key": "wallet-balance-slice5"}),
        ),
    )?;
    let balance = lab::wait_operation(&mut client, "wallet-balance", 120)?;
    if expect::integer(lab::artifact_content(&balance)?, "/balance_sat")? != 0 {
        bail!("new wallet did not have a zero sanitized balance: {balance}");
    }

    submit_idempotent(
        &mut client,
        "proofstorm_wallet_fund",
        scoped(
            "wallet-fund",
            json!({"wallet": "wallet", "mint": "mint", "payer_lightning": "payer-lnd", "amount_sat": 1000, "idempotency_key": "wallet-fund-slice5"}),
        ),
        "wallet-fund",
    )?;
    let funded = lab::wait_operation(&mut client, "wallet-fund", 120)?;
    let fund_result = lab::artifact_content(&funded)?;
    if expect::integer(fund_result, "/funded_sat")? != 1000
        || expect::integer(fund_result, "/balance_sat")? != 1000
    {
        bail!("wallet-fund artifact is invalid: {funded}");
    }

    let accepted_wallet = submit_idempotent(
        &mut client,
        "proofstorm_wallet_round_trip",
        scoped(
            "round-trip",
            json!({"wallet": "wallet", "mint": "mint", "payer_lightning": "payer-lnd", "amount_sat": 1000, "tolerance_sat": 100, "idempotency_key": "round-trip-slice5"}),
        ),
        "wallet",
    )?;
    let wallet_resource = expect::string(&accepted_wallet, "/resource_name")?.to_string();
    let kinds = kinds_by_operation(&action_kinds(context)?)?;
    if kinds.get("round-trip").map(String::as_str) != Some("wallet_round_trip") {
        bail!("wallet request did not create a typed runtime action: {kinds:?}");
    }
    let round_trip = lab::wait_operation(&mut client, "round-trip", 120)?;
    let wallet_result = lab::artifact_content(&round_trip)?;
    if expect::boolean(wallet_result, "/inflation")? {
        bail!("round-trip artifact is invalid: {round_trip}");
    }
    let balance_after_swap = expect::integer(wallet_result, "/balance_after_swap_sat")?;
    let wallet_jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        &namespace,
        "-l",
        &format!("proofstorm.dev/action={wallet_resource}"),
    ])?;
    if expect::array(&wallet_jobs, "/items")?.len() != 1 {
        bail!("caller retry duplicated the controller-owned wallet Job");
    }

    // --- a Job deleted across controller downtime must not replay ----------
    let accepted_lost = client.call(
        "proofstorm_conservation_oracle",
        scoped(
            "lost-conservation",
            json!({"wallet": "wallet", "mint": "mint", "expected_sat": balance_after_swap, "tolerance_sat": 0, "idempotency_key": "lost-conservation-slice5"}),
        ),
    )?;
    let lost_resource = expect::string(&accepted_lost, "/resource_name")?.to_string();
    let mut fenced = false;
    for _ in 0..60 {
        let runtime = kubectl.get_json(&[
            "get",
            "proofstormlabaction.proofstorm.dev",
            &lost_resource,
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        if runtime.pointer("/status/phase").and_then(Value::as_str) == Some("Running") {
            fenced = true;
            break;
        }
        sleep(Duration::from_millis(250));
    }
    if !fenced {
        bail!("lost-Job action never recorded its execution fence");
    }
    kubectl.stop_controller()?;
    kubectl.run(&[
        "delete",
        "job",
        &lost_resource,
        "-n",
        &namespace,
        "--wait=true",
    ])?;
    kubectl.start_controller()?;
    let lost = lab::wait_operation_phase(&mut client, "lost-conservation", "failed", 120)?;
    if lab::artifact_content(&lost)?
        .get("code")
        .and_then(Value::as_str)
        != Some("action_job_lost")
    {
        bail!("lost Job did not produce the replay-safe terminal error: {lost}");
    }
    let replayed = kubectl.run(&[
        "get",
        "job",
        &lost_resource,
        "-n",
        &namespace,
        "--ignore-not-found",
        "-o",
        "name",
    ])?;
    if !replayed.is_empty() {
        bail!("controller replayed a lost action Job");
    }

    // --- cancellation recorded while the controller is down ----------------
    kubectl.stop_controller()?;
    let accepted_cancelled = client.call(
        "proofstorm_conservation_oracle",
        scoped(
            "cancelled-conservation",
            json!({"wallet": "wallet", "mint": "mint", "expected_sat": balance_after_swap, "tolerance_sat": 0, "idempotency_key": "cancelled-conservation-slice5"}),
        ),
    )?;
    let cancelled_resource = expect::string(&accepted_cancelled, "/resource_name")?.to_string();
    let cancel_request = json!({"operation_id": "cancelled-conservation", "idempotency_key": "cancel-action-slice5"});
    let first_cancel = client.call("proofstorm_action_cancel", cancel_request.clone())?;
    let retried_cancel = client.call("proofstorm_action_cancel", cancel_request)?;
    if expect::string(&first_cancel, "/resource_name")? != cancelled_resource
        || expect::string(&retried_cancel, "/resource_name")? != cancelled_resource
        || expect::integer(&retried_cancel, "/sequence")?
            != expect::integer(&accepted_cancelled, "/sequence")?
    {
        bail!("cancellation retry changed the accepted action identity");
    }
    kubectl.start_controller()?;
    let cancelled =
        lab::wait_operation_phase(&mut client, "cancelled-conservation", "cancelled", 120)?;
    if lab::artifact_content(&cancelled)?
        .get("code")
        .and_then(Value::as_str)
        != Some("action_cancelled")
    {
        bail!("cancelled action artifact is invalid: {cancelled}");
    }
    let cancelled_jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        &namespace,
        "-l",
        &format!("proofstorm.dev/action={cancelled_resource}"),
    ])?;
    if !expect::array(&cancelled_jobs, "/items")?.is_empty() {
        bail!("cancelled action created or retained a runtime Job across controller restart");
    }

    // --- conservation oracle ------------------------------------------------
    let accepted_oracle = submit_idempotent(
        &mut client,
        "proofstorm_conservation_oracle",
        scoped(
            "conservation",
            json!({"wallet": "wallet", "mint": "mint", "expected_sat": balance_after_swap, "tolerance_sat": 0, "idempotency_key": "conservation-slice5"}),
        ),
        "oracle",
    )?;
    let oracle_resource = expect::string(&accepted_oracle, "/resource_name")?.to_string();
    let kinds = kinds_by_operation(&action_kinds(context)?)?;
    if kinds.get("conservation").map(String::as_str) != Some("conservation_oracle") {
        bail!("oracle request did not create a typed runtime action: {kinds:?}");
    }
    let oracle = lab::wait_operation(&mut client, "conservation", 120)?;
    if !expect::boolean(lab::artifact_content(&oracle)?, "/conserved")? {
        bail!("oracle artifact is invalid: {oracle}");
    }
    let oracle_jobs = kubectl.get_json(&[
        "get",
        "jobs",
        "-n",
        &namespace,
        "-l",
        &format!("proofstorm.dev/action={oracle_resource}"),
    ])?;
    if expect::array(&oracle_jobs, "/items")?.len() != 1 {
        bail!("caller retry duplicated the controller-owned oracle Job");
    }

    // --- private invoice and pay -------------------------------------------
    client.call(
        "proofstorm_wallet_initialize",
        scoped(
            "receiver-initialize",
            json!({"wallet": "receiver-wallet", "mint": "mint", "idempotency_key": "receiver-initialize-slice5"}),
        ),
    )?;
    let receiver_initialized = lab::wait_operation(&mut client, "receiver-initialize", 120)?;
    if expect::integer(
        lab::artifact_content(&receiver_initialized)?,
        "/balance_sat",
    )? != 0
    {
        bail!("receiver wallet did not initialize empty: {receiver_initialized}");
    }

    client.call(
        "proofstorm_wallet_invoice",
        scoped(
            "cancelled-wallet-invoice",
            json!({"quote_id": "cancelled-receiver-quote", "wallet": "receiver-wallet", "mint": "mint", "amount_sat": 50, "timeout_seconds": 300, "idempotency_key": "cancelled-wallet-invoice-slice5"}),
        ),
    )?;
    let invoice_path = "/wallet/.proofstorm/quotes/cancelled-receiver-quote/invoice.log";
    let mut materialized = false;
    for _ in 0..120 {
        let (ok, _, _) = kubectl.try_run(&[
            "exec",
            "-n",
            &namespace,
            "deployment/receiver-wallet",
            "--",
            "test",
            "-s",
            invoice_path,
        ])?;
        if ok {
            materialized = true;
            break;
        }
        sleep(Duration::from_millis(250));
    }
    if !materialized {
        bail!("cancelled invoice never materialized its private payment request");
    }
    client.call(
        "proofstorm_action_cancel",
        json!({"operation_id": "cancelled-wallet-invoice", "idempotency_key": "cancel-wallet-invoice-slice5"}),
    )?;
    lab::wait_operation_phase(&mut client, "cancelled-wallet-invoice", "cancelled", 120)?;
    let cancelled_quote = client.call(
        "proofstorm_wallet_quote_status",
        json!({"quote_id": "cancelled-receiver-quote"}),
    )?;
    if expect::string(&cancelled_quote, "/phase")? != "cancelled"
        || cancelled_quote.get("terminal_code").and_then(Value::as_str) != Some("action_cancelled")
    {
        bail!("pre-payment invoice cancellation was not final: {cancelled_quote}");
    }
    let mut removed = false;
    for _ in 0..120 {
        let (ok, _, _) = kubectl.try_run(&[
            "exec",
            "-n",
            &namespace,
            "deployment/receiver-wallet",
            "--",
            "test",
            "!",
            "-e",
            invoice_path,
        ])?;
        if ok {
            removed = true;
            break;
        }
        sleep(Duration::from_millis(250));
    }
    if !removed {
        bail!("cancelled invoice left private payment material on the wallet volume");
    }

    submit_idempotent(
        &mut client,
        "proofstorm_wallet_invoice",
        scoped(
            "wallet-invoice",
            json!({"quote_id": "receiver-quote", "wallet": "receiver-wallet", "mint": "mint", "amount_sat": 100, "timeout_seconds": 300, "idempotency_key": "wallet-invoice-slice5"}),
        ),
        "invoice",
    )?;
    let quote = client.call(
        "proofstorm_wallet_quote_status",
        json!({"quote_id": "receiver-quote"}),
    )?;
    if expect::string(&quote, "/phase")? != "ready"
        || expect::integer(&quote, "/amount_sat")? != 100
    {
        bail!("receive quote was not ready and sanitized: {quote}");
    }

    submit_idempotent(
        &mut client,
        "proofstorm_wallet_pay",
        scoped(
            "wallet-pay",
            json!({"quote_id": "receiver-quote", "wallet": "wallet", "mint": "mint", "idempotency_key": "wallet-pay-slice5"}),
        ),
        "pay",
    )?;
    let paid = lab::wait_operation(&mut client, "wallet-pay", 120)?;
    let paid_content = lab::artifact_content(&paid)?.clone();
    if expect::string(&paid_content, "/phase")? != "paid"
        || expect::integer(&paid_content, "/amount_sat")? != 100
    {
        bail!("wallet pay artifact is invalid: {paid}");
    }
    let settled_invoice = lab::wait_operation(&mut client, "wallet-invoice", 120)?;
    let invoice_content = lab::artifact_content(&settled_invoice)?.clone();
    if expect::string(&invoice_content, "/phase")? != "settled"
        || expect::integer(&invoice_content, "/balance_sat")? != 100
    {
        bail!("wallet invoice artifact is invalid: {settled_invoice}");
    }
    let quote = client.call(
        "proofstorm_wallet_quote_status",
        json!({"quote_id": "receiver-quote"}),
    )?;
    if expect::string(&quote, "/phase")? != "settled" || quote.get("settled_at_unix").is_none() {
        bail!("receive quote did not settle: {quote}");
    }
    let quote_list = client.call(
        "proofstorm_wallet_quote_list",
        json!({"experiment_id": EXPERIMENT, "limit": 10}),
    )?;
    if expect::array(&quote_list, "/quotes")? != &vec![cancelled_quote.clone(), quote.clone()] {
        bail!("quote list is not canonical: {quote_list}");
    }
    let serialized_flow = serde_json::to_string(
        &json!({"quote": quote, "pay": paid_content, "invoice": invoice_content}),
    )?
    .to_lowercase();
    for forbidden in ["lnbcrt", "payment_request", "adapter_quote", "mnemonic"] {
        if serialized_flow.contains(forbidden) {
            bail!("private payment material crossed MCP in quote flow: {forbidden}");
        }
    }

    // --- node lifecycle -----------------------------------------------------
    let node = json!({"component": "payer-lnd"});
    let node_scoped = |operation: &str, key: &str| -> Value {
        let mut extra = node.clone();
        if let Some(target) = extra.as_object_mut() {
            target.insert("idempotency_key".into(), Value::from(key));
        }
        scoped(operation, extra)
    };

    submit_idempotent(
        &mut client,
        "proofstorm_node_stop",
        node_scoped("payer-stop", "payer-stop-slice5"),
        "node stop",
    )?;
    let stopped = lab::wait_operation(&mut client, "payer-stop", 120)?;
    if expect::string(lab::artifact_content(&stopped)?, "/state")? != "stopped" {
        bail!("node stop artifact is invalid: {stopped}");
    }
    let stateful = kubectl.get_json(&["get", "statefulset/payer-lnd", "-n", &namespace])?;
    if expect::integer(&stateful, "/spec/replicas")? != 0 {
        bail!("stopped Lightning node did not retain zero desired replicas");
    }
    let mut degraded_ok = false;
    for _ in 0..60 {
        let stopped_lab = client.call("proofstorm_lab_status", json!({"instance_id": INSTANCE}))?;
        let stopped_components = client.call(
            "proofstorm_lab_component_status_list",
            json!({"instance_id": INSTANCE, "limit": 50}),
        )?;
        let payer = expect::array(&stopped_components, "/components")?
            .iter()
            .find(|component| component.get("id").and_then(Value::as_str) == Some("payer-lnd"))
            .ok_or_else(|| anyhow::anyhow!("payer-lnd is missing from component status"))?;
        if expect::string(&stopped_lab, "/phase")? == "ready" && !expect::boolean(payer, "/ready")?
        {
            degraded_ok = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !degraded_ok {
        bail!("intentionally stopped node corrupted lab readiness");
    }

    client.call(
        "proofstorm_node_start",
        node_scoped("payer-start", "payer-start-slice5"),
    )?;
    let started = lab::wait_operation(&mut client, "payer-start", 120)?;
    if expect::string(lab::artifact_content(&started)?, "/state")? != "running" {
        bail!("node start artifact is invalid: {started}");
    }
    let pod_before = kubectl.run(&[
        "get",
        "pod/payer-lnd-0",
        "-n",
        &namespace,
        "-o",
        "jsonpath={.metadata.uid}",
    ])?;
    client.call(
        "proofstorm_node_restart",
        node_scoped("payer-restart", "payer-restart-slice5"),
    )?;
    let restarted = lab::wait_operation(&mut client, "payer-restart", 120)?;
    if !expect::boolean(lab::artifact_content(&restarted)?, "/restarted")? {
        bail!("node restart artifact is invalid: {restarted}");
    }
    let pod_after = kubectl.run(&[
        "get",
        "pod/payer-lnd-0",
        "-n",
        &namespace,
        "-o",
        "jsonpath={.metadata.uid}",
    ])?;
    if pod_after == pod_before {
        bail!("node restart completed without replacing the component pod");
    }

    // --- network partition, restart recovery, selective heal ---------------
    let wallet_pod = component_pod(context, &namespace, "wallet")?;
    let receiver_pod = component_pod(context, &namespace, "receiver-wallet")?;
    let mut observations: Vec<String> = Vec::new();

    if !pod_can_reach_mint(context, &namespace, &wallet_pod)?
        || !pod_can_reach_mint(context, &namespace, &receiver_pod)?
    {
        bail!("wallets could not reach mint before the requested partitions");
    }
    observe_mint_reachability(
        &mut client,
        "reachability-baseline",
        "wallet",
        true,
        &mut observations,
    )?;

    submit_idempotent(
        &mut client,
        "proofstorm_network_partition",
        scoped(
            "wallet-mint-partition",
            json!({"from_component": "wallet", "to_component": "mint", "idempotency_key": "wallet-mint-partition-slice5"}),
        ),
        "network partition",
    )?;
    let partitioned = lab::wait_operation(&mut client, "wallet-mint-partition", 120)?;
    let partition_content = lab::artifact_content(&partitioned)?;
    if !expect::boolean(partition_content, "/partitioned")?
        || expect::string(partition_content, "/from_component")? != "wallet"
        || expect::string(partition_content, "/to_component")? != "mint"
        || expect::integer(partition_content, "/active_partition_count")? != 1
    {
        bail!("network partition artifact is invalid: {partitioned}");
    }
    wait_reachability(
        context,
        &namespace,
        &wallet_pod,
        false,
        "CNI continued to pass wallet-to-mint traffic after partition",
    )?;
    if !pod_can_reach_mint(context, &namespace, &receiver_pod)? {
        bail!("wallet-to-mint partition also blocked the independent receiver wallet");
    }
    observe_mint_reachability(
        &mut client,
        "reachability-wallet-blocked",
        "wallet",
        false,
        &mut observations,
    )?;
    observe_mint_reachability(
        &mut client,
        "reachability-receiver-open",
        "receiver-wallet",
        true,
        &mut observations,
    )?;

    client.call(
        "proofstorm_network_partition",
        scoped(
            "receiver-wallet-mint-partition",
            json!({"from_component": "receiver-wallet", "to_component": "mint", "idempotency_key": "receiver-wallet-mint-partition-slice5"}),
        ),
    )?;
    let receiver_partitioned =
        lab::wait_operation(&mut client, "receiver-wallet-mint-partition", 120)?;
    let receiver_content = lab::artifact_content(&receiver_partitioned)?;
    if !expect::boolean(receiver_content, "/partitioned")?
        || expect::string(receiver_content, "/from_component")? != "receiver-wallet"
        || expect::string(receiver_content, "/to_component")? != "mint"
        || expect::integer(receiver_content, "/active_partition_count")? != 2
    {
        bail!("overlapping network partition artifact is invalid: {receiver_partitioned}");
    }
    wait_reachability(
        context,
        &namespace,
        &receiver_pod,
        false,
        "CNI continued to pass receiver-wallet-to-mint traffic after partition",
    )?;
    observe_mint_reachability(
        &mut client,
        "reachability-receiver-blocked",
        "receiver-wallet",
        false,
        &mut observations,
    )?;

    let controller_before = kubectl.controller_pod_uid()?;
    kubectl.stop_controller()?;
    kubectl.run(&[
        "delete",
        "networkpolicy",
        "default-deny-all",
        "wallet",
        "receiver-wallet",
        "mint",
        "-n",
        &namespace,
        "--wait=true",
    ])?;
    let mut restored = false;
    for _ in 0..30 {
        if pod_can_reach_mint(context, &namespace, &wallet_pod)?
            && pod_can_reach_mint(context, &namespace, &receiver_pod)?
        {
            restored = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !restored {
        bail!("removing fault policies while proofstormd was stopped did not restore traffic");
    }
    kubectl.start_controller()?;
    if kubectl.controller_pod_uid()? == controller_before {
        bail!("network-fault persistence check did not replace the proofstormd pod");
    }
    let mut reconstructed = false;
    for _ in 0..30 {
        if !pod_can_reach_mint(context, &namespace, &wallet_pod)?
            && !pod_can_reach_mint(context, &namespace, &receiver_pod)?
        {
            reconstructed = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !reconstructed {
        bail!("proofstormd restart did not reconstruct both active partitions");
    }
    observe_mint_reachability(
        &mut client,
        "reachability-wallet-reconstructed",
        "wallet",
        false,
        &mut observations,
    )?;
    observe_mint_reachability(
        &mut client,
        "reachability-receiver-reconstructed",
        "receiver-wallet",
        false,
        &mut observations,
    )?;

    client.call(
        "proofstorm_network_heal",
        scoped(
            "wallet-mint-heal",
            json!({"partition_operation_id": "wallet-mint-partition", "idempotency_key": "wallet-mint-heal-slice5"}),
        ),
    )?;
    let healed = lab::wait_operation(&mut client, "wallet-mint-heal", 120)?;
    let heal_content = lab::artifact_content(&healed)?;
    if !expect::boolean(heal_content, "/healed")?
        || expect::string(heal_content, "/partition_operation_id")? != "wallet-mint-partition"
        || expect::integer(heal_content, "/active_partition_count")? != 1
    {
        bail!("network heal artifact is invalid: {healed}");
    }
    wait_reachability(
        context,
        &namespace,
        &wallet_pod,
        true,
        "wallet-to-mint traffic did not recover after heal",
    )?;
    if pod_can_reach_mint(context, &namespace, &receiver_pod)? {
        bail!("healing one partition also healed the overlapping receiver partition");
    }
    observe_mint_reachability(
        &mut client,
        "reachability-wallet-healed",
        "wallet",
        true,
        &mut observations,
    )?;
    observe_mint_reachability(
        &mut client,
        "reachability-receiver-still-blocked",
        "receiver-wallet",
        false,
        &mut observations,
    )?;

    client.call(
        "proofstorm_network_heal",
        scoped(
            "receiver-wallet-mint-heal",
            json!({"partition_operation_id": "receiver-wallet-mint-partition", "idempotency_key": "receiver-wallet-mint-heal-slice5"}),
        ),
    )?;
    let receiver_healed = lab::wait_operation(&mut client, "receiver-wallet-mint-heal", 120)?;
    let receiver_heal_content = lab::artifact_content(&receiver_healed)?;
    if !expect::boolean(receiver_heal_content, "/healed")?
        || expect::string(receiver_heal_content, "/partition_operation_id")?
            != "receiver-wallet-mint-partition"
        || expect::integer(receiver_heal_content, "/active_partition_count")? != 0
    {
        bail!("overlapping network heal artifact is invalid: {receiver_healed}");
    }
    let mut both_back = false;
    for _ in 0..30 {
        if pod_can_reach_mint(context, &namespace, &wallet_pod)?
            && pod_can_reach_mint(context, &namespace, &receiver_pod)?
        {
            both_back = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !both_back {
        bail!("receiver-wallet-to-mint traffic did not recover after its targeted heal");
    }
    observe_mint_reachability(
        &mut client,
        "reachability-receiver-healed",
        "receiver-wallet",
        true,
        &mut observations,
    )?;

    // --- CLN interoperability and rebalance --------------------------------
    client.call(
        "proofstorm_peer_connect",
        scoped(
            "cln-peer-connect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "idempotency_key": "cln-peer-connect-slice5"}),
        ),
    )?;
    let cln_peer = lab::wait_operation(&mut client, "cln-peer-connect", 120)?;
    if !expect::boolean(lab::artifact_content(&cln_peer)?, "/connected")? {
        bail!("CLN to LND peer connection artifact is invalid: {cln_peer}");
    }

    client.call(
        "proofstorm_channel_open",
        scoped(
            "cln-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 300_000, "idempotency_key": "cln-channel-open-slice5"}),
        ),
    )?;
    let cln_channel = lab::wait_operation(&mut client, "cln-channel-open", 120)?;
    let cln_channel_id = assert_handle(lab::artifact_content(&cln_channel)?, "LND to CLN channel")?;

    client.call(
        "proofstorm_peer_connect",
        scoped(
            "rebalance-bridge-peer-connect",
            json!({"from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "idempotency_key": "rebalance-bridge-peer-connect-slice5"}),
        ),
    )?;
    let bridge_peer = lab::wait_operation(&mut client, "rebalance-bridge-peer-connect", 120)?;
    if !expect::boolean(lab::artifact_content(&bridge_peer)?, "/connected")? {
        bail!("rebalance bridge peer artifact is invalid: {bridge_peer}");
    }

    client.call(
        "proofstorm_channel_open",
        scoped(
            "rebalance-bridge-channel-open",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 0, "idempotency_key": "rebalance-bridge-channel-open-slice5"}),
        ),
    )?;
    let bridge_channel = lab::wait_operation(&mut client, "rebalance-bridge-channel-open", 120)?;
    let bridge_channel_id =
        assert_handle(lab::artifact_content(&bridge_channel)?, "rebalance bridge")?;

    submit_idempotent(
        &mut client,
        "proofstorm_channel_rebalance",
        scoped(
            "channel-rebalance",
            json!({"lightning": "mint-lnd", "outgoing_channel_id": channel_id, "incoming_channel_id": cln_channel_id, "amount_sat": 100_000, "max_fee_sat": 100, "idempotency_key": "channel-rebalance-slice5"}),
        ),
        "channel rebalance",
    )?;
    let rebalanced = lab::wait_operation(&mut client, "channel-rebalance", 120)?;
    let rebalance_content = lab::artifact_content(&rebalanced)?;
    if !expect::boolean(rebalance_content, "/rebalanced")?
        || expect::integer(rebalance_content, "/amount_sat")? != 100_000
        || expect::integer(rebalance_content, "/fee_sat")? > 100
        || expect::string(rebalance_content, "/outgoing_channel_id")? != channel_id
        || expect::string(rebalance_content, "/incoming_channel_id")? != cln_channel_id
        || expect::integer(rebalance_content, "/outgoing_local_before_sat")?
            <= expect::integer(rebalance_content, "/outgoing_local_after_sat")?
        || expect::integer(rebalance_content, "/incoming_local_before_sat")?
            >= expect::integer(rebalance_content, "/incoming_local_after_sat")?
    {
        bail!("channel rebalance artifact is invalid: {rebalanced}");
    }

    // --- topology teardown --------------------------------------------------
    client.call(
        "proofstorm_channel_close",
        scoped(
            "rebalance-bridge-channel-close",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "channel_id": bridge_channel_id, "idempotency_key": "rebalance-bridge-channel-close-slice5"}),
        ),
    )?;
    let bridge_closed = lab::wait_operation(&mut client, "rebalance-bridge-channel-close", 120)?;
    if expect::string(lab::artifact_content(&bridge_closed)?, "/channel_id")? != bridge_channel_id {
        bail!("rebalance bridge close artifact is invalid: {bridge_closed}");
    }

    submit_idempotent(
        &mut client,
        "proofstorm_channel_close",
        scoped(
            "channel-close",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_id": channel_id, "idempotency_key": "channel-close-slice5"}),
        ),
        "channel close",
    )?;
    let closed = lab::wait_operation(&mut client, "channel-close", 120)?;
    let closed_content = lab::artifact_content(&closed)?;
    if !expect::boolean(closed_content, "/closed")?
        || !expect::boolean(closed_content, "/confirmed")?
        || expect::boolean(closed_content, "/force")?
        || expect::boolean(closed_content, "/pending_resolution")?
        || expect::string(closed_content, "/channel_id")? != channel_id
    {
        bail!("cooperative channel close artifact is invalid: {closed}");
    }

    client.call(
        "proofstorm_channel_close",
        scoped(
            "bootstrap-channel-close",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "mint-lnd", "channel_id": bootstrap_channel_id, "idempotency_key": "bootstrap-channel-close-slice5"}),
        ),
    )?;
    let bootstrap_closed = lab::wait_operation(&mut client, "bootstrap-channel-close", 120)?;
    if expect::string(lab::artifact_content(&bootstrap_closed)?, "/channel_id")?
        != bootstrap_channel_id
    {
        bail!("bootstrap channel close artifact is invalid: {bootstrap_closed}");
    }

    submit_idempotent(
        &mut client,
        "proofstorm_peer_disconnect",
        scoped(
            "peer-disconnect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "idempotency_key": "peer-disconnect-slice5"}),
        ),
        "peer disconnect",
    )?;
    let disconnected = lab::wait_operation(&mut client, "peer-disconnect", 120)?;
    if !expect::boolean(lab::artifact_content(&disconnected)?, "/disconnected")? {
        bail!("peer disconnect artifact is invalid: {disconnected}");
    }

    client.call(
        "proofstorm_peer_connect",
        scoped(
            "peer-reconnect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "idempotency_key": "peer-reconnect-slice5"}),
        ),
    )?;
    let reconnected = lab::wait_operation(&mut client, "peer-reconnect", 120)?;
    if !expect::boolean(lab::artifact_content(&reconnected)?, "/connected")? {
        bail!("peer reconnect artifact is invalid: {reconnected}");
    }

    client.call(
        "proofstorm_channel_open",
        scoped(
            "force-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_sat": 1_000_000, "push_sat": 0, "idempotency_key": "force-channel-open-slice5"}),
        ),
    )?;
    let force_channel = lab::wait_operation(&mut client, "force-channel-open", 120)?;
    let force_channel_id =
        assert_handle(lab::artifact_content(&force_channel)?, "force-close target")?;

    client.call(
        "proofstorm_channel_force_close",
        scoped(
            "channel-force-close",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_id": force_channel_id, "idempotency_key": "channel-force-close-slice5"}),
        ),
    )?;
    let force_closed = lab::wait_operation(&mut client, "channel-force-close", 120)?;
    let force_content = lab::artifact_content(&force_closed)?;
    if !expect::boolean(force_content, "/closed")?
        || !expect::boolean(force_content, "/confirmed")?
        || !expect::boolean(force_content, "/force")?
        || !expect::boolean(force_content, "/pending_resolution")?
        || expect::string(force_content, "/channel_id")? != force_channel_id
    {
        bail!("force channel close artifact is invalid: {force_closed}");
    }

    client.call(
        "proofstorm_channel_close",
        scoped(
            "cln-channel-close",
            json!({"chain": "chain", "from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "channel_id": cln_channel_id, "idempotency_key": "cln-channel-close-slice5"}),
        ),
    )?;
    let cln_closed = lab::wait_operation(&mut client, "cln-channel-close", 120)?;
    let cln_closed_content = lab::artifact_content(&cln_closed)?;
    if !expect::boolean(cln_closed_content, "/closed")?
        || !expect::boolean(cln_closed_content, "/confirmed")?
        || expect::boolean(cln_closed_content, "/force")?
        || expect::boolean(cln_closed_content, "/pending_resolution")?
        || expect::string(cln_closed_content, "/channel_id")? != cln_channel_id
    {
        bail!("CLN cooperative close artifact is invalid: {cln_closed}");
    }

    client.call(
        "proofstorm_peer_disconnect",
        scoped(
            "cln-peer-disconnect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "idempotency_key": "cln-peer-disconnect-slice5"}),
        ),
    )?;
    let cln_disconnected = lab::wait_operation(&mut client, "cln-peer-disconnect", 120)?;
    if !expect::boolean(lab::artifact_content(&cln_disconnected)?, "/disconnected")? {
        bail!("CLN to LND disconnect artifact is invalid: {cln_disconnected}");
    }

    client.call(
        "proofstorm_peer_connect",
        scoped(
            "cln-peer-reconnect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "idempotency_key": "cln-peer-reconnect-slice5"}),
        ),
    )?;
    let cln_reconnected = lab::wait_operation(&mut client, "cln-peer-reconnect", 120)?;
    if !expect::boolean(lab::artifact_content(&cln_reconnected)?, "/connected")? {
        bail!("CLN to LND reconnect artifact is invalid: {cln_reconnected}");
    }

    client.call(
        "proofstorm_channel_open",
        scoped(
            "cln-force-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 300_000, "idempotency_key": "cln-force-channel-open-slice5"}),
        ),
    )?;
    let cln_force_channel = lab::wait_operation(&mut client, "cln-force-channel-open", 120)?;
    let cln_force_channel_id = assert_handle(
        lab::artifact_content(&cln_force_channel)?,
        "CLN force-close target",
    )?;

    client.call(
        "proofstorm_channel_force_close",
        scoped(
            "cln-channel-force-close",
            json!({"chain": "chain", "from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "channel_id": cln_force_channel_id, "idempotency_key": "cln-channel-force-close-slice5"}),
        ),
    )?;
    let cln_force_closed = lab::wait_operation(&mut client, "cln-channel-force-close", 120)?;
    let cln_force_content = lab::artifact_content(&cln_force_closed)?;
    if !expect::boolean(cln_force_content, "/closed")?
        || !expect::boolean(cln_force_content, "/confirmed")?
        || !expect::boolean(cln_force_content, "/force")?
        || !expect::boolean(cln_force_content, "/pending_resolution")?
        || expect::string(cln_force_content, "/channel_id")? != cln_force_channel_id
    {
        bail!("CLN force-close artifact is invalid: {cln_force_closed}");
    }

    // --- the first request past the lease budget is refused ----------------
    client.call_refused(
        "proofstorm_wallet_balance",
        scoped(
            "over-budget",
            json!({"wallet": "wallet", "mint": "mint", "idempotency_key": "over-budget-slice5"}),
        ),
        "action_budget_exceeded",
    )?;

    let runtime_items = action_kinds(context)?;
    if expect::array(&runtime_items, "/items")?.iter().any(|item| {
        item.pointer("/spec/operationId").and_then(Value::as_str) == Some("over-budget")
    }) {
        bail!("exhausted action budget created a runtime action");
    }
    let kinds = kinds_by_operation(&runtime_items)?;
    let expect_kinds = |operations: &[&str], wanted: &[&str], label: &str| -> Result<()> {
        let actual: Vec<&str> = operations
            .iter()
            .map(|operation| kinds.get(*operation).map_or("", String::as_str))
            .collect();
        if actual != wanted {
            bail!("{label} did not use typed runtime actions: {actual:?}");
        }
        Ok(())
    };
    expect_kinds(
        &["wallet-invoice", "wallet-pay"],
        &["wallet_invoice", "wallet_pay"],
        "quote flow",
    )?;
    expect_kinds(
        &["payer-stop", "payer-start", "payer-restart"],
        &["node_stop", "node_start", "node_restart"],
        "node lifecycle",
    )?;
    expect_kinds(
        &[
            "channel-close",
            "bootstrap-channel-close",
            "peer-disconnect",
            "peer-reconnect",
            "force-channel-open",
            "channel-force-close",
        ],
        &[
            "channel_close",
            "channel_close",
            "peer_disconnect",
            "peer_connect",
            "channel_open",
            "channel_force_close",
        ],
        "topology teardown",
    )?;
    expect_kinds(
        &[
            "cln-peer-connect",
            "cln-channel-open",
            "cln-channel-close",
            "cln-peer-disconnect",
            "cln-peer-reconnect",
            "cln-force-channel-open",
            "cln-channel-force-close",
        ],
        &[
            "peer_connect",
            "channel_open",
            "channel_close",
            "peer_disconnect",
            "peer_connect",
            "channel_open",
            "channel_force_close",
        ],
        "CLN interoperability",
    )?;
    expect_kinds(
        &[
            "rebalance-bridge-peer-connect",
            "rebalance-bridge-channel-open",
            "channel-rebalance",
            "rebalance-bridge-channel-close",
        ],
        &[
            "peer_connect",
            "channel_open",
            "channel_rebalance",
            "channel_close",
        ],
        "rebalance",
    )?;
    expect_kinds(
        &[
            "wallet-mint-partition",
            "receiver-wallet-mint-partition",
            "wallet-mint-heal",
            "receiver-wallet-mint-heal",
        ],
        &[
            "network_partition",
            "network_partition",
            "network_heal",
            "network_heal",
        ],
        "network faults",
    )?;
    if observations.iter().any(|observation| {
        kinds.get(observation).map(String::as_str) != Some("reachability_oracle")
    }) {
        bail!("reachability observations did not use typed runtime actions: {kinds:?}");
    }

    // --- journal, evidence, verified close ---------------------------------
    let journal_page = client.call(
        "proofstorm_action_list",
        json!({"experiment_id": EXPERIMENT, "after_sequence": 0, "limit": 100}),
    )?;
    let journal = expect::array(&journal_page, "/actions")?;
    let sequences: Vec<u64> = journal
        .iter()
        .map(|action| expect::integer(action, "/sequence"))
        .collect::<Result<_>>()?;
    if sequences != (1..=47).collect::<Vec<u64>>() {
        bail!("action journal is not canonical and ordered: {sequences:?}");
    }
    for (index, action) in journal.iter().enumerate() {
        let phase = expect::string(action, "/phase")?;
        let wanted = match index {
            7 => "failed",
            8 | 11 => "cancelled",
            _ => "succeeded",
        };
        if phase != wanted {
            bail!("failure, cancellation, and success states are not ordered at {index}: {action}");
        }
    }

    client.call(
        "proofstorm_lease_release",
        json!({"lease_id": LEASE, "idempotency_key": "release-slice5-lease"}),
    )?;
    let closed_experiment = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id": EXPERIMENT, "idempotency_key": "close-slice5-experiment"}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    let evidence = client.call(
        "proofstorm_artifact_export",
        json!({
            "experiment_id": EXPERIMENT,
            "include_oracle_artifacts": true,
            "artifact_operation_ids": ["wallet-pay"],
            "include_content": true
        }),
    )?;
    let byte_length = expect::integer(&evidence, "/byte_length")?;
    let evidence_sequences: Vec<u64> = expect::array(&evidence, "/content/journal")?
        .iter()
        .map(|action| expect::integer(action, "/sequence"))
        .collect::<Result<_>>()?;
    if expect::string(&evidence, "/media_type")?
        != "application/vnd.proofstorm.evidence.v1alpha1+json"
        || !expect::string(&evidence, "/digest")?.starts_with("sha256:")
        || byte_length == 0
        || byte_length > 512 * 1024
        || expect::string(&evidence, "/content/api_version")? != "proofstorm/evidence/v1alpha1"
        || expect::string(&evidence, "/content/instance/revision_digest")? != revision_digest
        || expect::string(&evidence, "/content/instance/lock_digest")? != lock_digest
        || evidence_sequences != (1..=47).collect::<Vec<u64>>()
    {
        bail!("evidence bundle identity or journal is invalid: {evidence}");
    }
    let mut expected_artifacts: Vec<String> = vec![
        "lost-conservation".to_string(),
        "cancelled-conservation".to_string(),
        "conservation".to_string(),
        "wallet-pay".to_string(),
    ];
    expected_artifacts.extend(observations.iter().cloned());
    expected_artifacts.sort();
    let mut actual_artifacts: Vec<String> = expect::array(&evidence, "/content/artifacts")?
        .iter()
        .map(|artifact| expect::string(artifact, "/operation_id").map(str::to_owned))
        .collect::<Result<_>>()?;
    actual_artifacts.sort();
    if actual_artifacts != expected_artifacts {
        bail!(
            "evidence bundle did not contain exactly the selected and oracle artifacts: {actual_artifacts:?}"
        );
    }
    let serialized_evidence = serde_json::to_string(&evidence)?.to_lowercase();
    for forbidden in [
        "resource_name",
        "instance_key",
        "lnbcrt",
        "payment_request",
        "adapter_quote",
        "mnemonic",
    ] {
        if serialized_evidence.contains(forbidden) {
            bail!("private or runtime-only material crossed evidence export: {forbidden}");
        }
    }

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    let final_status =
        lab::wait_phase(&mut client, INSTANCE, "closed", 90, Duration::from_secs(3))?;
    if !expect::boolean(&final_status, "/teardown_receipt/verified_absent")? {
        bail!("invalid teardown receipt: {final_status}");
    }

    println!(
        "MCP composer, recovery, private invoice/pay, node lifecycle, network partition/heal, channel rebalance, topology teardown, oracle, evidence export, and verified close acceptance passed"
    );
    Ok(())
}
