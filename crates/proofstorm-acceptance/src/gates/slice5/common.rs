use std::{
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::Scenario;
use crate::{GateContext, McpClient, cell, gate::CONTROL_NAMESPACE, json as expect};

pub(super) const INSTANCE: &str = "slice5-instance";
pub(super) const EXPERIMENT: &str = "slice5-experiment";

pub(super) fn components(scenario: Scenario) -> Vec<Value> {
    let mut components = vec![
        json!({"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {"txindex": true, "fallback_fee": 0.0002}}),
        json!({"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-mint"}}),
        json!({"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-payer"}}),
        json!({"id": "attacker-cln", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "attacker", "config": {"alias": "proofstorm-attacker"}}),
        json!({"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm Slice 5", "description": "Agent-created Cashu cell"}}),
        json!({"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}),
        json!({"id": "receiver-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}),
    ];
    if matches!(scenario, Scenario::Channels) {
        components.retain(|component| {
            matches!(component["kind"].as_str(), Some("bitcoin" | "lightning"))
        });
    } else if matches!(scenario, Scenario::Smoke) {
        // The Nutshell wallet's authoritative fee reader understands Nutshell's
        // SQLite mint schema, not CDK's redb storage. Conservation needs real
        // fee evidence; CDK payment interoperability has its own wallet gate.
        let mint = components.iter_mut().find(|c| c["id"] == "mint").unwrap();
        mint["implementation"] = json!("nutshell");
        mint["version"] = json!("0.21.0");
        mint["config_version"] = json!("nutshell-mint/0.20/v1");
    }
    components
}

pub(super) fn links(scenario: Scenario) -> Vec<Value> {
    let mut links = vec![
        json!({"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "network": "regtest"}),
        json!({"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "network": "regtest"}),
        json!({"id": "attacker-cln-chain", "kind": "chain_backend", "from": "attacker-cln", "to": "chain", "network": "regtest"}),
        json!({"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "mint-lnd", "method": "bolt11", "unit": "sat"}),
    ];
    if matches!(scenario, Scenario::Channels) {
        links.retain(|link| link["kind"] == "chain_backend");
    }
    links
}

/// An empty draft the composer then fills one mutation at a time.
pub(super) fn empty_cell() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "slice5-cashu-round-trip",
        "components": [],
        "links": [],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

/// The instance, experiment and session triple every runtime action carries.
pub(super) fn scoped(operation: &str, extra: Value) -> Value {
    let mut base = json!({
        "name": INSTANCE,
        "run_id": EXPERIMENT,

        "request_id": operation
    });
    if let (Some(target), Value::Object(source)) = (base.as_object_mut(), extra) {
        for (key, value) in source {
            target.insert(key, value);
        }
    }
    base
}

/// Submit an action twice and prove the retry did not change its identity.
pub(super) fn submit_idempotent(
    context: &GateContext,
    client: &mut McpClient,
    submit: fn(&GateContext, &mut McpClient, Value) -> Result<Value>,
    request: Value,
    label: &str,
) -> Result<Value> {
    let accepted = submit(context, client, request.clone())?;
    let retried = submit(context, client, request)?;
    if expect::string(&retried, "/resource_name")? != expect::string(&accepted, "/resource_name")?
        || expect::integer(&retried, "/sequence")? != expect::integer(&accepted, "/sequence")?
    {
        bail!("{label} retry changed the accepted action identity: {accepted} {retried}");
    }
    Ok(accepted)
}

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// TCP reachability from one pod to the mint, used as the CNI ground truth
/// alongside the MCP reachability oracle.
pub(super) fn pod_can_reach_mint(
    context: &GateContext,
    namespace: &str,
    pod: &str,
) -> Result<bool> {
    let (ok, _, _) = context.kubectl.try_run(&[
        "exec",
        "-n",
        namespace,
        pod,
        "--",
        "/opt/proofstorm/driver",
        "tcp",
        "mint",
        "3338",
    ])?;
    Ok(ok)
}

pub(super) fn wait_reachability(
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

pub(super) fn component_pod(
    context: &GateContext,
    namespace: &str,
    component: &str,
) -> Result<String> {
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
pub(super) fn observe_mint_reachability(
    client: &mut McpClient,
    operation: &str,
    component: &str,
    expected: bool,
    observations: &mut Vec<String>,
) -> Result<()> {
    client.call(
        "network_probe",
        scoped(
            operation,
            json!({
                "from_component": component,
                "to_component": "mint",
                "service": "http",
                "timeout_seconds": 2,
                "attempts": 3}),
        ),
    )?;
    let observed = cell::wait_operation(client, operation, 120)?;
    let content = cell::artifact_content(&observed)?;
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

pub(super) fn action_kinds(context: &GateContext, instance_key: &str) -> Result<Value> {
    context.kubectl.get_json(&[
        "get",
        "proofstormcellactions.proofstorm.dev",
        "-n",
        CONTROL_NAMESPACE,
        "-l",
        &format!("proofstorm.dev/instance={instance_key}"),
    ])
}

/// Map runtime action `operationId` to its typed action kind.
pub(super) fn kinds_by_operation(
    items: &Value,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut map = std::collections::BTreeMap::new();
    for item in expect::array(items, "/items")? {
        map.insert(
            expect::string(item, "/spec/operationId")?.to_string(),
            expect::string(item, "/spec/action/kind")?.to_string(),
        );
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservation_fixture_uses_the_authoritative_fee_schema() {
        for scenario in [Scenario::Smoke, Scenario::Recovery, Scenario::Network] {
            let components = components(scenario);
            let mint = components.iter().find(|c| c["id"] == "mint").unwrap();
            let implementation = if matches!(scenario, Scenario::Smoke) {
                "nutshell"
            } else {
                "cdk"
            };
            assert_eq!(mint["implementation"], implementation);
            let wallet = components.iter().find(|c| c["id"] == "wallet").unwrap();
            assert_eq!(wallet["implementation"], "nutshell-wallet");
        }
    }

    #[test]
    fn channel_scenario_needs_only_bitcoin_and_lightning() {
        let components = components(Scenario::Channels);
        let ids: std::collections::BTreeSet<_> = components
            .iter()
            .map(|component| component["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["chain", "mint-lnd", "payer-lnd", "attacker-cln"]
                .into_iter()
                .collect()
        );
        for link in links(Scenario::Channels) {
            assert!(ids.contains(link["from"].as_str().unwrap()));
            assert!(ids.contains(link["to"].as_str().unwrap()));
        }
    }
}
