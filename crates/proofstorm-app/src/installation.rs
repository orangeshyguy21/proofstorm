//! Durable identity and pure runtime configuration for an isolated installation.
//! Creating an installation does not contact Docker or Kubernetes.
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    time::Duration,
};

pub const CATALOG_REGISTRY: &str = "proofstorm-registry.localhost:5000";
pub const OWNER_LABEL: &str = "proofstorm.dev/installation";
const MANIFEST: &str = "installation.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub format_version: u32,
    pub id: String,
    pub home: PathBuf,
    pub api_port: u16,
    pub registry_port: u16,
}

/// An OS-process-lifetime `SQLite` lock. A crash releases the transaction.
pub struct InstallationGuard(Connection);

impl Drop for InstallationGuard {
    fn drop(&mut self) {
        let _ = self.0.execute_batch("ROLLBACK");
    }
}

impl Installation {
    /// Serialize all future setup/reset mutations using this same guard.
    pub fn lock(home: &Path) -> Result<InstallationGuard> {
        private_directory(home)?;
        let path = home.join("installation-lock.sqlite3");
        create_private_file(&path)?;
        let db = Connection::open(&path)?;
        db.busy_timeout(Duration::ZERO)?;
        db.execute_batch("BEGIN IMMEDIATE")
            .context("another installation operation is running; retry when it finishes")?;
        Ok(InstallationGuard(db))
    }

    /// Initialize once. Explicit ports on retries must match the saved ports.
    /// Port discovery is provisional; setup must check availability again.
    pub fn initialize(
        home: &Path,
        api_port: Option<u16>,
        registry_port: Option<u16>,
    ) -> Result<Self> {
        let _guard = Self::lock(home)?;
        let home = home.canonicalize()?;
        if home.join(MANIFEST).try_exists()? {
            let installation = Self::load(&home)?;
            ensure!(
                api_port.is_none_or(|port| port == installation.api_port)
                    && registry_port.is_none_or(|port| port == installation.registry_port),
                "requested ports differ from this installation's saved ports"
            );
            installation.write_runtime_config()?;
            return Ok(installation);
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("installation ID: {error}"))?;
        let mut id = String::with_capacity(32);
        for byte in random {
            write!(id, "{byte:02x}")?;
        }
        // Keep both sockets alive until distinct choices have been recorded.
        let api = available_port(api_port)?;
        let registry = available_port(registry_port)?;
        let installation = Self {
            format_version: 1,
            id,
            home,
            api_port: api.local_addr()?.port(),
            registry_port: registry.local_addr()?.port(),
        };
        write_new_private(
            &installation.home.join(MANIFEST),
            &serde_json::to_vec_pretty(&installation)?,
        )?;
        installation.write_runtime_config()?;
        Ok(installation)
    }

    /// Reading configuration never initializes an environment or adopts a cluster.
    pub fn load(home: &Path) -> Result<Self> {
        let home = home.canonicalize().with_context(|| {
            format!(
                "installation home {} is missing; run proofstorm --home <path> init",
                home.display()
            )
        })?;
        let path = home.join(MANIFEST);
        let bytes = fs::read(&path).with_context(|| {
            format!(
                "read {}; initialize this home explicitly with proofstorm --home <path> init",
                path.display()
            )
        })?;
        let installation: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid installation manifest {}", path.display()))?;
        ensure!(
            installation.format_version == 1,
            "unsupported installation format"
        );
        ensure!(
            installation.id.len() == 32
                && installation
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "invalid installation ID"
        );
        ensure!(
            installation.home == home,
            "installation belongs to a different home; copied installation state cannot adopt its runtime"
        );
        ensure!(
            installation.api_port != 0
                && installation.registry_port != 0
                && installation.api_port != installation.registry_port,
            "invalid installation ports"
        );
        Ok(installation)
    }

    #[must_use]
    pub fn cluster_name(&self) -> String {
        // k3d limits cluster names to 32 characters. Keep 112 bits of the ID;
        // the full 128-bit identity remains in the manifest and ownership label.
        format!("pst-{}", self.id.chars().take(28).collect::<String>())
    }

    #[must_use]
    pub fn context(&self) -> String {
        format!("k3d-{}", self.cluster_name())
    }

