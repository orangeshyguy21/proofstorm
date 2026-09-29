//! Harness-independent attempt output. Transcript formats belong to adapters.
use super::{Context, score::Call};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Durable intent before spawning a model-capable CLI. An interrupted or failed
/// spawn is conservatively treated as possible model exposure, never retried.
pub(super) fn mark_launch(config: &Context) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(config.work.join("model-launch.json"))?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "state":"launch_requested", "model":config.model,
            "task":config.task.id, "task_version":config.task.version
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Harness {
    OpenCode {
        executable: PathBuf,
    },
    Codex {
        executable: PathBuf,
        auth_file: Option<PathBuf>,
    },
    ClaudeCode {
        executable: PathBuf,
        #[serde(default)]
        auth: ClaudeAuth,
    },
    Reference,
}
/// How Claude Code authenticates. `Login` reuses the machine's normal Claude
/// Code login and config directory; `Environment` isolates both and takes one
/// explicit credential from the runner environment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeAuth {
    #[default]
    Login,
    Environment,
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
        Harness::Codex { .. } => super::codex::run(config)?,
        Harness::ClaudeCode { .. } => super::claude::run(config)?,
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
        Harness::Codex { .. } => super::codex::retained(work),
        Harness::ClaudeCode { .. } => super::claude::retained(work),
        Harness::Reference => anyhow::bail!("reference control cannot receive a model score"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_intent_is_private_and_cannot_be_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let context = Context {
            root: dir.path().into(),
            work: dir.path().into(),
            home: dir.path().into(),
            mcp: dir.path().join("unused"),
            model: "fixture".into(),
            harness: Harness::Reference,
            task: super::super::task::o1().clone(),
        };
        mark_launch(&context).unwrap();
        let path = dir.path().join("model-launch.json");
        let original = std::fs::read(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(mark_launch(&context).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}
