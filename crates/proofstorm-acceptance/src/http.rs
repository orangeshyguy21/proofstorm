//! Minimal HTTP for gates that drive a mint's public API through a
//! port-forward.
//!
//! Shells out to `curl`, which the repository already requires on the host, so
//! the crate needs no HTTP dependency.

use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    thread::sleep,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::Kubectl;

/// A `kubectl port-forward` child that is killed when it goes out of scope.
pub struct PortForward {
    child: Child,
    port: u16,
}

impl PortForward {
    /// Forward a local ephemeral port to a service port inside the cluster.
    pub fn open(kubectl: &Kubectl, namespace: &str, service: &str, remote: u16) -> Result<Self> {
        let port = free_port()?;
        let child = kubectl
            .command(&[
                "port-forward",
                "-n",
                namespace,
                service,
                &format!("{port}:{remote}"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn kubectl port-forward")?;
        Ok(Self { child, port })
    }

    /// Base URL for requests through this forward.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Whether the forward is still alive.
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// GET a JSON document, retrying while the endpoint is still coming up.
pub fn get_json_retrying(forward: &mut PortForward, path: &str, attempts: u32) -> Result<Value> {
    let mut last = String::new();
    for _ in 0..attempts {
        if !forward.running() {
            bail!("port-forward stopped before the endpoint answered");
        }
        match get_json(&forward.url(path)) {
            Ok(value) => return Ok(value),
            Err(error) => last = error.to_string(),
        }
        sleep(Duration::from_secs(1));
    }
    bail!("endpoint {path} was not reachable through port-forward: {last}");
}

/// GET a JSON document.
pub fn get_json(url: &str) -> Result<Value> {
    let output = curl(&[
        "--silent",
        "--show-error",
        "--fail",
        "--max-time",
        "10",
        url,
    ])?;
    serde_json::from_str(&output).with_context(|| format!("parse JSON from {url}: {output}"))
}

/// POST a JSON body and parse the JSON response.
pub fn post_json(url: &str, payload: &Value) -> Result<Value> {
    let body = serde_json::to_string(payload)?;
    let output = curl(&[
        "--silent",
        "--show-error",
        "--fail",
        "--max-time",
        "10",
        "--header",
        "content-type: application/json",
        "--data",
        &body,
        url,
    ])?;
    serde_json::from_str(&output).with_context(|| format!("parse JSON from {url}: {output}"))
}

/// POST a JSON body and return only the HTTP status, for negative cases.
pub fn post_status(url: &str, payload: &Value) -> Result<u32> {
    let body = serde_json::to_string(payload)?;
    let output = curl(&[
        "--silent",
        "--output",
        "/dev/null",
        "--write-out",
        "%{http_code}",
        "--max-time",
        "10",
        "--header",
        "content-type: application/json",
        "--data",
        &body,
        url,
    ])?;
    output.trim().parse().context("parse the HTTP status code")
}

fn curl(args: &[&str]) -> Result<String> {
    let output = Command::new("curl")
        .args(args)
        .output()
        .context("run curl")?;
    if !output.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Bind an ephemeral port and release it, so kubectl can claim it next.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("reserve a local port")?;
    Ok(listener.local_addr()?.port())
}