    #[must_use]
    pub fn network_name(&self) -> String {
        self.context()
    }

    #[must_use]
    pub fn registry_name(&self) -> String {
        format!("k3d-{}-registry", self.cluster_name())
    }

    #[must_use]
    pub fn registry_endpoint(&self) -> String {
        format!("http://{}:5000", self.registry_name())
    }

    #[must_use]
    pub fn host_registry(&self) -> String {
        format!("127.0.0.1:{}", self.registry_port)
    }

    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.home.join("proofstorm.sqlite3")
    }

    #[must_use]
    pub fn kubeconfig(&self) -> PathBuf {
        self.home.join("kubeconfig")
    }

    #[must_use]
    pub fn cluster_config_path(&self) -> PathBuf {
        self.home.join("k3d.yaml")
    }

    /// JSON is also valid YAML. Keep catalog digests unchanged and route pulls
    /// through the private Docker network's registry endpoint.
    #[must_use]
    pub fn cluster_config(&self) -> Value {
        let mirrors =
            json!({"mirrors": {CATALOG_REGISTRY: {"endpoint": [self.registry_endpoint()]}}});
        json!({
            "apiVersion": "k3d.io/v1alpha5", "kind": "Simple",
            "metadata": {"name": self.cluster_name()},
            "image": format!("docker.io/rancher/k3s:{}", pinned_k3s()),
            "servers": 1, "agents": 1,
            "kubeAPI": {"host": "127.0.0.1", "hostIP": "127.0.0.1", "hostPort": self.api_port.to_string()},
            "registries": {
                "create": {"name": self.registry_name(), "host": "127.0.0.1", "hostPort": self.registry_port.to_string()},
                // k3d interprets a single-line string as a file path, not inline config.
                "config": format!("{mirrors:#}")
            },
            "options": {
                "kubeconfig": {"updateDefaultKubeconfig": false, "switchCurrentContext": false},
                "k3d": {"wait": true, "timeout": "120s"},
                "k3s": {"extraArgs": [
                    {"arg": "--disable=traefik", "nodeFilters": ["server:*"]},
                    {"arg": "--disable=servicelb", "nodeFilters": ["server:*"]},
                    {"arg": "--disable-default-registry-endpoint", "nodeFilters": ["server:*", "agent:*"]}
                ]},
                "runtime": {"labels": [{"label": format!("{OWNER_LABEL}={}", self.id), "nodeFilters": ["server:*", "agent:*", "loadbalancer"]}]}
            }
        })
    }

    /// A typed command description for setup. Export kubeconfig through stdout
    /// (`k3d kubeconfig get NAME`), never by merging the user's config.
    #[must_use]
    pub fn creation_command(&self, k3d: &Path) -> std::process::Command {
        let mut command = std::process::Command::new(k3d);
        // Viper gives K3D_* variables precedence over the generated config.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("K3D_") {
                command.env_remove(key);
            }
        }
        command
            .args(["cluster", "create", "--config"])
            .arg(self.cluster_config_path())
            .args([
                "--kubeconfig-update-default=false",
                "--kubeconfig-switch-context=false",
            ])
            .env("KUBECONFIG", self.kubeconfig());
        command
    }

    fn write_runtime_config(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.cluster_config())?;
        let path = self.cluster_config_path();
        if path.try_exists()? {
            ensure!(
                fs::read(&path)? == bytes,
                "generated k3d configuration differs from saved installation; refusing to overwrite it"
            );
            return Ok(());
        }
        write_new_private(&path, &bytes)
    }
}

fn available_port(requested: Option<u16>) -> Result<TcpListener> {
    ensure!(
        requested != Some(0),
        "explicit ports must be nonzero; omit the port for automatic selection"
    );
    TcpListener::bind((Ipv4Addr::LOCALHOST, requested.unwrap_or(0)))
        .context("installation port is unavailable; choose another port")
}

fn pinned_k3s() -> &'static str {
    include_str!("../../../tools/versions.env")
        .lines()
        .find_map(|line| line.strip_prefix("K3S_VERSION="))
        .expect("checked-in tools/versions.env must pin K3S_VERSION")
}

fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests;
