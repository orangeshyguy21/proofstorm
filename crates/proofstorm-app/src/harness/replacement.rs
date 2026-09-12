//! Explicit, content-bound consent for replacing one project MCP connection.
use super::{Harness, agents, config, json_config};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct ConnectionConflict {
    pub name: String,
    pub config: PathBuf,
    pub confirmation: String,
}
impl std::fmt::Display for ConnectionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MCP connection '{}' in {} needs replacement approval. Retry with --replace, or review it in {} gui.",
            self.name,
            self.config.display(),
            crate::command_name()
        )
    }
}
impl std::error::Error for ConnectionConflict {}

pub(super) fn merge(
    harness: Harness,
    path: &Path,
    text: Option<&str>,
    entry: &Value,
    owned: &[Value],
    confirmation: Option<&str>,
) -> Result<String> {
    let Some(text) = text else {
        ensure!(
            confirmation.is_none(),
            "connection changed; review it and retry"
        );
        return agents::merge(harness, path, None, entry, owned);
    };
    let data = if harness == Harness::Codex {
        config::value(&config::document(path, text)?)?
    } else {
        json_config::value(text, harness == Harness::Opencode)?
    };
    let servers = data[agents::key(harness)].as_object();
    let conflicts = servers
        .into_iter()
        .flat_map(|s| s.iter())
        .filter(|(name, value)| {
            json_config::is_proofstorm(name, value)
                && (name.as_str() != "proofstorm" || !owned.contains(value))
        })
        .collect::<Vec<_>>();
    if conflicts.is_empty() {
        ensure!(
            confirmation.is_none(),
            "connection changed; review it and retry"
        );
        return agents::merge(harness, path, Some(text), entry, owned);
    }
    ensure!(
        conflicts.len() == 1
            && (conflicts[0].0 == "proofstorm"
                || servers.is_none_or(|s| !s.contains_key("proofstorm"))),
        "multiple Proofstorm connections in {}; remove the duplicate connections explicitly before retrying (nothing overwritten)",
        path.display()
    );
    let (name, old) = conflicts[0];
    // Bind approval to agent, path, original bytes AND proposed connection. Never
    // reuse consent after edits, project changes or changed connection arguments.
    let expected =
        super::hash(serde_json::to_string(&json!([harness, path, text, entry]))?.as_bytes());
    if confirmation.is_none() {
        return Err(ConnectionConflict {
            name: name.clone(),
            config: path.into(),
            confirmation: expected,
        }
        .into());
    }
    ensure!(
        confirmation == Some(expected.as_str()),
        "connection changed since confirmation; review it and retry (nothing overwritten)"
    );
    let renamed = if name == "proofstorm" {
        text.to_owned()
    } else if harness == Harness::Codex {
        let mut doc = config::document(path, text)?;
        let table = doc["mcp_servers"]
            .as_table_mut()
            .context("MCP servers must be a regular TOML table")?;
        let old = table.remove(name).context("connection disappeared")?;
        table.insert("proofstorm", old);
        doc.to_string()
    } else {
        json_config::rename(
            text,
            agents::key(harness),
            name,
            "proofstorm",
            harness == Harness::Opencode,
        )?
    };
    // apply() still verifies MCP, checks unchanged bytes and writes a private
    // backup of the ORIGINAL document before activating this proposed version.
    agents::merge(
        harness,
        path,
        Some(&renamed),
        entry,
        std::slice::from_ref(old),
    )
}
