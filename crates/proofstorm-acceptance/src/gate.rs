//! Shared per-gate execution context.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use tempfile::TempDir;

use crate::{McpClient, kubectl::Kubectl};

/// Control-plane namespace every live gate drives.
pub const CONTROL_NAMESPACE: &str = "proofstorm-system";

/// Everything a gate needs: where the server is, a private database, and a
/// kubectl bound to the lab cluster.
pub struct GateContext {
    pub root: PathBuf,
    pub mcp_binary: PathBuf,
    pub kubectl: Kubectl,
    pub run_id: String,
    database: PathBuf,
    _database_dir: TempDir,
}

impl GateContext {
    /// Resolve the repository root, the debug MCP binary, and a fresh database.
    ///
    /// `PROOFSTORM_TEST_RUN_ID` is honoured when the caller set one, so a gate
    /// keeps the unique identity its shell wrapper used to supply.
    pub fn new(root: &Path, mcp_binary: Option<PathBuf>) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("proofstorm-gate-")
            .tempdir()
            .context("create gate database directory")?;
        let database = directory.path().join("proofstorm.sqlite3");
        let run_id = std::env::var("PROOFSTORM_TEST_RUN_ID").unwrap_or_else(|_| {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            format!("{seconds}-{}", std::process::id())
        });
        Ok(Self {
            root: root.to_path_buf(),
            mcp_binary: mcp_binary.unwrap_or_else(|| root.join("target/debug/proofstorm-mcp")),
            kubectl: Kubectl::pinned(root),
            run_id,
            database,
            _database_dir: directory,
        })
    }

    /// Start a capability-scoped MCP session against this gate's database.
    pub fn session(
        &self,
        workspace: &str,
        principal: &str,
        capabilities: &[&str],
    ) -> Result<McpClient> {
        let database = self.database.to_string_lossy().to_string();
        let joined = capabilities.join(",");
        McpClient::spawn(
            &self.mcp_binary,
            workspace,
            &[
                ("PROOFSTORM_DB", database.as_str()),
                ("PROOFSTORM_WORKSPACE", workspace),
                ("PROOFSTORM_PRINCIPAL", principal),
                ("PROOFSTORM_CAPABILITIES", joined.as_str()),
                ("PROOFSTORM_CONTROL_NAMESPACE", CONTROL_NAMESPACE),
            ],
        )
    }

    /// Path to this gate's private `SQLite` database.
    pub fn database(&self) -> &Path {
        &self.database
    }
}

/// The capability set an experiment-driving gate needs on top of the lifecycle.
pub const EXPERIMENT_CAPABILITIES: &[&str] = &[
    "catalog.read",
    "lab.read",
    "lab.create",
    "lab.validate",
    "lab.publish",
    "lab.materialize",
    "lab.status",
    "lab.close",
    "experiment.create",
    "experiment.read",
    "experiment.close",
    "lease.acquire",
    "lease.release",
    "wallet.create",
    "wallet.control",
    "wallet.fund",
    "chain.mine",
    "peer.connect",
    "channel.open",
    "oracle.run",
    "artifact.read",
];

/// The capability set a full lifecycle gate needs.
pub const LIFECYCLE_CAPABILITIES: &[&str] = &[
    "catalog.read",
    "lab.read",
    "lab.create",
    "lab.validate",
    "lab.publish",
    "lab.materialize",
    "lab.status",
    "lab.close",
];
