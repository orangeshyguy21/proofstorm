//! Shared per-gate execution context.

use std::path::{Path, PathBuf};

use anyhow::Result;
use proofstorm_app::{artifacts::TestArtifacts, installation::Installation};

use crate::{McpClient, kubectl::Kubectl};

/// Control-plane namespace every live gate drives.
pub const CONTROL_NAMESPACE: &str = "proofstorm-system";

/// Everything a gate needs: where the server is, a private database, and a
/// kubectl bound to the cell cluster.
pub struct GateContext {
    pub root: PathBuf,
    pub kubectl: Kubectl,
    pub run_id: String,
    pub installation: Installation,
    pub(crate) artifacts: TestArtifacts,
    database: PathBuf,
}

impl GateContext {
    /// Ordinary product CLI, pinned to this owned run and its verified artifacts.
    pub fn command(&self, args: &[&str]) -> Result<std::process::Command> {
        self.artifacts.verify_for(&self.installation.home)?;
        let mut command = std::process::Command::new(&self.artifacts.cli);
        crate::client::clear_runtime_environment(&mut command);
        command
            .arg("--home")
            .arg(&self.installation.home)
            .args(args);
        command.current_dir(self.installation.home.parent().expect("run directory"));
        Ok(command)
    }

    pub fn cli(&self, args: &[&str]) -> Result<serde_json::Value> {
        let mut command = self.command(&["--json"])?;
        command.args(args);
        crate::process::json(command, 5400)
    }

    pub fn work(&self) -> &Path {
        self.installation.home.parent().expect("run directory")
    }

    pub fn record(&self, name: &str, value: &serde_json::Value) -> Result<()> {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(self.work())?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.persist(self.work().join(name))?;
        Ok(())
    }
    /// Bind a gate to a verified installation. Never consult an ambient home,
    /// database, test identity, helper binary or Kubernetes context.
    pub fn new(root: &Path, installation: Installation, artifacts: TestArtifacts) -> Result<Self> {
        artifacts.verify_for(&installation.home)?;
        proofstorm_app::bootstrap::verify_runtime_identity(&installation)?;
        // The run already owns a fresh home. Use its ordinary database so CLI
        // and MCP share the same state; actor/workspace scopes isolate gates.
        let database = installation.database();
        let run_id = installation.id.clone();
        Ok(Self {
            root: root.to_path_buf(),
            kubectl: Kubectl::for_installation(&installation)?,
            run_id,
            installation,
            artifacts,
            database,
        })
    }

    /// Configure and open the ordinary project attachment, without launching an agent model.
    pub fn managed_session(&self, label: &str) -> Result<McpClient> {
        use anyhow::Context;
        let project = self.work().join(format!("mcp-{label}"));
        std::fs::create_dir_all(&project)?;
        let mut configure = self.command(&[
            "--json",
            "agent",
            "configure",
            "codex",
            "--allow-development",
        ])?;
        configure
            .current_dir(&project)
            .env("CODEX_HOME", self.work().join("acceptance-codex"))
            .env("CLAUDE_CONFIG_DIR", self.work().join("acceptance-claude"));
        let configured = crate::process::json(configure, 240)?;
        let actor = configured["actor"]
            .as_str()
            .context("managed actor missing")?;
        let mut command = std::process::Command::new(&self.artifacts.mcp);
        crate::client::clear_runtime_environment(&mut command);
        command
            .arg("--home")
            .arg(&self.installation.home)
            .arg("--attachment")
            .arg(actor)
            .current_dir(&project);
        McpClient::from_command(command, label)
    }

    /// An explicitly scoped authorization fixture using the single registry's default grants.
    pub fn default_session(&self, workspace: &str, principal: &str) -> Result<McpClient> {
        let capabilities: Vec<String> = proofstorm_core::mcp::default_capabilities()
            .into_iter()
            .map(|c| serde_json::from_value(serde_json::json!(c)).expect("capability string"))
            .collect();
        self.session(
            workspace,
            principal,
            &capabilities.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }

    /// Start a capability-scoped MCP session against this gate's database.
    pub fn session(
        &self,
        workspace: &str,
        principal: &str,
        capabilities: &[&str],
    ) -> Result<McpClient> {
        self.artifacts.verify_for(&self.installation.home)?;
        proofstorm_app::bootstrap::verify_runtime_identity(&self.installation)?;
        let home = self.installation.home.to_string_lossy().to_string();
        let joined = capabilities.join(",");
        McpClient::spawn(
            &self.artifacts.mcp,
            workspace,
            &[
                ("PROOFSTORM_HOME", home.as_str()),
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

    /// Read the same installation state through the normal CLI, using existing
    /// actor grants rather than passing startup capabilities to the process.
    pub fn inspect_cli(
        &self,
        workspace: &str,
        principal: &str,
        cell: &str,
    ) -> Result<serde_json::Value> {
        self.artifacts
            .inspect_cli(&self.installation.home, workspace, principal, cell)
    }
}
