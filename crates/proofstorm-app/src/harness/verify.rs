use anyhow::{Context, Result, ensure};
use rmcp::{ServiceExt, model::CallToolRequestParams};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};

/// Exercise the proposed command/arguments/cwd, never arbitrary project config.
pub(super) async fn server(entry: &Value) -> Result<Value> {
    let mut command =
        tokio::process::Command::new(entry["command"].as_str().context("MCP command")?);
    let args = entry["args"]
        .as_array()
        .context("MCP args")?
        .iter()
        .map(|arg| arg.as_str().context("MCP argument"))
        .collect::<Result<Vec<_>>>()?;
    command
        .args(args)
        .current_dir(entry["cwd"].as_str().context("MCP cwd")?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().context("start managed MCP server")?;
    let output = child.stdout.take().context("MCP stdout")?;
    let input = child.stdin.take().context("MCP stdin")?;
    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        let client = ().serve((output, input)).await?;
        let result: Result<Value> = async {
            ensure!(client.peer_info().is_some_and(|info| info.server_info.as_ref().is_some_and(|server| server.name == "proofstorm-mcp")), "unexpected MCP server identity");
            let tools = client.list_all_tools().await?;
            for required in ["environment_read", "lab_up", "lab_inspect"] {
                ensure!(tools.iter().any(|tool| tool.name == required), "required tool {required} unavailable; review this actor's grants (not automatically restored)");
            }
            let environment = client.call_tool(CallToolRequestParams::new("environment_read").with_arguments(serde_json::Map::new())).await?;
            ensure!(environment.is_error != Some(true), "read-only environment call failed");
            Ok(json!({"initialize":true,"tools_list":true,"environment_read":true,"tool_count":tools.len()}))
        }.await;
        client.cancel().await?;
        result
    }).await.context("MCP verification timed out")?;
    // Kill only our verification child, including on protocol failure.
    let _ = child.kill().await;
    let _ = child.wait().await;
    outcome
}
