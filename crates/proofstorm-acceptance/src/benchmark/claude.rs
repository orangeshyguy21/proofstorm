//! Claude Code print-mode adapter. The only tools are the benchmark's Proofstorm
//! MCP tools and no personal settings load. Login mode reuses the machine's
//! Claude Code login; environment mode isolates home and config. No model
//! substitution.
mod preflight;
mod telemetry;
#[cfg(test)]
mod tests;

use super::{
    Context,
    harness::{AttemptOutput, ClaudeAuth, Harness},
    save,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const SERVER: &str = "proofstorm";
/// Exactly one is taken from the runner's environment and passed explicitly.
const CREDENTIALS: [&str; 2] = ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];
/// Everything else, including the host's ANTHROPIC_*/CLAUDE_* settings, is dropped.
const INHERITED: [&str; 8] = [
    "PATH", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "USER", "LOGNAME",
];

pub(super) fn tool_name(name: &str) -> String {
    format!("mcp__{SERVER}__{name}")
}

pub(super) fn command(context: &Context) -> Command {
    let Harness::ClaudeCode { executable, auth } = &context.harness else {
        unreachable!("Claude Code adapter")
    };
    let mut command = Command::new(executable);
    command.env_clear();
    for key in INHERITED {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    match auth {
        // The normal config directory supplies the login; `--setting-sources ""`
        // still keeps personal settings, hooks and plugins out.
        ClaudeAuth::Login => {
            if let Some(home) = std::env::var_os("HOME") {
                command.env("HOME", home);
            }
        }
        // An owned HOME keeps caches and MCP logs out of the user's home; the
        // owned config directory replaces ~/.claude and ~/.claude.json.
        ClaudeAuth::Environment => {
            let home = context.work.join("claude-home");
            command
                .env("CLAUDE_CONFIG_DIR", home.join("config"))
                .env("HOME", home);
        }
    }
    command
        .current_dir(context.work.join("agent"))
        .env("PWD", context.work.join("agent"))
        .env("DISABLE_AUTOUPDATER", "1")
        .env("DISABLE_TELEMETRY", "1")
        .env("DISABLE_ERROR_REPORTING", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        // Auto-memory instructs the model to use file tools the profile removes.
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("MCP_TIMEOUT", "30000")
        .env("MCP_TOOL_TIMEOUT", "120000");
    command
}

/// Print-mode arguments shared by preflight and the scored attempt. The prompt
/// follows `--` because several options are variadic.
pub(super) fn arguments(context: &Context, mcp_config: &Path) -> Vec<OsString> {
    let allowed: Vec<_> = context
        .task
        .allowed_tools
        .iter()
        .map(|name| tool_name(name))
        .collect();
    [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
    ]
    .iter()
    .map(OsString::from)
    .chain([mcp_config.as_os_str().to_owned()])
    .chain(
        [
            "--tools",
            "",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--disable-slash-commands",
            "--no-chrome",
            "--model",
            &context.model,
            "--allowedTools",
            &allowed.join(","),
            "--",
            &context.task.prompt,
        ]
        .iter()
        .map(OsString::from),
    )
    .collect()
}

/// Claude passes its whole environment to stdio MCP servers. Restore the
/// runner's HOME for the proxy and blank the model credentials there.
pub(super) fn write_mcp_config(proxy_context: &Path, target: &Path) -> Result<()> {
    let mut env = serde_json::Map::new();
    if let Some(home) = std::env::var_os("HOME") {
        env.insert("HOME".into(), json!(home.to_str().context("HOME path")?));
    }
    for key in CREDENTIALS.iter().chain(&["ANTHROPIC_BASE_URL"]) {
        env.insert((*key).into(), json!(""));
    }
    let config = json!({"mcpServers":{SERVER:{
        "type":"stdio",
        "command":std::env::current_exe()?,
        "args":["--benchmark-proxy",proxy_context],
        "env":env,
    }}});
    save(target, &config)
}

/// The credential kind is recorded; its value is never written to disk.
pub(super) fn credential(
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<(&'static str, OsString)> {
    let mut present = CREDENTIALS
        .into_iter()
        .filter_map(|key| lookup(key).filter(|v| !v.is_empty()).map(|v| (key, v)));
    let first = present.next();
    ensure!(
        first.is_some() && present.next().is_none(),
        "set exactly one of ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN for Claude Code; no ambient login or keychain is used"
    );
    Ok(first.expect("checked"))
}

pub fn run(context: &Context) -> Result<()> {
    let Harness::ClaudeCode { auth, .. } = &context.harness else {
        unreachable!("Claude Code adapter")
    };
    let credential = match auth {
        ClaudeAuth::Environment => Some(credential(|key| std::env::var_os(key))?),
        ClaudeAuth::Login => None,
    };
    let owned: &[&str] = match auth {
        ClaudeAuth::Login => &["agent"],
        ClaudeAuth::Environment => &["agent", "claude-home", "claude-home/config"],
    };
    for name in owned {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(context.work.join(name))?;
    }
    // Bound project and git-status discovery to an empty owned repository, not
    // the checkout that contains the work directory.
    let mut init = Command::new("git");
    init.args(["init", "--quiet", "--initial-branch=main"])
        .arg(context.work.join("agent"));
    ensure!(
        crate::process::capture(init, 15)?.status.success(),
        "create owned Claude Code project"
    );
    let mut version = command(context);
    version.arg("--version");
    let version = crate::process::capture(version, 30)?;
    ensure!(version.status.success(), "Claude Code version check failed");
    let login = match auth {
        ClaudeAuth::Login => login_status(context)?,
        ClaudeAuth::Environment => Value::Null,
    };
    let mcp_config = context.work.join("claude-mcp.private.json");
    write_mcp_config(&context.work.join("benchmark-context.json"), &mcp_config)?;
    let profile = preflight::run(context)?;
    let mut source = Command::new("git");
    source
        .current_dir(&context.root)
        .args(["rev-parse", "HEAD"]);
    let source = crate::process::capture(source, 15)?;
    let mut dirty = Command::new("git");
    dirty
        .current_dir(&context.root)
        .args(["status", "--porcelain"]);
    let dirty = crate::process::capture(dirty, 15)?;
    ensure!(
        source.status.success() && dirty.status.success(),
        "source identity unavailable"
    );
    save(
        &context.work.join("benchmark-manifest.json"),
        &json!({
        "task":context.task,"harness":"claude-code","harness_version":String::from_utf8_lossy(&version.stdout).trim(),
        "model_requested":context.model,"model_resolved":null,"model_resolution":"harness-outcome.json usage.models_observed",
        "credential_kind":match &credential {
            None => "login",
            Some(("ANTHROPIC_API_KEY", _)) => "api_key",
            Some(_) => "oauth_token",
        },"login":login,
        "profile":"controlled-mcp-only-v1","harness_profile":profile,
        "mcp_config_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(&mcp_config)?)),
        "prompt_sha256":proofstorm_core::digest_json(&context.task.prompt),
        "source_revision":String::from_utf8_lossy(&source.stdout).trim(),"source_dirty":!dirty.stdout.is_empty(),
        "runner_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(std::env::current_exe()?)?)),
        "platform":std::env::consts::ARCH,"os":std::env::consts::OS,
        "budget":{"wall_seconds":context.task.deadline_seconds,"token_hard_limit":null,"spend_hard_limit":null},
        "limitations":["Claude Code's own system prompt, default effort and thinking settings apply; the preflight wire request records them",
        "host managed (policy) settings, if installed, still apply",
        "built-in subagent definitions are listed but no delegation tool is available",
        "reported cost is Claude Code's estimate; subscription tokens are not billed per call",
        "default MAX_MCP_OUTPUT_TOKENS applies to tool results",
        "no hard token/spend budget claimed","time target provisional"]}),
    )?;
    match &credential {
        Some((kind, secret)) => execute(context, &[(kind, secret.as_os_str())]),
        None => execute(context, &[]),
    }
}

/// Model-free login check. Only the method and plan are retained, never the
/// account email or organization.
fn login_status(context: &Context) -> Result<Value> {
    let mut status = command(context);
    status.args(["auth", "status"]);
    let status = crate::process::capture(status, 30)?;
    let value: Value = serde_json::from_slice(&status.stdout).unwrap_or(Value::Null);
    ensure!(
        status.status.success() && value["loggedIn"] == true,
        "Claude Code is not logged in; run `claude` and log in, or use --benchmark-claude-auth environment"
    );
    Ok(
        json!({"auth_method":value["authMethod"],"api_provider":value["apiProvider"],
        "subscription_type":value["subscriptionType"]}),
    )
}

/// `environment` carries the explicit credential (and, in contract tests, a
/// loopback endpoint); nothing else reaches the CLI beyond `command`.
pub(super) fn execute(context: &Context, environment: &[(&str, &OsStr)]) -> Result<()> {
    fs::write(context.work.join("prompt.txt"), &context.task.prompt)?;
    let private = |name| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(context.work.join(name))
    };
    let out = private("harness.jsonl")?;
    let err = private("harness.stderr")?;
    save(
        &context.work.join("benchmark-attempt.json"),
        &json!({"outcome":"running","elapsed_seconds":null}),
    )?;
    let mut cmd = command(context);
    cmd.args(arguments(
        context,
        &context.work.join("claude-mcp.private.json"),
    ))
    .envs(environment.iter().copied())
    .stdin(Stdio::null())
    .stdout(out)
    .stderr(err);
    // Inherit the acceptance worker's process group. Its parent reaps the whole
    // tree on completion/cancellation; creating another group would escape it.
    let start = Instant::now();
    super::harness::mark_launch(context)?;
    let mut child = cmd.spawn().context("start Claude Code")?;
    let result = (|| -> Result<&str> {
        loop {
            let over = ["harness.jsonl", "harness.stderr"].iter().try_fold(
                false,
                |over, name| -> Result<bool> {
                    Ok(over || fs::metadata(context.work.join(name))?.len() > 32 * 1024 * 1024)
                },
            )?;
            if over {
                return Ok("output_limit");
            }
            if let Some(status) = child.try_wait()? {
                return Ok(if status.success() {
                    "completed"
                } else {
                    "harness_failure"
                });
            }
            if start.elapsed() >= Duration::from_secs(context.task.deadline_seconds.into()) {
                return Ok("timeout");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    save(
        &context.work.join("benchmark-attempt.json"),
        &json!({"outcome":result.as_ref().copied().unwrap_or("harness_failure"),"elapsed_seconds":start.elapsed().as_secs_f64(),"model":context.model,"usage":null,"cost":null}),
    )?;
    result.map(|_| ())
}

pub(super) fn retained(work: &Path) -> Result<AttemptOutput> {
    telemetry::retained(work)
}

/// The exact tool surface Claude Code must expose for this task.
pub(super) fn expected_tools(task: &super::task::Task) -> std::collections::BTreeSet<String> {
    task.allowed_tools
        .iter()
        .map(|name| tool_name(name))
        .collect()
}

pub(super) fn json_lines(text: &str) -> (Vec<Value>, bool) {
    let mut complete = !text.is_empty();
    let rows = text
        .lines()
        .filter_map(|line| {
            serde_json::from_str(line)
                .map_err(|_| complete = false)
                .ok()
        })
        .collect();
    (rows, complete)
}
