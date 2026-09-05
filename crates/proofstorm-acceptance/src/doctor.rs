//! Operator health checks.
//!
//! `doctor` performs the real capability-filtered MCP handshake an agent would,
//! using the operator's own OpenCode configuration, and `cluster_schema`
//! verifies that any lab already on the cluster still deserializes with the
//! current API types.
//!
//! Both replace Python: `tools/proofstorm-doctor.py` and the inline schema
//! heredoc that lived in `tools/proofstorm-cluster`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{Kubectl, McpClient, gate::CONTROL_NAMESPACE, json as expect};

/// Tools a fully granted principal must still see after capability filtering.
const REQUIRED_TOOLS: &[&str] = &[
    "proofstorm_artifact_export",
    "proofstorm_catalog_entry_read",
    "proofstorm_catalog_list",
    "proofstorm_channel_open",
    "proofstorm_component_exec_live",
    "proofstorm_component_forensics",
    "proofstorm_component_restart",
    "proofstorm_conservation_oracle",
    "proofstorm_evidence_section_read",
    "proofstorm_lab_close",
    "proofstorm_lab_component_status_list",
    "proofstorm_network_heal",
    "proofstorm_network_partition",
    "proofstorm_node_restart",
    "proofstorm_peer_connect",
    "proofstorm_reachability_oracle",
    "proofstorm_wallet_balance",
    "proofstorm_wallet_fund",
    "proofstorm_wallet_initialize",
    "proofstorm_wallet_invoice",
    "proofstorm_wallet_melt_quote_refresh",
    "proofstorm_wallet_pay",
];

/// Native operation replaces the wallet and bootstrap-dependent mutations.
const REQUIRED_NATIVE_TOOLS: &[&str] = &[
    "proofstorm_catalog_list",
    "proofstorm_catalog_entry_read",
    "proofstorm_candidate_build",
    "proofstorm_candidate_wait",
    "proofstorm_network_capabilities",
    "proofstorm_lab_plan",
    "proofstorm_lab_apply",
    "proofstorm_lab_wait",
    "proofstorm_lab_close",
    "proofstorm_experiment_create",
    "proofstorm_experiment_close",
    "proofstorm_lease_acquire",
    "proofstorm_lease_release",
    "proofstorm_component_exec_live",
    "proofstorm_component_forensics",
    "proofstorm_component_restart",
    "proofstorm_component_logs",
    "proofstorm_network_partition",
    "proofstorm_network_heal",
    "proofstorm_reachability_oracle",
    "proofstorm_wallet_balance",
    "proofstorm_operation_wait_many",
    "proofstorm_action_cancel",
    "proofstorm_artifact_export",
    "proofstorm_evidence_section_read",
];

/// Spawn the configured server and assert it still advertises every required tool.
///
/// The database path is redirected to a temporary file so the doctor never
/// touches the operator's durable store.
pub fn run(mcp_binary: &Path, config_path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let config: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", config_path.display()))?;
    let environment = expect::object(&config, "/mcp/proofstorm/environment")
        .context("the configuration has no Proofstorm MCP environment")?;

    let directory = tempfile::Builder::new()
        .prefix("proofstorm-doctor-")
        .tempdir()
        .context("create the doctor database directory")?;
    let database = directory.path().join("doctor.sqlite3");

    let mut variables: Vec<(String, String)> = Vec::new();
    for (key, value) in environment {
        let text = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("environment value for {key} is not a string"))?;
        variables.push((key.clone(), text.to_string()));
    }
    variables.retain(|(key, _)| key != "PROOFSTORM_DB");
    variables.push((
        "PROOFSTORM_DB".to_string(),
        database.to_string_lossy().to_string(),
    ));

    let mut client = McpClient::spawn(mcp_binary, "proofstorm-doctor", &variables)?;
    let listed = client.request("tools/list", json!({}))?;
    let names: Vec<&str> = expect::array(&listed, "/tools")?
        .iter()
        .map(|tool| expect::string(tool, "/name"))
        .collect::<Result<_>>()?;

    let required = if environment
        .get("PROOFSTORM_TOOLSET")
        .and_then(Value::as_str)
        == Some("native")
    {
        REQUIRED_NATIVE_TOOLS
    } else {
        REQUIRED_TOOLS
    };
    let missing: Vec<&&str> = required
        .iter()
        .filter(|required| !names.contains(*required))
        .collect();
    if !missing.is_empty() {
        bail!("MCP capability configuration hides required tools: {missing:?}");
    }

    println!(
        "MCP stdio handshake passed with {} capability-filtered tools",
        names.len()
    );
    Ok(())
}

/// Refuse to upgrade a controller against labs written by an older alpha schema.
///
/// Every lab already on the cluster is deserialized with the current
/// `ProofstormLabSpec`, so this check can never drift from the real types the
/// way the hand-written Python predicate could.
pub fn cluster_schema(kubectl: &Kubectl) -> Result<()> {
    let labs = kubectl.get_json(&["get", "proofstormlabs.proofstorm.dev", "--all-namespaces"])?;
    let items = expect::array(&labs, "/items")?;
    for item in items {
        let name = item
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>");
        let spec = item
            .get("spec")
            .ok_or_else(|| anyhow::anyhow!("lab {name} has no spec"))?;
        if let Err(error) =
            serde_json::from_value::<proofstorm_kube::ProofstormLabSpec>(spec.clone())
        {
            bail!(
                "existing Proofstorm lab {name} uses an incompatible alpha schema: {error}\n\
                 labs are not migrated or deleted automatically; reset the disposable developer cluster:\n\
                 \x20 make down\n\
                 \x20 make setup"
            );
        }
    }
    println!(
        "cluster schema check passed for {} existing lab(s) in {CONTROL_NAMESPACE}",
        items.len()
    );
    Ok(())
}
