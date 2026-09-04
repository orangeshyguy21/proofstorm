//! Synchronous MCP stdio client.
//!
//! One newline-delimited JSON-RPC frame per line, matching the transport the
//! Python acceptance clients drove.

use std::{
    ffi::OsStr,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// MCP protocol revision the server implements.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// A spawned `proofstorm-mcp` process with an initialized MCP session.
///
/// The child is killed on drop, so a gate that aborts mid-way never leaks a
/// server process.
pub struct McpClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    initialize_result: Value,
}

impl McpClient {
    /// Spawn the server, perform `initialize`, and send `notifications/initialized`.
    pub fn spawn<K, V>(binary: &Path, client_name: &str, env: &[(K, V)]) -> Result<Self>
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut command = Command::new(binary);
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {}", binary.display()))?;
        let stdin = child.stdin.take().context("child stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("child stdout")?);

        let mut client = Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout,
            next_id: 0,
            initialize_result: Value::Null,
        };
        client.initialize_result = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": client_name, "version": "0.1.0"}
            }),
        )?;
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    /// Spawn with no extra environment, for the unconfigured default surface.
    pub fn spawn_bare(binary: &Path, client_name: &str) -> Result<Self> {
        Self::spawn::<&str, &str>(binary, client_name, &[])
    }

    /// The `initialize` result captured during [`McpClient::spawn`].
    pub fn initialize_result(&self) -> &Value {
        &self.initialize_result
    }

    /// Send one frame and read one response frame verbatim.
    ///
    /// Neither JSON-RPC errors nor `isError` tool results are interpreted; use
    /// this when a gate asserts on the raw envelope.
    pub fn exchange(&mut self, message: &Value) -> Result<Value> {
        self.send(message)?;
        self.receive()
    }

    /// Call a JSON-RPC method and return its `result`.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let response = self.response(method, params)?;
        if let Some(error) = response.get("error") {
            bail!("MCP {method} failed: {error}");
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP {method} returned no result: {response}"))
    }

    /// Call a JSON-RPC method that must fail, returning its `error` object.
    pub fn request_error(&mut self, method: &str, params: Value) -> Result<Value> {
        let response = self.response(method, params)?;
        response
            .get("error")
            .cloned()
            .ok_or_else(|| anyhow!("MCP {method} unexpectedly succeeded: {response}"))
    }

    /// Invoke a tool and parse its first text content block as JSON.
    ///
    /// This mirrors what every Python client did: `json.loads(result["content"][0]["text"])`.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        let result = self.request("tools/call", tool_params(tool, arguments))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!("tool {tool} failed: {result}");
        }
        tool_content(tool, &result)
    }

    /// Invoke a tool and return the whole JSON-RPC envelope.
    ///
    /// Neither JSON-RPC errors nor `isError` are interpreted, so gates can
    /// assert on `structuredContent` or on the encoded wire size.
    pub fn call_response(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        self.response("tools/call", tool_params(tool, arguments))
    }

    /// Invoke a tool that must fail, returning its parsed error payload.
    ///
    /// Accepts either a JSON-RPC error or an `isError` tool result, because the
    /// server uses both depending on whether the route or the handler refused.
    pub fn call_error(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        let response = self.response("tools/call", tool_params(tool, arguments))?;
        if let Some(error) = response.get("error") {
            return Ok(error.clone());
        }
        let result = response
            .get("result")
            .ok_or_else(|| anyhow!("tool {tool} returned neither result nor error: {response}"))?;
        if result.get("isError").and_then(Value::as_bool) != Some(true) {
            bail!("tool {tool} unexpectedly succeeded: {result}");
        }
        tool_content(tool, result)
    }

    /// Assert a tool refuses with a given error code.
    ///
    /// The code may appear in a JSON-RPC error or in an `isError` result, so
    /// this checks the whole response, matching the Python helper.
    pub fn call_refused(&mut self, tool: &str, arguments: Value, code: &str) -> Result<()> {
        let response = self.response("tools/call", tool_params(tool, arguments))?;
        let payload = response
            .get("error")
            .or_else(|| response.get("result"))
            .unwrap_or(&response);
        if !serde_json::to_string(payload)?.contains(code) {
            bail!("tool {tool} did not refuse with {code}: {response}");
        }
        Ok(())
    }

    /// Send a notification, which has no id and no response.
    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&frame([
            ("jsonrpc", Value::from("2.0")),
            ("method", Value::from(method)),
            ("params", params),
        ]))
    }

    /// Write raw bytes followed by a newline, for transport-level negative tests.
    pub fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin already closed")?;
        stdin.write_all(bytes).context("write raw frame")?;
        stdin.write_all(b"\n").context("terminate raw frame")?;
        stdin.flush().context("flush raw frame")
    }

    /// Assert the server closed the transport without answering.
    pub fn expect_transport_closed(&mut self) -> Result<()> {
        let mut response = String::new();
        let read = self
            .stdout
            .read_line(&mut response)
            .context("read transport EOF")?;
        if read != 0 {
            bail!("expected a closed transport, read: {response}");
        }
        Ok(())
    }

    /// Drop stdin so the server observes EOF.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Wait for exit. Call [`McpClient::close_stdin`] first for a clean shutdown.
    pub fn wait(mut self) -> Result<ExitStatus> {
        self.stdin = None;
        let mut child = self.child.take().context("child already reaped")?;
        child.wait().context("reap proofstorm-mcp")
    }

    /// Everything the server wrote to stderr, for failure diagnostics.
    pub fn stderr(&mut self) -> String {
        let Some(child) = self.child.as_mut() else {
            return String::new();
        };
        let Some(mut stderr) = child.stderr.take() else {
            return String::new();
        };
        let mut buffer = String::new();
        let _ = stderr.read_to_string(&mut buffer);
        buffer
    }

    /// Call a JSON-RPC method and return the whole envelope, id already checked.
    pub fn response(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let response = self.exchange(&frame([
            ("jsonrpc", Value::from("2.0")),
            ("id", Value::from(id)),
            ("method", Value::from(method)),
            ("params", params),
        ]))?;
        let observed = response.get("id").and_then(Value::as_u64);
        if observed != Some(id) {
            bail!("MCP response id {observed:?} does not match request {id}: {response}");
        }
        Ok(response)
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin already closed")?;
        writeln!(stdin, "{message}").context("write MCP frame")?;
        stdin.flush().context("flush MCP frame")
    }

    fn receive(&mut self) -> Result<Value> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line).context("read MCP frame")? == 0 {
            let stderr = self.stderr();
            bail!("MCP server closed the transport: {stderr}");
        }
        serde_json::from_str(&line).with_context(|| format!("parse MCP frame: {line}"))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Build a JSON object, moving each value in rather than cloning it.
///
/// `json!` serializes an interpolated `Value` by reference, which would deep
/// copy every lab document a gate submits.
fn frame<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn tool_params(tool: &str, arguments: Value) -> Value {
    frame([("name", Value::from(tool)), ("arguments", arguments)])
}

fn tool_content(tool: &str, result: &Value) -> Result<Value> {
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("tool {tool} returned no text content: {result}"))?;
    serde_json::from_str(text).with_context(|| format!("parse {tool} content as JSON: {text}"))
}
