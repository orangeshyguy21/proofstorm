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
    pub installation: Option<crate::installation::Installation>,
    pub kubeconfig: Option<PathBuf>,
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
        let absolute = |path: PathBuf| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        };
        let optional_path = |key: &str| -> Result<Option<PathBuf>> {
            get(key)
                .map(|path| {
                    ensure_path_not_empty(key, &path)?;
                    Ok(absolute(PathBuf::from(path)))
                })
                .transpose()
        };
        let installation = optional_path("PROOFSTORM_HOME")?
            .map(|home| crate::installation::Installation::load(&home))
            .transpose()?;
        let database = optional_path("PROOFSTORM_DB")?.unwrap_or_else(|| {
            installation.as_ref().map_or_else(
                || cwd.join(DEFAULT_DATABASE),
                crate::installation::Installation::database,
            )
        });
        let kubeconfig = optional_path("PROOFSTORM_KUBECONFIG")?.or_else(|| {
            installation
                .as_ref()
                .map(crate::installation::Installation::kubeconfig)
        });
        let default_context = installation.as_ref().map_or_else(
            || DEFAULT_CONTEXT.into(),
            crate::installation::Installation::context,
        );
        Ok(Self {
            installation,
            kubeconfig,
            database,
            workspace: value("PROOFSTORM_WORKSPACE", DEFAULT_WORKSPACE)?,
            principal,
            context: value("PROOFSTORM_CONTEXT", &default_context)?,
            namespace: value("PROOFSTORM_CONTROL_NAMESPACE", DEFAULT_NAMESPACE)?,
            mode,
        })
    }

    pub async fn runtime(&self) -> Result<crate::Runtime> {
        if let Some(installation) = &self.installation {
            anyhow::ensure!(
                self.context == installation.context()
                    && self.kubeconfig.as_ref() == Some(&installation.kubeconfig())
                    && self.namespace == DEFAULT_NAMESPACE,
                "an installed runtime requires its private context, kubeconfig and namespace; omit --home for an explicitly selected external runtime"
            );
        }
        let config = self.kubernetes_config().await?;
        Ok(crate::Runtime {
            client: kube::Client::try_from(config)?,
            control_namespace: self.namespace.clone(),
            cluster_source: self.context.clone(),
        })
    }

    pub(crate) async fn kubernetes_config(&self) -> Result<kube::Config> {
        if self.mode != Mode::Connected {
            bail!("runtime access requires PROOFSTORM_MODE=connected");
        }
        let options = kube::config::KubeConfigOptions {
            context: Some(self.context.clone()),
            ..Default::default()
        };
        let config = if let Some(path) = &self.kubeconfig {
            let kubeconfig = kube::config::Kubeconfig::read_from(path)
                .with_context(|| format!("read selected kubeconfig {}; no fallback to the developer cluster", path.display()))?;
            kube::Config::from_custom_kubeconfig(kubeconfig, &options).await
        } else {
            kube::Config::from_kubeconfig(&options).await
        }
        .with_context(|| format!("read Kubernetes context {:?}; run just setup or select PROOFSTORM_CONTEXT explicitly", self.context))?;
        Ok(config)
    }

    pub fn report(&self) {
        eprintln!(
            "mode={:?} database={} workspace={} principal={} context={} namespace={} kubeconfig={}",
            self.mode,
            if self.mode == Mode::Memory {
                "<memory>".into()
            } else {
                self.database.display().to_string()
            },
            self.workspace,
            self.principal,
            self.context,
            self.namespace,
            self.kubeconfig
                .as_ref()
                .map_or_else(|| "<user config>".into(), |path| path.display().to_string())
        );
    }
}

fn ensure_path_not_empty(key: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{key} must not be empty");
    }
    Ok(())
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
