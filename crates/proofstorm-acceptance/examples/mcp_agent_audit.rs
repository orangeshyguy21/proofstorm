//! Offline discovery/planner measurements against an explicitly selected MCP binary.
//! Uses a private database and never materializes a cell or contacts Kubernetes.
use anyhow::{Context, Result, ensure};
use proofstorm_acceptance::{client::clear_runtime_environment, process};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    time::timeout,
};

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    sequence: u64,
}

impl Client {
    fn spawn(binary: &Path, directory: &Path, database: &Path) -> Result<Self> {
        let mut command = std::process::Command::new(binary);
        clear_runtime_environment(&mut command);
        command
            .current_dir(directory)
            .envs([
                ("PROOFSTORM_MODE", "offline"),
                ("PROOFSTORM_WORKSPACE", "audit"),
                ("PROOFSTORM_PRINCIPAL", "audit"),
                ("PROOFSTORM_TOOLSET", "all"),
                (
                    "PROOFSTORM_CAPABILITIES",
                    "catalog.read,cell.create,cell.read,cell.validate",
                ),
            ])
            .env("PROOFSTORM_DB", database);
        let mut child = tokio::process::Command::from(command)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(tempfile::tempfile()?)
            .spawn()?;
        Ok(Self {
            stdin: Some(child.stdin.take().context("MCP stdin")?),
            stdout: BufReader::new(child.stdout.take().context("MCP stdout")?),
            child,
            sequence: 0,
        })
    }

    async fn send(&mut self, frame: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        let input = self.stdin.as_mut().context("MCP stdin closed")?;
        input.write_all(&bytes).await?;
        input.flush().await?;
        Ok(())
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        timeout(Duration::from_secs(30), async {
            self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
                .await?;
            loop {
                let mut line = Vec::new();
                let count = (&mut self.stdout)
                    .take(8 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut line)
                    .await?;
                ensure!(count > 0, "MCP transport closed during {method}");
                ensure!(
                    count <= 8 * 1024 * 1024,
                    "MCP response exceeds audit capture limit"
                );
                let response: Value = serde_json::from_slice(&line)?;
                if response.get("id") == Some(&json!(id)) {
                    return Ok(response);
                }
                ensure!(response.get("id").is_none(), "unexpected MCP response id");
            }
        })
        .await
        .with_context(|| format!("MCP {method} timed out"))?
    }

    async fn tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.call("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }

    async fn close(&mut self) -> Result<()> {
        self.stdin.take();
        if let Ok(status) = timeout(Duration::from_secs(5), self.child.wait()).await {
            status?;
        } else {
            self.child.kill().await?;
            self.child.wait().await?;
        }
        Ok(())
    }
}

fn bytes(value: &Value) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len())
}

fn field<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value> {
    value
        .pointer(pointer)
        .with_context(|| format!("missing audit response field {pointer}"))
}

async fn audit(client: &mut Client, report: &mut Value) -> Result<()> {
    let initialized = client
        .call(
            "initialize",
            json!({
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"proofstorm-offline-audit","version":"1"}
            }),
        )
        .await?;
    field(&initialized, "/result")?;
    timeout(
        Duration::from_secs(30),
        client.send(json!({
            "jsonrpc":"2.0","method":"notifications/initialized","params":{}
        })),
    )
    .await??;
    let listed = client.call("tools/list", json!({})).await?;
    let tools = field(&listed, "/result/tools")?
        .as_array()
        .context("tools array")?;
    report["offline_discovery"] = json!({
        "tool_names":tools.iter().map(|tool| field(tool,"/name").cloned()).collect::<Result<Vec<_>>>()?,
        "wire_bytes":bytes(&listed)?
    });
    let schema = tools
        .iter()
        .find(|tool| tool.get("name") == Some(&json!("cell_plan")))
        .context("cell_plan discovery")?;
    report["planner_input_fields"] = json!(
        field(schema, "/inputSchema/properties")?
            .as_object()
            .context("planner properties")?
            .keys()
            .collect::<Vec<_>>()
    );
    let catalog = client
        .tool("catalog_list", json!({"implementations":["bitcoin-core"]}))
        .await?;
    let version = field(&catalog, "/result/structuredContent/items/0/version")?
        .as_str()
        .context("catalog version")?;
    report["catalog_version"] = json!(version);
    let mut plans = Vec::new();
    for count in [1, 8, 16, 32, 64] {
        let id = format!("audit-{count}");
        let arguments = json!({
            "plan_id":id,"idempotency_key":id,"connections":[],"runtime_requirements":[],
            "components":(0..count).map(|index| json!({
                "id":format!("chain-{index}"),"implementation":"bitcoin-core","version":version
            })).collect::<Vec<_>>()
        });
        let planned = client.tool("cell_plan", arguments.clone()).await?;
        let read = client.tool("cell_read", json!({"draft_id":id})).await?;
        let stored = read
            .pointer("/result/structuredContent/cell")
            .filter(|cell| cell.is_object());
        let mut row = json!({
            "components":count,"request_argument_bytes":bytes(&arguments)?,
            "plan_wire_bytes":bytes(&planned)?,"error":planned.get("error"),
            "stored_draft_readable":stored.is_some(),
            "stored_component_count":stored.map(|cell| field(cell,"/components")?.as_array()
                .map(Vec::len).context("stored components array")).transpose()?,
            "read_wire_bytes":bytes(&read)?
        });
        if planned.get("result").is_some() {
            row["plan_structured_bytes"] =
                json!(bytes(field(&planned, "/result/structuredContent")?)?);
        }
        if planned.get("error").is_some() && stored.is_some() {
            row["exact_retry_error"] = client
                .tool("cell_plan", arguments)
                .await?
                .get("error")
                .cloned()
                .unwrap_or(Value::Null);
        }
        plans.push(row);
    }
    report["plans"] = json!(plans);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let binary = std::path::PathBuf::from(
        args.next()
            .context("expected trusted proofstorm-mcp binary path")?,
    )
    .canonicalize()?;
    ensure!(
        args.next().is_none(),
        "expected only a proofstorm-mcp binary path"
    );
    let mut command = std::process::Command::new(&binary);
    clear_runtime_environment(&mut command);
    command.arg("--release-info");
    let release = process::json(command, 10)?;
    let mut revision = std::process::Command::new("git");
    revision
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let revision = process::capture(revision, 10)?;
    ensure!(revision.status.success(), "could not read source revision");
    let mut report = json!({
        "binary_sha256":format!("{:x}",Sha256::digest(std::fs::read(&binary)?)),
        "binary_embedded_source_revision":release.get("source_revision"),
        "source_revision":String::from_utf8(revision.stdout)?.trim(),
        "scope":"offline planner and discovery; fresh temporary database; no runtime calls"
    });
    let directory = tempfile::Builder::new()
        .prefix("proofstorm-mcp-audit-")
        .tempdir()?;
    let database = directory.path().join("audit.sqlite3");
    let mut client = Client::spawn(&binary, directory.path(), &database)?;
    let result = audit(&mut client, &mut report).await;
    let closed = client.close().await;
    result?;
    closed?;
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    report["persisted_plan_count"] = json!(connection.query_row(
        "SELECT COUNT(*) FROM drafts WHERE workspace_id='audit'",
        [],
        |row| row.get::<_, i64>(0)
    )?);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
