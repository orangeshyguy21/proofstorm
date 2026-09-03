//! Native component execution: six digest-pinned native commands across two
//! independently selectable Bitcoin nodes, action idempotency, the controller's
//! isolation contract, an ordered journal, evidence export, and verified close.
//!
//! Ported from `tests/kubernetes/native_exec_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, gate::CONTROL_NAMESPACE, json as expect};

/// Native exec is a separate, secret-bearing authority, so its principal is
/// granted `component.exec` and nothing from the typed runtime surface.
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
    "component.exec",
    "artifact.read",
];

const BITCOIN_RPC: &str = concat!(
    "bitcoin-cli -regtest -rpcconnect=\"$BITCOIN_RPC_HOST\" ",
    "-rpcport=\"$BITCOIN_RPC_PORT\" ",
    "-rpcuser=\"$BITCOIN_RPC_USER\" -rpcpassword=\"$BITCOIN_RPC_PASSWORD\" ",
    "-rpcwait -rpcwaittimeout=20 getblockchaininfo"
);

/// `(operation, component, target_component, script, expected output fragments)`
fn commands() -> Vec<(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Vec<&'static str>,
)> {
    vec![
        (
            "bitcoin-help",
            "chain",
            "chain",
            "bitcoin-cli --help",
            vec!["bitcoin-cli"],
        ),
        (
            "bitcoin-rpc",
            "chain",
            "chain",
            BITCOIN_RPC,
            vec!["\"chain\"", "\"regtest\""],
        ),
        (
            "bitcoin-rpc-chain-b",
            "chain",
            "chain-b",
            BITCOIN_RPC,
            vec!["\"chain\"", "\"regtest\""],
        ),
        (
            "lnd-help",
            "lightning",
            "lightning",
            "lncli --help",
            vec!["lncli"],
        ),
        (
            "wallet-help",
            "wallet",
            "wallet",
            "cd /app && python3 -c 'from cashu.wallet.cli.cli import cli; cli()' --help",
            vec!["usage", "cashu"],
        ),
        (
            "token-isolation",
            "wallet",
            "wallet",
            "test ! -e /var/run/secrets/kubernetes.io/serviceaccount/token && echo token_absent",
            vec!["token_absent"],
        ),
    ]
}

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "native-exec-acceptance",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "chain-b", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "native-exec-lnd"}},
            {"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.0", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Native Exec Mint"}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}
        ],
        "policy": {
            "allow": ["component.exec"],
            "limits": {"max_components": 8, "max_links": 16, "max_config_bytes": 16384}
        }
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let run_id = &context.run_id;
    let workspace = format!("native-exec-{run_id}");
    let draft = format!("native-exec-{run_id}");
    let instance = format!("native-exec-instance-{run_id}");
    let experiment = format!("native-exec-experiment-{run_id}");
    let lease = format!("native-exec-lease-{run_id}");

    let mut client = context.session(&workspace, "experiment-agent", CAPABILITIES)?;

    let tools = client.request("tools/list", json!({}))?;
    let advertised = expect::array(&tools, "/tools")?
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some("proofstorm_component_exec"));
    if !advertised {
        bail!("component exec was not advertised for an authorized principal: {tools}");
    }

    let created = client.call(
        "proofstorm_lab_create",
        json!({"draft_id": draft, "lab": lab_document(), "idempotency_key": format!("create-{run_id}")}),
    )?;
    let document = client.call("proofstorm_lab_read", json!({"draft_id": draft}))?;
    let validation = client.call(
        "proofstorm_lab_validate",
        json!({"lab": document.get("lab").cloned().unwrap_or(Value::Null)}),
    )?;
    if !expect::boolean(&validation, "/valid")? {
        bail!("native exec lab is invalid: {validation}");
    }

    let published = client.call(
        "proofstorm_lab_publish",
        json!({
            "draft_id": draft,
            "expected_version": expect::integer(&created, "/version")?,
            "idempotency_key": format!("publish-{run_id}"),
            "include_revision": true
        }),
    )?;

    let mut locks = std::collections::BTreeMap::new();
    for entry in expect::array(&published, "/lock/entries")? {
        locks.insert(
            expect::string(entry, "/component_id")?.to_string(),
            expect::string(entry, "/image")?.to_string(),
        );
    }
    let names: Vec<&str> = locks.keys().map(String::as_str).collect();
    if names != ["chain", "chain-b", "lightning", "mint", "wallet"]
        || !locks.values().all(|image| image.contains("@sha256:"))
    {
        bail!("native exec lab did not resolve exact images: {locks:?}");
    }

    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": instance, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": format!("materialize-{run_id}")}),
    )?;
    let waited = client.call(
        "proofstorm_lab_wait",
        json!({"instance_id": instance, "target_phase": "ready", "timeout_seconds": 120}),
    )?;
    if !expect::boolean(&waited, "/reached")? || expect::boolean(&waited, "/timed_out")? {
        bail!("native exec lab did not become ready: {waited}");
    }
    let status = client.call("proofstorm_lab_status", json!({"instance_id": instance}))?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();

    client.call(
        "proofstorm_experiment_create",
        json!({"experiment_id": experiment, "instance_id": instance, "idempotency_key": format!("create-experiment-{run_id}")}),
    )?;
    client.call(
        "proofstorm_lease_acquire",
        json!({"experiment_id": experiment, "lease_id": lease, "duration_seconds": 600, "max_actions": 6, "idempotency_key": format!("acquire-lease-{run_id}")}),
    )?;

    let mut records = Vec::new();
    for (operation, component, target, script, fragments) in commands() {
        let request = json!({
            "instance_id": instance,
            "experiment_id": experiment,
            "lease_id": lease,
            "operation_id": operation,
            "component": component,
            "target_component": target,
            "script": script,
            "timeout_seconds": 30,
            "idempotency_key": format!("{operation}-native-exec")
        });
        let accepted = client.call("proofstorm_component_exec", request.clone())?;
        let replayed = client.call("proofstorm_component_exec", request)?;
        if expect::string(&replayed, "/resource_name")?
            != expect::string(&accepted, "/resource_name")?
            || expect::integer(&replayed, "/sequence")? != expect::integer(&accepted, "/sequence")?
        {
            bail!("native exec retry changed action identity: {accepted} {replayed}");
        }

        let finished = client.call(
            "proofstorm_operation_wait",
            json!({"operation_id": operation, "timeout_seconds": 120}),
        )?;
        if expect::boolean(&finished, "/timed_out")? || !expect::boolean(&finished, "/terminal")? {
            bail!("operation {operation} did not finish: {finished}");
        }
        if expect::string(&finished, "/phase")? != "succeeded" {
            bail!("operation {operation} terminated unexpectedly: {finished}");
        }

        let content = finished
            .pointer("/artifact/content")
            .ok_or_else(|| anyhow::anyhow!("operation {operation} has no artifact content"))?;
        let output = content
            .get("combined_output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if expect::string(content, "/component")? != component
            || expect::string(content, "/target_component")? != target
            || expect::integer(content, "/exit_code")? != 0
        {
            bail!("native exec returned invalid identity or exit status: {finished}");
        }
        let lowered = output.to_lowercase();
        for fragment in &fragments {
            if !lowered.contains(&fragment.to_lowercase()) {
                bail!("native output for {operation} lacks {fragments:?}: {output}");
            }
        }
        expect::within_bytes(content, 32 * 1024, "native artifact")?;

        records.push((
            expect::string(&finished, "/operation_id")?.to_string(),
            expect::string(&accepted, "/resource_name")?.to_string(),
        ));
    }

    // Operator-side conformance: Proofstorm, not the MCP caller, fixed the
    // image, identity, token policy and network labels.
    for (_, resource) in &records {
        let action = context.kubectl.get_json(&[
            "get",
            "proofstormlabaction.proofstorm.dev",
            resource,
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        let component = expect::string(&action, "/spec/action/parameters/component")?;
        let job = context
            .kubectl
            .get_json(&["get", "job", resource, "-n", &namespace])?;
        let template = job
            .pointer("/spec/template")
            .ok_or_else(|| anyhow::anyhow!("job {resource} has no pod template"))?;
        let image = expect::string(template, "/spec/containers/0/image")?;
        let automount = template.pointer("/spec/automountServiceAccountToken");
        let labels = expect::object(template, "/metadata/labels")?;
        if image != locks[component]
            || automount != Some(&Value::Bool(false))
            || labels
                .get("proofstorm.dev/network-identity")
                .and_then(Value::as_str)
                != Some(component)
            || labels.contains_key("proofstorm.dev/component")
            || labels.contains_key("proofstorm.dev/operation")
        {
            bail!("controller did not preserve the native exec isolation contract: {template}");
        }
    }

    let journal_page = client.call(
        "proofstorm_action_list",
        json!({"experiment_id": experiment, "after_sequence": 0, "limit": 10}),
    )?;
    let journal = expect::array(&journal_page, "/actions")?;
    let sequences: Vec<u64> = journal
        .iter()
        .map(|entry| expect::integer(entry, "/sequence"))
        .collect::<Result<_>>()?;
    if sequences != [1, 2, 3, 4, 5, 6]
        || journal
            .iter()
            .any(|entry| entry.get("phase").and_then(Value::as_str) != Some("succeeded"))
    {
        bail!("native exec journal is not ordered and terminal: {journal_page}");
    }

    client.call(
        "proofstorm_lease_release",
        json!({"lease_id": lease, "idempotency_key": format!("release-lease-{run_id}")}),
    )?;
    let closed_experiment = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id": experiment, "idempotency_key": format!("close-experiment-{run_id}")}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    let operation_ids: Vec<&str> = records.iter().map(|(id, _)| id.as_str()).collect();
    let evidence = client.call(
        "proofstorm_artifact_export",
        json!({
            "experiment_id": experiment,
            "include_oracle_artifacts": false,
            "include_content": true,
            "artifact_operation_ids": operation_ids
        }),
    )?;
    if !expect::string(&evidence, "/digest")?.starts_with("sha256:")
        || expect::array(&evidence, "/content/journal")?.len() != 6
        || expect::array(&evidence, "/content/artifacts")?.len() != 6
    {
        bail!("native exec evidence is incomplete: {evidence}");
    }

    client.call("proofstorm_lab_close", json!({"instance_id": instance}))?;
    let closed = client.call(
        "proofstorm_lab_wait",
        json!({"instance_id": instance, "target_phase": "closed", "timeout_seconds": 120}),
    )?;
    if !expect::boolean(&closed, "/reached")? || expect::boolean(&closed, "/timed_out")? {
        bail!("native exec lab did not close: {closed}");
    }
    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("native exec teardown was not verified: {closed}");
    }

    context.kubectl.assert_teardown_verified()?;
    context.kubectl.assert_no_instance_namespaces()?;
    context.kubectl.assert_no_lab_actions()?;

    println!(
        "MCP native component execution, bounded artifacts, workload isolation, evidence, and verified close acceptance passed"
    );
    Ok(())
}
