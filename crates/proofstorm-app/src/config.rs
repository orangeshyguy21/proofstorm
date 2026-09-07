//! One resolved environment for CLI and MCP. Resolution never changes kubeconfig.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const DEFAULT_DATABASE: &str = ".proofstorm/proofstorm.sqlite3";
pub const DEFAULT_WORKSPACE: &str = "local-lab";
pub const DEFAULT_CONTEXT: &str = "k3d-proofstorm";
pub const DEFAULT_NAMESPACE: &str = "proofstorm-system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Connected,
    Offline,
    Memory,
}

#[derive(Debug, Clone)]
pub struct Environment {
    pub database: PathBuf,
    pub workspace: String,
    pub principal: String,
    pub context: String,
    pub namespace: String,
    pub mode: Mode,
}

impl Environment {
    pub fn resolve(get: impl Fn(&str) -> Option<String>, cwd: &Path) -> Result<Self> {
        let value = |key: &str, default: &str| -> Result<String> {
            let value = get(key).unwrap_or_else(|| default.into());
            if value.trim().is_empty() {
                bail!("{key} must not be empty");
            }
            Ok(value)
        };
        let mode = match value("PROOFSTORM_MODE", "connected")?.as_str() {
            "connected" => Mode::Connected,
            "offline" => Mode::Offline,
            "memory" => Mode::Memory,
            _ => bail!("PROOFSTORM_MODE must be connected, offline, or memory"),
        };
        let principal = match get("PROOFSTORM_PRINCIPAL") {
            Some(principal) if !principal.trim().is_empty() => principal,
            None if mode == Mode::Memory => "local".into(),
            _ => bail!(
                "set PROOFSTORM_PRINCIPAL to the configured agent identity; use examples/opencode/proofstorm-only.json for local setup"
            ),
        };
        let database = PathBuf::from(value("PROOFSTORM_DB", DEFAULT_DATABASE)?);
        Ok(Self {
            database: if database.is_absolute() {
                database
            } else {
                cwd.join(database)
            },
            workspace: value("PROOFSTORM_WORKSPACE", DEFAULT_WORKSPACE)?,
            principal,
            context: value("PROOFSTORM_CONTEXT", DEFAULT_CONTEXT)?,
            namespace: value("PROOFSTORM_CONTROL_NAMESPACE", DEFAULT_NAMESPACE)?,
            mode,
        })
    }

    pub async fn runtime(&self) -> Result<crate::Runtime> {
        if self.mode != Mode::Connected {
            bail!("runtime access requires PROOFSTORM_MODE=connected");
        }
        let config = kube::Config::from_kubeconfig(&kube::config::KubeConfigOptions {
            context: Some(self.context.clone()),
            ..Default::default()
        })
        .await
        .with_context(|| format!("read Kubernetes context {:?}; run make setup or select PROOFSTORM_CONTEXT explicitly", self.context))?;
        Ok(crate::Runtime {
            client: kube::Client::try_from(config)?,
            control_namespace: self.namespace.clone(),
            cluster_source: self.context.clone(),
        })
    }

    pub fn report(&self) {
        eprintln!(
            "mode={:?} database={} workspace={} principal={} context={} namespace={}",
            self.mode,
            if self.mode == Mode::Memory {
                "<memory>".into()
            } else {
                self.database.display().to_string()
            },
            self.workspace,
            self.principal,
            self.context,
            self.namespace
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pin_cluster_and_resolve_storage_without_ambient_kubeconfig() {
        let config = Environment::resolve(
            |key| (key == "PROOFSTORM_PRINCIPAL").then(|| "agent".into()),
            Path::new("/repo"),
        )
        .unwrap();
        assert_eq!(
            config.database,
            Path::new("/repo/.proofstorm/proofstorm.sqlite3")
        );
        assert_eq!(config.context, "k3d-proofstorm");
        assert_eq!(config.workspace, "local-lab");
        assert_eq!(config.mode, Mode::Connected);
        assert!(Environment::resolve(|_| None, Path::new("/repo")).is_err());
    }

    #[test]
    fn modes_and_overrides_are_explicit() {
        let config = Environment::resolve(
            |key| match key {
                "PROOFSTORM_MODE" => Some("offline".into()),
                "PROOFSTORM_PRINCIPAL" => Some("reader".into()),
                "PROOFSTORM_CONTEXT" => Some("other-cluster".into()),
                "PROOFSTORM_DB" => Some("/data/lab.db".into()),
                _ => None,
            },
            Path::new("/repo"),
        )
        .unwrap();
        assert_eq!(config.database, Path::new("/data/lab.db"));
        assert_eq!(config.context, "other-cluster");
        assert_eq!(config.mode, Mode::Offline);
        assert!(Environment::resolve(|_| Some(String::new()), Path::new("/repo")).is_err());
        assert!(
            Environment::resolve(
                |key| (key == "PROOFSTORM_MODE").then(|| "typo".into()),
                Path::new("/repo")
            )
            .is_err()
        );
    }
}
