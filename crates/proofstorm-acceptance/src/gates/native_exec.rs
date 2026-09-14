//! Native component execution: six digest-pinned native commands across two
//! independently selectable Bitcoin nodes, action idempotency, the controller's
//! isolation contract, an ordered journal, evidence export, and verified close.
//!
//! Ported from `tests/kubernetes/native_exec_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, gate::CONTROL_NAMESPACE, json as expect};

const BITCOIN_RPC: &str = concat!(
    "bitcoin-cli -regtest -rpcconnect=127.0.0.1 -rpcport=18443 ",
    "-rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only ",
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
            "chain-b",
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
            "cd /app && cashu --help",
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

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "native-exec-acceptance",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "chain-b", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "native-exec-lnd"}},
            {"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.0", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Native Exec Mint"}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "network": "regtest"},
            {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "lightning", "method": "bolt11", "unit": "sat"}
        ],
        "policy": {
            "allow": ["component.exec_live"],
            "limits": {"max_components": 8, "max_links": 16, "max_config_bytes": 16384}
        }
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let run_id = &context.run_id;
    let workspace = format!("native-exec-{run_id}");

    let instance = format!("native-exec-instance-{run_id}");
    let experiment = format!("native-exec-experiment-{run_id}");

    let mut client = context.default_session(&workspace, "experiment-agent")?;

    let tools = client.request("tools/list", json!({}))?;
    let advertised = expect::array(&tools, "/tools")?
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some("cell_exec"));
    if !advertised {
        bail!("component exec was not advertised for an authorized principal: {tools}");
    }

    let preview = client.call(
        "cell_plan",
        json!({"name":instance,"request_id":format!("preview-{run_id}"),"cell":cell_document()}),
    )?;
    let published = crate::cell::review(&mut client, &preview)?;

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
        bail!("native exec cell did not resolve exact images: {locks:?}");
    }

    crate::cell::apply(&mut client, &preview)?;
    let waited = client.call(
        "cell_wait",
        json!({"name": instance, "target_phase": "ready", "timeout_seconds": 120}),
    )?;
    if !expect::boolean(&waited, "/reached")? || expect::boolean(&waited, "/timed_out")? {
        bail!("native exec cell did not become ready: {waited}");
    }
    let status = crate::cell::status(&mut client, &(instance))?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();

    client.call(
        "run_start",
        json!({"request_id":"6730","run_id": experiment, "name": instance}),
    )?;

    let mut records = Vec::new();
    let mut serving_targets = 0;
    for (operation, component, target, script, fragments) in commands() {
        let request = json!({
            "name": instance,
            "run_id": experiment,

            "request_id": operation,
            "component": component,
            "script": script,
            "output": {"mode":"public"},
            "timeout_seconds": 30});
        let accepted = client.call("cell_exec", request.clone())?;
        let replayed = client.call("cell_exec", request)?;
        if expect::string(&replayed, "/operation_id")?
            != expect::string(&accepted, "/operation_id")?
            || expect::integer(&replayed, "/sequence")? != expect::integer(&accepted, "/sequence")?
        {
            bail!("native exec retry changed action identity: {accepted} {replayed}");
        }

        let finished = crate::cell::wait_one(&mut client, operation, 120)?;
        if expect::boolean(&finished, "/timed_out")? || !expect::boolean(&finished, "/terminal")? {
            bail!("operation {operation} did not finish: {finished}");
        }
        if expect::string(&finished, "/phase")? != "succeeded" {
            bail!("operation {operation} terminated unexpectedly: {finished}");
        }

        let content = finished
            .pointer("/artifact/content")
            .ok_or_else(|| anyhow::anyhow!("operation {operation} has no artifact content"))?;
        let output = format!(
            "{}{}",
            content
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            content
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        if expect::string(content, "/component")? != component
            || expect::string(content, "/execution_context")? != "live_component"
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

        serving_targets += usize::from(component == target);

        records.push((
            expect::string(&finished, "/operation_id")?.to_string(),
            expect::string(
                &client.call(
                    "operation_read",
                    json!({"operation_id":operation,"pointer":"/resource_name"}),
                )?,
                "/value",
            )?
            .to_string(),
            expect::string(content, "/pod")?.to_string(),
            component.to_string(),
        ));
    }

    if serving_targets == 0 {
        bail!("no exec observed a serving target, so endpoint reporting is unproven");
    }

    // A component's own log is readable without starting a workload, which is
    // the only way to see a native error from a component that never became
    // ready. This target is healthy, so the read must still succeed and carry
    // the pod state that explains the log.
    let logs_operation = format!("native-exec-logs-{run_id}");
    client.call(
        "component_logs",
        json!({
            "name": instance,
            "run_id": experiment,

            "request_id": logs_operation,
            "component": "chain",
            "tail_lines": 25}),
    )?;
    let logs = crate::cell::wait_one(&mut client, &(logs_operation), 60)?;
    if expect::boolean(&logs, "/timed_out")? || expect::string(&logs, "/phase")? != "succeeded" {
        bail!("component logs did not succeed: {logs}");
    }
    let log_content = logs
        .pointer("/artifact/content")
        .ok_or_else(|| anyhow::anyhow!("component logs produced no artifact"))?;
    if expect::string(log_content, "/component")? != "chain"
        || expect::integer(log_content, "/tail_lines")? != 25
    {
        bail!("component logs artifact lost its identity: {log_content}");
    }
    if !expect::boolean(log_content, "/container_ready")? {
        bail!("a ready component must report container readiness: {log_content}");
    }
    if expect::string(log_content, "/log")?.trim().is_empty() {
        bail!("a running Bitcoin node must have produced log output: {log_content}");
    }

    // Operator-side conformance: Proofstorm, not the MCP caller, fixed the
    // image, identity, token policy and network labels.
    for (_, resource, pod_name, expected_component) in &records {
        let action = context.kubectl.get_json(&[
            "get",
            "proofstormcellaction.proofstorm.dev",
            resource,
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        let component = expect::string(&action, "/spec/action/parameters/component")?;
        let pod = context
            .kubectl
            .get_json(&["get", "pod", pod_name, "-n", &namespace])?;
        let image = expect::string(&pod, "/spec/containers/0/image")?;
        let automount = pod.pointer("/spec/automountServiceAccountToken");
        let labels = expect::object(&pod, "/metadata/labels")?;
        if image != locks[component]
            || automount != Some(&Value::Bool(false))
            || labels
                .get("proofstorm.dev/component")
                .and_then(Value::as_str)
                != Some(expected_component.as_str())
        {
            bail!("controller did not execute in the selected live component: {pod}");
        }
    }

    let journal_page = Ok::<_, anyhow::Error>(
        json!({"actions":crate::cell::journal(&mut client, &(experiment))?}),
    )?;
    let journal = expect::array(&journal_page, "/actions")?;
    if journal
        .last()
        .and_then(|entry| entry.get("kind"))
        .and_then(Value::as_str)
        != Some("component_logs")
    {
        bail!("the log read must be journaled like any other action: {journal_page}");
    }
    let sequences: Vec<u64> = journal
        .iter()
        .map(|entry| expect::integer(entry, "/sequence"))
        .collect::<Result<_>>()?;
    // Six native executions followed by the component log read.
    if sequences != [1, 2, 3, 4, 5, 6, 7]
        || journal
            .iter()
            .any(|entry| entry.get("phase").and_then(Value::as_str) != Some("succeeded"))
    {
        bail!("native exec journal is not ordered and terminal: {journal_page}");
    }

    let closed_experiment = client.call(
        "run_finish",
        json!({"request_id":"13336","run_id": experiment}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    // The log read is evidence like any execution, so it is exported too.
    let mut operation_ids: Vec<&str> = records.iter().map(|(id, _, _, _)| id.as_str()).collect();
    operation_ids.push(logs_operation.as_str());
    let evidence = crate::cell::evidence(
        &mut client,
        json!({
            "run_id": experiment,
            "include_oracle_artifacts": false,

            "artifact_operation_ids": operation_ids
        }),
    )?;
    if !expect::string(&evidence, "/digest")?.starts_with("sha256:")
        || expect::array(&evidence, "/content/journal")?.len() != 7
        || expect::array(&evidence, "/content/artifacts")?.len() != 7
    {
        bail!("native exec evidence is incomplete: {evidence}");
    }

    client.call("cell_remove", json!({"name": instance}))?;
    let closed = client.call(
        "cell_wait",
        json!({"name": instance, "target_phase": "closed", "timeout_seconds": 120}),
    )?;
    if !expect::boolean(&closed, "/reached")? || expect::boolean(&closed, "/timed_out")? {
        bail!("native exec cell did not close: {closed}");
    }
    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("native exec teardown was not verified: {closed}");
    }

    context.record(
        "native-exec-proof.json",
        &json!({
            "evidence_digest":evidence["digest"], "journal_count":7, "artifact_count":7,
            "closed":closed
        }),
    )?;
    context.kubectl.assert_no_instance_namespaces()?;
    context.kubectl.assert_no_cell_actions()?;

    println!(
        "MCP native component execution, bounded artifacts, workload isolation, evidence, and verified close acceptance passed"
    );
    Ok(())
}
