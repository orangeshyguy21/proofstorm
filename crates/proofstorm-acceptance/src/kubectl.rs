//! Context-pinned `kubectl` wrapper.
//!
//! Gates assert post-conditions that MCP deliberately does not expose:
//! teardown receipts, residual instance namespaces, and controller restarts.
//! Everything here shells out to the version pinned in `tools/versions.env`,
//! preferring `.tools/bin` over whatever is on `PATH`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// The k3d context created by the cluster bootstrap.
pub const DEFAULT_CONTEXT: &str = "k3d-proofstorm";
/// Namespace holding the controller and its receipts.
pub const CONTROL_NAMESPACE: &str = "proofstorm-system";

pub struct Kubectl {
    binary: PathBuf,
    context: String,
}

impl Kubectl {
    /// Resolve the pinned binary under `root/.tools/bin`, falling back to `PATH`.
    pub fn pinned(root: &Path) -> Self {
        let pinned = root.join(".tools/bin/kubectl");
        Self {
            binary: if pinned.is_file() {
                pinned
            } else {
                PathBuf::from("kubectl")
            },
            context: DEFAULT_CONTEXT.to_string(),
        }
    }

    /// Run a command that must succeed, returning trimmed stdout.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let (success, stdout, stderr) = self.try_run(args)?;
        if !success {
            bail!("kubectl {} failed: {stderr}", args.join(" "));
        }
        Ok(stdout)
    }

    /// Build a context-pinned command without running it.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command.arg("--context").arg(&self.context).args(args);
        command
    }

    /// Run a command that is allowed to fail, returning success, stdout and stderr.
    pub fn try_run(&self, args: &[&str]) -> Result<(bool, String, String)> {
        let output = self
            .command(args)
            .output()
            .with_context(|| format!("run kubectl {}", args.join(" ")))?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }

    /// Run a program inside a workload, feeding it `input` on stdin.
    ///
    /// The in-container drivers read a JSON payload from stdin and print one
    /// JSON line, so this returns that last line.
    pub fn exec_stdin(
        &self,
        namespace: &str,
        target: &str,
        argv: &[&str],
        input: &str,
    ) -> Result<String> {
        use std::io::Write;
        let mut invocation = vec!["exec", "-i", target, "-n", namespace, "--"];
        invocation.extend_from_slice(argv);
        let mut child = self
            .command(&invocation)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("spawn kubectl exec")?;
        child
            .stdin
            .take()
            .context("kubectl stdin")?
            .write_all(input.as_bytes())
            .context("write driver payload")?;
        let output = child.wait_with_output().context("run kubectl exec")?;
        if !output.status.success() {
            bail!(
                "kubectl exec failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Decode a Secret's base64 `data` map into plain strings.
    pub fn secret_data(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let secret = self.get_json(&["get", &format!("secret/{name}"), "-n", namespace])?;
        let mut decoded = std::collections::BTreeMap::new();
        if let Some(data) = secret.get("data").and_then(serde_json::Value::as_object) {
            for (key, value) in data {
                let encoded = value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("secret {name} key {key} is not a string"))?;
                decoded.insert(key.clone(), crate::postgres::decode_base64(encoded)?);
            }
        }
        Ok(decoded)
    }

    /// Apply a manifest supplied on stdin, as `kubectl apply -f -`.
    pub fn apply_stdin(&self, manifest: &str) -> Result<String> {
        use std::io::Write;
        let mut child = self
            .command(&["apply", "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("spawn kubectl apply")?;
        child
            .stdin
            .take()
            .context("kubectl stdin")?
            .write_all(manifest.as_bytes())
            .context("write manifest")?;
        let output = child.wait_with_output().context("run kubectl apply")?;
        if !output.status.success() {
            bail!(
                "kubectl apply failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// SHA-256 of a command's raw stdout, for proving an object did not change.
    pub fn digest(&self, args: &[&str]) -> Result<String> {
        use sha2::{Digest, Sha256};
        let raw = self.run(args)?;
        Ok(format!("{:x}", Sha256::digest(raw.as_bytes())))
    }

    /// Run a command with `-o json` and parse the result.
    pub fn get_json(&self, args: &[&str]) -> Result<Value> {
        let mut full = args.to_vec();
        full.extend_from_slice(&["-o", "json"]);
        let stdout = self.run(&full)?;
        serde_json::from_str(&stdout).context("parse kubectl JSON output")
    }

    /// Assert no namespace carries the instance label, proving verified teardown.
    pub fn assert_no_instance_namespaces(&self) -> Result<()> {
        let names = self.run(&[
            "get",
            "namespaces",
            "-l",
            "proofstorm.dev/instance",
            "-o",
            "name",
        ])?;
        if !names.is_empty() {
            bail!("Proofstorm instance namespaces remain after close: {names}");
        }
        Ok(())
    }

    /// Assert no lab action resource survived close.
    pub fn assert_no_lab_actions(&self) -> Result<()> {
        let names = self.run(&[
            "get",
            "proofstormlabactions.proofstorm.dev",
            "-n",
            CONTROL_NAMESPACE,
            "-o",
            "name",
        ])?;
        if !names.is_empty() {
            bail!("ProofstormLabActions remain after verified close: {names}");
        }
        Ok(())
    }

    /// Assert the most recent teardown receipt recorded verified absence.
    pub fn assert_teardown_verified(&self) -> Result<()> {
        let verified = self.run(&[
            "get",
            "configmap",
            "-n",
            CONTROL_NAMESPACE,
            "-l",
            "proofstorm.dev/receipt=teardown",
            "-o",
            "jsonpath={.items[0].data.verifiedAbsent}",
        ])?;
        if verified != "true" {
            bail!("teardown receipt did not record verified absence: {verified:?}");
        }
        Ok(())
    }

    /// Run a program inside a workload's default container.
    ///
    /// `target` is a kubectl workload selector such as `deployment/mint`.
    pub fn exec(&self, namespace: &str, target: &str, argv: &[&str]) -> Result<String> {
        let mut invocation = vec!["exec", target, "-n", namespace, "--"];
        invocation.extend_from_slice(argv);
        self.run(&invocation)
    }

    /// Restart one workload and wait for its rollout to finish.
    pub fn rollout_restart(&self, namespace: &str, target: &str) -> Result<()> {
        self.run(&["rollout", "restart", target, "-n", namespace])?;
        self.run(&[
            "rollout",
            "status",
            target,
            "-n",
            namespace,
            "--timeout=180s",
        ])?;
        Ok(())
    }

    /// Scale the controller to zero and wait for its pod to disappear.
    pub fn stop_controller(&self) -> Result<()> {
        self.run(&[
            "scale",
            "deployment/proofstormd",
            "-n",
            CONTROL_NAMESPACE,
            "--replicas=0",
        ])?;
        self.run(&[
            "wait",
            "--for=delete",
            "pod",
            "-n",
            CONTROL_NAMESPACE,
            "-l",
            "app.kubernetes.io/name=proofstormd",
            "--timeout=90s",
        ])?;
        Ok(())
    }

    /// Scale the controller back to one replica and wait for the rollout.
    pub fn start_controller(&self) -> Result<()> {
        self.run(&[
            "scale",
            "deployment/proofstormd",
            "-n",
            CONTROL_NAMESPACE,
            "--replicas=1",
        ])?;
        self.run(&[
            "rollout",
            "status",
            "deployment/proofstormd",
            "-n",
            CONTROL_NAMESPACE,
            "--timeout=90s",
        ])?;
        Ok(())
    }

    /// UID of the current controller pod, for proving it was replaced.
    pub fn controller_pod_uid(&self) -> Result<String> {
        self.run(&[
            "get",
            "pod",
            "-n",
            CONTROL_NAMESPACE,
            "-l",
            "app.kubernetes.io/name=proofstormd",
            "-o",
            "jsonpath={.items[0].metadata.uid}",
        ])
    }

    /// Restart the controller and wait for the new generation to become available.
    pub fn restart_controller(&self) -> Result<()> {
        self.run(&[
            "rollout",
            "restart",
            "deployment/proofstormd",
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        self.run(&[
            "rollout",
            "status",
            "deployment/proofstormd",
            "-n",
            CONTROL_NAMESPACE,
            "--timeout=90s",
        ])?;
        Ok(())
    }
}
