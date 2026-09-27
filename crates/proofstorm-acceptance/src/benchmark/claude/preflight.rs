//! Model-free preflight. Claude Code runs the exact attempt configuration with a
//! placeholder key against an owned loopback endpoint that refuses every request.
//! The real credential is never used and no model request leaves the host.
use super::{Context, arguments, command, expected_tools, json_lines, write_mcp_config};
use crate::benchmark::{read, save};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::DirBuilderExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const PLACEHOLDER_KEY: &str = "proofstorm-preflight-not-a-key";
const MAX_BODY: usize = 8 * 1024 * 1024;

pub(super) fn run(context: &Context) -> Result<Value> {
    // A separate proxy identity keeps discovery out of the scored trace.
    let directory = context.work.join("transport-preflight");
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut preflight_context = json!(context);
    preflight_context["work"] = json!(directory);
    let path = directory.join("context.json");
    save(&path, &preflight_context)?;
    let mcp_config = directory.join("mcp.json");
    write_mcp_config(&path, &mcp_config)?;
    let responder = Responder::start()?;
    let mut probe = command(context);
    probe
        .args(arguments(context, &mcp_config))
        .env("ANTHROPIC_API_KEY", PLACEHOLDER_KEY)
        .env("ANTHROPIC_BASE_URL", responder.url());
    let output = crate::process::capture(probe, 90);
    let requests = responder.finish()?;
    let output = output?;
    let (rows, _) = json_lines(&String::from_utf8_lossy(&output.stdout));
    save(
        &context.work.join("claude-preflight.private.json"),
        &json!({"stdout":rows,"stderr":String::from_utf8_lossy(&output.stderr),
            "requests":requests,"model_request_made":false,"placeholder_credential":true}),
    )?;
    let profile = audit(context, &rows, &requests)?;
    ensure!(
        directory.join("proxy-ready.json").exists(),
        "Claude Code did not connect to the owned benchmark proxy; model not started"
    );
    let tools = read(&directory.join("tools.json"))?;
    ensure!(
        tools["tools"]
            .as_array()
            .is_some_and(|tools| tools.len() == context.task.allowed_tools.len()),
        "benchmark proxy tools missing"
    );
    save(
        &context.work.join("harness-preflight.private.txt"),
        &json!({"proxy_ready":read(&directory.join("proxy-ready.json"))?,
            "tools_sha256":proofstorm_core::digest_json(&tools),"profile":profile,"model_request_made":false}),
    )?;
    Ok(profile)
}

/// Check the resolved session and the request Claude Code would have sent.
pub(super) fn audit(context: &Context, rows: &[Value], requests: &[Value]) -> Result<Value> {
    let expected = expected_tools(&context.task);
    let init = rows
        .iter()
        .find(|row| row["type"] == "system" && row["subtype"] == "init")
        .context("Claude Code produced no session initialization; model not started")?;
    let listed: BTreeSet<String> = init["tools"]
        .as_array()
        .context("session tool list missing")?
        .iter()
        .filter_map(|tool| tool.as_str().map(str::to_owned))
        .collect();
    ensure!(
        listed == expected,
        "Claude Code tool surface differs from the benchmark profile; model not started"
    );
    let servers = init["mcp_servers"]
        .as_array()
        .context("MCP server list missing")?;
    ensure!(
        servers.len() == 1
            && servers[0]["name"] == super::SERVER
            && servers[0]["status"] == "connected",
        "additional or disconnected Claude Code MCP servers; model not started"
    );
    ensure!(
        init["permissionMode"] == "dontAsk"
            && init["model"] == context.model.as_str()
            && init["apiKeySource"] == "ANTHROPIC_API_KEY",
        "Claude Code resolved a different permission mode, model or credential source; model not started"
    );
    ensure!(
        ["skills", "slash_commands"]
            .iter()
            .all(|key| init[*key].as_array().is_none_or(Vec::is_empty))
            && init["plugins"]
                .as_array()
                .is_none_or(|plugins| plugins.iter().all(|p| p["path"] == "builtin")),
        "personal skills, commands or plugins loaded; model not started"
    );
    let messages: Vec<_> = requests
        .iter()
        .filter(|r| {
            r["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("/v1/messages"))
        })
        .collect();
    ensure!(
        !messages.is_empty(),
        "Claude Code did not reach the preflight endpoint"
    );
    for request in &messages {
        let body = &request["body"];
        let tools: BTreeSet<String> = body["tools"]
            .as_array()
            .context("request tools missing")?
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        ensure!(
            body["model"] == context.model.as_str() && tools == expected,
            "Claude Code request model or tools differ from the benchmark profile"
        );
        ensure!(
            prompt_delivered(body, &context.task.prompt),
            "task prompt was not delivered intact"
        );
    }
    let first = &messages[0]["body"];
    Ok(json!({
        "claude_code_version":init["claude_code_version"],"model":init["model"],
        "permission_mode":init["permissionMode"],"tools":listed,
        "plugins":init["plugins"],"agents":init["agents"],
        "effort":first["output_config"],"thinking":first["thinking"],"max_tokens":first["max_tokens"],
        "system_prompt_sha256":proofstorm_core::digest_json(&first["system"]),
        "tool_definitions_sha256":proofstorm_core::digest_json(&first["tools"]),
        "headers":messages[0]["headers"],"request_paths":requests.iter().map(|r| &r["path"]).collect::<Vec<_>>(),
    }))
}

fn prompt_delivered(body: &Value, prompt: &str) -> bool {
    let prompt = prompt.trim();
    body["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "user"
                && match &message["content"] {
                    Value::String(text) => text.trim() == prompt,
                    Value::Array(blocks) => blocks.iter().any(|b| {
                        b["type"] == "text" && b["text"].as_str().map(str::trim) == Some(prompt)
                    }),
                    _ => false,
                }
        })
    })
}

/// Loopback endpoint that records each request and refuses it without retry.
pub(super) struct Responder {
    url: String,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<Vec<Value>>>,
}

impl Responder {
    pub(super) fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || -> Result<Vec<Value>> {
            let mut requests = Vec::new();
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => requests.push(respond(stream)?),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => return Err(e.into()),
                }
                ensure!(requests.len() <= 32, "unexpected preflight request volume");
            }
            Ok(requests)
        });
        Ok(Self { url, stop, handle })
    }

    pub(super) fn url(&self) -> &str {
        &self.url
    }

    pub(super) fn finish(self) -> Result<Vec<Value>> {
        self.stop.store(true, Ordering::SeqCst);
        self.handle
            .join()
            .map_err(|_| anyhow::anyhow!("preflight endpoint panicked"))?
    }
}

/// Credentials are never recorded: only allowlisted headers are retained.
fn respond(stream: TcpStream) -> Result<Value> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut length = 0;
    let mut headers = serde_json::Map::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name == "content-length" {
                length = value.trim().parse::<usize>()?;
            }
            if matches!(
                name.as_str(),
                "user-agent" | "anthropic-beta" | "anthropic-version"
            ) {
                headers.insert(name, json!(value.trim()));
            }
        }
    }
    ensure!(length <= MAX_BODY, "preflight request too large");
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let reply = json!({"type":"error","error":{"type":"invalid_request_error",
        "message":"Proofstorm preflight endpoint: no model request is served"}})
    .to_string();
    write!(
        reader.get_mut(),
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )?;
    reader.get_mut().flush()?;
    Ok(json!({"method":method,"path":path,"headers":headers,
        "body":serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null)}))
}
