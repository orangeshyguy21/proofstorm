//! Operator health checks.
//!
//! `doctor` performs the real capability-filtered MCP handshake an agent would,
//! using the operator's own OpenCode configuration, and `cluster_schema`
//! verifies that any cell already on the cluster still deserializes with the
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
    "artifact_export",
    "catalog_entry_read",
    "catalog_list",
    "channel_open",
    "component_exec_live",
    "component_forensics",
    "component_restart",
    "conservation_oracle",
    "evidence_section_read",
    "cell_close",
    "cell_component_status_list",
    "network_heal",
    "network_partition",
    "node_restart",
    "peer_connect",
    "reachability_oracle",
    "wallet_balance",
    "wallet_fund",
    "wallet_initialize",
    "wallet_invoice",
    "wallet_melt_quote_refresh",
    "wallet_pay",
];

/// Ordinary cell workflow, without exposing manual run/session coordination.
const REQUIRED_DEVELOPER_TOOLS: &[&str] = &[
    "catalog_list",
    "catalog_entry_read",
    "catalog_config_schema_read",
    "cell_up",
    "cell_inspect",
    "session_list",
    "cell_exec",
    "cell_sync",
    "cell_finish",
    "cell_component_status_list",
    "operation_status",
    "operation_wait_many",
    "action_cancel",
];

/// Native operation replaces the wallet and bootstrap-dependent mutations.
const REQUIRED_NATIVE_TOOLS: &[&str] = &[
    "catalog_list",
    "catalog_entry_read",
    "candidate_build",
    "candidate_wait",
    "network_capabilities",
    "cell_plan",
    "cell_apply",
    "cell_wait",
    "cell_close",
    "experiment_create",
    "experiment_close",
    "session_start",
    "session_finish",
    "component_exec_live",
    "component_forensics",
    "component_restart",
    "component_logs",
    "network_partition",
    "network_heal",
    "reachability_oracle",
    "wallet_balance",
    "operation_wait_many",
    "action_cancel",
    "artifact_export",
    "evidence_section_read",
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

    let required = match environment
        .get("PROOFSTORM_TOOLSET")
        .and_then(Value::as_str)
    {
        None | Some("developer") => REQUIRED_DEVELOPER_TOOLS,
        Some("native") => REQUIRED_NATIVE_TOOLS,
        _ => REQUIRED_TOOLS,
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

/// Refuse to upgrade a controller against cells written by an older alpha schema.
///
/// Every cell already on the cluster is deserialized with the current
/// `ProofstormCellSpec`, so this check can never drift from the real types the
/// way the hand-written Python predicate could.
pub fn cluster_schema(kubectl: &Kubectl) -> Result<()> {
    let cells = kubectl.get_json(&["get", "proofstormcells.proofstorm.dev", "--all-namespaces"])?;
    let items = expect::array(&cells, "/items")?;
    for item in items {
        let name = item
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>");
        let spec = item
            .get("spec")
            .ok_or_else(|| anyhow::anyhow!("cell {name} has no spec"))?;
        if let Err(error) =
            serde_json::from_value::<proofstorm_kube::ProofstormCellSpec>(spec.clone())
        {
            bail!(
                "existing Proofstorm cell {name} uses an incompatible alpha schema: {error}\n\
                 cells are not migrated or deleted automatically; reset the disposable developer cluster:\n\
                 \x20 just down\n\
                 \x20 just setup"
            );
        }
    }
    println!(
        "cluster schema check passed for {} existing cell(s) in {CONTROL_NAMESPACE}",
        items.len()
    );
    Ok(())
}
