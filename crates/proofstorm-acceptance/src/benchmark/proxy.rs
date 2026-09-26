//! MCP boundary capture; independent observations are never supplied by the model.
use super::{Context, append, observer, read, save};
use crate::McpClient;
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, Read, Write},
    path::Path,
    time::Instant,
};

pub fn client(config: &Context, label: &str) -> Result<McpClient> {
    let grants: Vec<String> = proofstorm_core::mcp::default_capabilities()
        .into_iter()
        .map(|c| serde_json::from_value(json!(c)).expect("capability string"))
        .collect();
    McpClient::spawn(
        &config.mcp,
        label,
        &[
            (
                "PROOFSTORM_HOME".to_owned(),
                config.home.to_string_lossy().into_owned(),
            ),
            ("PROOFSTORM_WORKSPACE".into(), "benchmark".into()),
            ("PROOFSTORM_PRINCIPAL".into(), "benchmark".into()),
            ("PROOFSTORM_CAPABILITIES".into(), grants.join(",")),
            (
                "PROOFSTORM_CONTROL_NAMESPACE".into(),
                "proofstorm-system".into(),
            ),
        ],
    )
}

pub fn serve(path: &Path) -> Result<()> {
    let config: Context = serde_json::from_value(read(path)?)?;
    // Fail closed on concurrent/reconnected adapters: no overwritten capture or ambiguous IDs.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(config.work.join("proxy.started"))?;
    let mut upstream = client(&config, "benchmark-harness")?;
    let mut observer = client(&config, "benchmark-observer")?;
    save(
        &config.work.join("proxy-ready.json"),
        &json!({"home":config.home,"mcp":config.mcp}),
    )?;
    let mut id = 0_u64;
    let mut input = std::io::stdin().lock();
    loop {
        let mut line = String::new();
        let n = (&mut input)
            .take(8 * 1024 * 1024 + 1)
            .read_line(&mut line)?;
        if n == 0 {
            break;
        }
        ensure!(n <= 8 * 1024 * 1024, "MCP input too large");
        let frame: Value = serde_json::from_str(&line)?;
        let Some(request_id) = frame.get("id") else {
            continue;
        };
        let response = match frame["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({"jsonrpc":"2.0","id":request_id,"result":upstream.initialize_result()})
            }
            "ping" => json!({"jsonrpc":"2.0","id":request_id,"result":{}}),
            "tools/list" => {
                let mut listed = upstream.request("tools/list", json!({}))?;
                let tools = listed["tools"]
                    .as_array_mut()
                    .context("tool discovery missing")?;
                tools.retain(|tool| {
                    tool["name"]
                        .as_str()
                        .is_some_and(|name| config.task.allowed(name))
                });
                tools.push(json!({"name":"benchmark_checkpoint","description":format!("Retain independent {} payment observations BEFORE cleanup. Call funded after minting {} sat, then paid after melting {} sat. Each successful checkpoint is immutable. This is a report submission, not a payment tool.",config.task.id,config.task.amounts.mint_sat,config.task.amounts.melt_sat),"inputSchema":{
                    "type":"object","additionalProperties":false,"properties":{
                        "stage":{"type":"string","enum":["funded","paid"]},"mint_quote_id":{"type":"string"},
                        "melt_quote_id":{"type":"string"},"payment_hash":{"type":"string"},
                        "minted_sat":{"type":"integer"},"paid_sat":{"type":"integer"},"remaining_sat":{"type":"integer"}},
                    "required":["stage","mint_quote_id"]}}));
                save(&config.work.join("tools.json"), &listed)?;
                json!({"jsonrpc":"2.0","id":request_id,"result":listed})
            }
            "tools/call" => {
                id += 1;
                let tool = frame["params"]["name"].as_str().unwrap_or("");
                let args = &frame["params"]["arguments"];
                let start = Instant::now();
                append(
                    &config.work.join("events.jsonl"),
                    &json!({"kind":"start","id":id,"tool":tool,"arguments":args,"at_unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()}),
                )?;
                let result = (|| -> Result<Value> {
                    ensure!(config.task.allowed(tool), "tool outside benchmark profile");
                    if let Some(name) = args.get("name") {
                        ensure!(
                            name.as_str() == Some(config.task.cell_name.as_str()),
                            "cell outside benchmark scope"
                        );
                    }
                    if matches!(tool, "cell_plan" | "cell_up" | "cell_exec" | "cell_remove") {
                        ensure!(args["name"] == config.task.cell_name, "cell name required");
                    }
                    if tool == "benchmark_checkpoint" {
                        let observed = observer::checkpoint(&config, &mut observer, args)?;
                        Ok(
                            json!({"jsonrpc":"2.0","id":request_id,"result":{"content":[{"type":"text","text":serde_json::to_string(&observed)?}],"structuredContent":observed}}),
                        )
                    } else {
                        if tool == "cell_remove"
                            && let Err(error) = observer::retain_terminal(&config, &mut observer)
                        {
                            save(
                                &config.work.join("terminal-observation.json"),
                                &json!({"verified":false,"error":format!("{error:#}")}),
                            )?;
                        }
                        upstream.exchange(&frame)
                    }
                })();
                let reply=result.unwrap_or_else(|error|json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32000,"message":format!("{error:#}")}}));
                let success = reply.get("error").is_none()
                    && reply["result"]["isError"] != true
                    && reply.get("result").is_some();
                append(
                    &config.work.join("events.jsonl"),
                    &json!({"kind":"end","id":id,"tool":tool,"arguments":args,"success":success,"elapsed_ms":start.elapsed().as_millis(),"response":reply}),
                )?;
                reply
            }
            _ => {
                json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"method outside benchmark profile"}})
            }
        };
        serde_json::to_writer(std::io::stdout().lock(), &response)?;
        println!();
        std::io::stdout().flush()?;
    }
    Ok(())
}
