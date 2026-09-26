//! Harness-independent attempt output. Transcript formats belong to adapters.
use super::{Context, score::Call};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Harness {
    OpenCode { executable: PathBuf },
    Reference,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct AttemptOutput {
    pub outcome: String,
    pub elapsed_seconds: Option<f64>,
    pub final_text: String,
    pub usage: Value,
    pub calls: Vec<Call>,
    pub unauthorized: bool,
    pub telemetry_error: Option<String>,
}
pub fn run(config: &Context) -> Result<AttemptOutput> {
    match &config.harness {
        Harness::OpenCode { .. } => super::opencode::run(config)?,
        Harness::Reference => anyhow::bail!("reference control is not a model harness"),
    }
    let outcome = retained(&config.harness, &config.work)?;
    super::save(
        &config.work.join("harness-outcome.json"),
        &serde_json::to_value(&outcome)?,
    )?;
    Ok(outcome)
}
pub fn retained(harness: &Harness, work: &Path) -> Result<AttemptOutput> {
    match harness {
        Harness::OpenCode { .. } => super::opencode::retained(work),
        Harness::Reference => anyhow::bail!("reference control cannot receive a model score"),
    }
}
