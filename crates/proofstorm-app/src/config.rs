//! One resolved environment for CLI and MCP. Resolution never changes kubeconfig.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const DEFAULT_DATABASE: &str = ".proofstorm/proofstorm.sqlite3";
pub const DEFAULT_WORKSPACE: &str = "local-cell";
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
                "set PROOFSTORM_PRINCIPAL to the configured agent identity; for normal setup use storm agent open or storm agent configure with your agent name"
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
        let context = match get("PROOFSTORM_CONTEXT") {
            Some(context) => {
                ensure_path_not_empty("PROOFSTORM_CONTEXT", &context)?;
                context
            }
            None => installation
                .as_ref()
                .map(crate::installation::Installation::context)
                .unwrap_or_default(),
        };
        if mode == Mode::Connected {
            anyhow::ensure!(
                kubeconfig.is_some() && !context.trim().is_empty(),
                "select an installation with --home (PROOFSTORM_HOME); an external runtime requires both PROOFSTORM_CONTEXT and PROOFSTORM_KUBECONFIG explicitly; the global kubeconfig is never used"
            );
        }
        Ok(Self {
            installation,
            kubeconfig,
            database,
            workspace: value("PROOFSTORM_WORKSPACE", DEFAULT_WORKSPACE)?,
            principal,
            context,
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
        // Recheck here too: Environment is public and can be constructed without resolve.
        anyhow::ensure!(
            !self.context.trim().is_empty(),
            "runtime access requires an explicitly selected Kubernetes context"
        );
        let path = self.kubeconfig.as_ref().context(
            "runtime access requires an explicitly selected kubeconfig; the global kubeconfig is never used",
        )?;
        let kubeconfig = kube::config::Kubeconfig::read_from(path).with_context(|| {
            format!(
                "read selected kubeconfig {}; no fallback to the global configuration",
                path.display()
            )
        })?;
        let config = kube::Config::from_custom_kubeconfig(kubeconfig, &options)
            .await
            .with_context(|| format!("read selected Kubernetes context {:?}", self.context))?;
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
                .map_or_else(|| "<none>".into(), |path| path.display().to_string())
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
    fn connected_mode_requires_both_external_selectors_without_an_installation() {
        for (context, kubeconfig) in [
            (None, None),
            (Some("external"), None),
            (None, Some("external.yaml")),
        ] {
            let error = Environment::resolve(
                |key| match key {
                    "PROOFSTORM_PRINCIPAL" => Some("agent".into()),
                    "PROOFSTORM_CONTEXT" => context.map(str::to_owned),
                    "PROOFSTORM_KUBECONFIG" => kubeconfig.map(str::to_owned),
                    // An ambient global config never counts as a selector.
                    "KUBECONFIG" => Some("/global/config".into()),
                    _ => None,
                },
                Path::new("/repo"),
            )
            .unwrap_err();
            assert!(error.to_string().contains("requires both"), "{error}");
        }
    }

    #[test]
    fn external_runtime_selection_is_explicit_and_resolves_relative_paths() {
        let config = Environment::resolve(
            |key| match key {
                "PROOFSTORM_PRINCIPAL" => Some("agent".into()),
                "PROOFSTORM_CONTEXT" => Some("external".into()),
                "PROOFSTORM_KUBECONFIG" => Some("selected.yaml".into()),
                _ => None,
            },
            Path::new("/repo"),
        )
        .unwrap();
        assert_eq!(
            config.database,
            Path::new("/repo/.proofstorm/proofstorm.sqlite3")
        );
        assert_eq!(config.context, "external");
        assert_eq!(
            config.kubeconfig,
            Some(PathBuf::from("/repo/selected.yaml"))
        );
        assert_eq!(config.workspace, "local-cell");
        assert_eq!(config.mode, Mode::Connected);
        assert!(Environment::resolve(|_| None, Path::new("/repo")).is_err());
    }

    #[tokio::test]
    async fn non_connected_modes_cannot_access_a_runtime() {
        for mode in ["offline", "memory"] {
            let mut config = Environment::resolve(
                |key| match key {
                    "PROOFSTORM_MODE" => Some(mode.into()),
                    "PROOFSTORM_PRINCIPAL" if mode == "offline" => Some("reader".into()),
                    _ => None,
                },
                Path::new("/repo"),
            )
            .unwrap();
            assert!(config.context.is_empty());
            assert!(config.kubeconfig.is_none());
            assert!(config.kubernetes_config().await.is_err());
            // Direct mutation cannot recover the old ambient fallback either.
            config.mode = Mode::Connected;
            assert!(config.kubernetes_config().await.is_err());
            config.context = "external".into();
            assert!(
                config
                    .kubernetes_config()
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("explicitly selected kubeconfig")
            );
        }
    }

    #[tokio::test]
    async fn installed_selection_uses_private_paths_and_missing_files_do_not_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let installation =
            crate::installation::Installation::initialize(root.path(), None, None).unwrap();
        let config = Environment::resolve(
            |key| match key {
                "PROOFSTORM_HOME" => Some(root.path().display().to_string()),
                "PROOFSTORM_PRINCIPAL" => Some("developer".into()),
                _ => None,
            },
            Path::new("/unrelated"),
        )
        .unwrap();
        assert_eq!(config.context, installation.context());
        assert_eq!(config.kubeconfig, Some(installation.kubeconfig()));
        assert_eq!(config.database, installation.database());
        let error = config.kubernetes_config().await.unwrap_err();
        assert!(error.to_string().contains("no fallback"), "{error}");
    }

    #[test]
    fn modes_and_overrides_are_explicit() {
        let config = Environment::resolve(
            |key| match key {
                "PROOFSTORM_MODE" => Some("offline".into()),
                "PROOFSTORM_PRINCIPAL" => Some("reader".into()),
                "PROOFSTORM_CONTEXT" => Some("other-cluster".into()),
                "PROOFSTORM_DB" => Some("/data/cell.db".into()),
                _ => None,
            },
            Path::new("/repo"),
        )
        .unwrap();
        assert_eq!(config.database, Path::new("/data/cell.db"));
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
