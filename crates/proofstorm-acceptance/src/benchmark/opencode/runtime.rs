//! A process-owned OpenCode 2 server, never the user's background service.
use super::{Context, Harness, profile, save};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    fmt::Write,
    fs,
    io::Read,
    os::unix::fs::DirBuilderExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn command(context: &Context) -> Command {
    let Harness::OpenCode { executable, .. } = &context.harness else {
        unreachable!("OpenCode selection")
    };
    let home = context.work.join("opencode-home");
    let mut command = Command::new(executable);
    command.env_clear();
    for key in ["PATH", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR", "LANG"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .current_dir(context.work.join("agent"))
        .env("HOME", &home)
        .env("PWD", context.work.join("agent"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
        .env("OPENCODE_DISABLE_MODELS_FETCH", "true")
        .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
        .env("OPENCODE_DISABLE_FILEWATCHER", "true")
        .env("OPENCODE_CONFIG", context.work.join("agent/opencode.json"));
    command
}

pub(super) struct Server {
    child: Child,
    url: String,
    password: String,
}

impl Server {
    pub(super) fn start(context: &Context) -> Result<Self> {
        let mut random = [0_u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let password = random.iter().fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").expect("string write");
            text
        });
        let ready = context.work.join("opencode-server.private.jsonl");
        let child = command(context)
            .args(["serve", "--stdio", "--port", "0", "--hostname", "127.0.0.1"])
            .env("OPENCODE_PASSWORD", &password)
            .stdin(Stdio::piped())
            .stdout(fs::File::create(&ready)?)
            .stderr(fs::File::create(
                context.work.join("opencode-server.private.log"),
            )?)
            .spawn()?;
        let mut server = Self {
            child,
            url: String::new(),
            password,
        };
        let start = Instant::now();
        loop {
            ensure!(
                server.child.try_wait()?.is_none(),
                "private OpenCode server exited during startup"
            );
            if let Some(line) = fs::read_to_string(&ready)?.lines().next()
                && let Ok(value) = serde_json::from_str::<Value>(line)
                && let Some(url) = value["url"].as_str()
            {
                let port = url
                    .strip_prefix("http://127.0.0.1:")
                    .context("OpenCode server must bind owned loopback")?
                    .parse::<u16>()?;
                ensure!(port != 0, "invalid OpenCode port");
                server.url = url.into();
                return Ok(server);
            }
            ensure!(
                start.elapsed() < Duration::from_secs(30),
                "OpenCode readiness timeout"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub(super) fn command(&self, context: &Context, subcommand: &str) -> Command {
        let mut cmd = command(context);
        cmd.arg(subcommand)
            .arg("--server")
            .arg(&self.url)
            .env("OPENCODE_PASSWORD", &self.password);
        cmd
    }

    pub(super) fn api(&self, context: &Context, operation: &str) -> Result<Value> {
        self.api_params(context, operation, &[])
    }

    fn api_params(
        &self,
        context: &Context,
        operation: &str,
        parameters: &[String],
    ) -> Result<Value> {
        let mut cmd = self.command(context, "api");
        cmd.arg(operation);
        for parameter in parameters {
            cmd.args(["--param", parameter]);
        }
        let result = crate::process::capture(cmd, 60)?;
        if !result.status.success() {
            fs::write(
                context.work.join("opencode-api-error.private.txt"),
                &result.stderr,
            )?;
            anyhow::bail!("OpenCode API {operation} failed; see private diagnostics");
        }
        Ok(serde_json::from_slice(&result.stdout)?)
    }

    pub(super) fn capture_session(&self, context: &Context) -> Result<()> {
        let transcript = fs::read_to_string(context.work.join("harness.jsonl"))?;
        let rows = transcript
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let session = rows
            .iter()
            .find_map(|row| row["sessionID"].as_str())
            .context("OpenCode session identity missing")?;
        ensure!(
            rows.iter()
                .all(|row| row["sessionID"].as_str().is_none_or(|id| id == session)),
            "mixed OpenCode sessions"
        );
        let export = self.api_params(
            context,
            "experimental.session.export",
            &[format!("sessionID={session}")],
        )?;
        let data = &export["data"];
        let (provider, model) = context.model.split_once('/').context("model identity")?;
        let model = model.split('#').next().context("model identity")?;
        ensure!(
            data["info"]["id"] == session
                && data["info"]["model"]["providerID"] == provider
                && data["info"]["model"]["id"] == model
                && data["info"]["agent"] == "benchmark",
            "OpenCode session identity differs"
        );
        let messages = data["messages"]
            .as_array()
            .context("session messages missing")?;
        ensure!(
            messages
                .iter()
                .any(|m| m["type"] == "user" && m["text"] == context.task.prompt),
            "session task prompt differs"
        );
        ensure!(
            messages
                .iter()
                .filter(|m| m["type"] == "assistant")
                .all(|m| m["model"]["providerID"] == provider && m["model"]["id"] == model),
            "session model substitution"
        );
        save(&context.work.join("opencode-session.private.json"), &export)
    }

    pub(super) fn preflight(&self, context: &Context, expected: &Value) -> Result<Value> {
        profile::audit(context, &self.api(context, "config.get")?, expected)?;
        let (provider_id, model_id) = context.model.split_once('/').context("model identity")?;
        let model_id = model_id.split('#').next().context("model identity")?;
        let start = Instant::now();
        loop {
            let agents = self.api(context, "agent.list")?;
            let models = self.api(context, "model.list")?;
            let mcp = self.api(context, "mcp.list")?;
            let plugins = self.api(context, "plugin.list")?;
            let benchmark = agents["data"]
                .as_array()
                .context("OpenCode agents missing")?
                .iter()
                .find(|a| a["id"] == "benchmark");
            let model = models["data"]
                .as_array()
                .context("OpenCode models missing")?
                .iter()
                .find(|m| {
                    m["providerID"] == provider_id && m["id"] == model_id && m["enabled"] == true
                });
            save(
                &context.work.join("opencode-preflight.private.json"),
                &json!({"agents":agents,"mcp":mcp,"plugins":plugins,"model":model}),
            )?;
            if let (Some(agent), Some(model)) = (benchmark, model) {
                let plugin_entries = plugins["data"]
                    .as_array()
                    .context("OpenCode plugins missing")?;
                let expected_plugins =
                    expected["plugins"].as_array().context("expected plugins")?;
                let local: Vec<_> = plugin_entries
                    .iter()
                    .filter(|p| p["source"]["type"] != "builtin")
                    .collect();
                ensure!(
                    local
                        .iter()
                        .all(|plugin| plugin["source"]["type"] == "local"
                            && plugin["source"]["path"].as_str().is_some_and(|path| {
                                expected_plugins.iter().any(|root| {
                                    root.as_str().is_some_and(|root| {
                                        std::path::Path::new(path).starts_with(root)
                                    })
                                })
                            })),
                    "unexpected OpenCode plugin"
                );
                let plugins_ready = expected_plugins.iter().all(|root| {
                    root.as_str().is_some_and(|root| {
                        local.iter().any(|plugin| {
                            plugin["state"]["status"] == "active"
                                && plugin["source"]["path"].as_str().is_some_and(|path| {
                                    std::path::Path::new(path).starts_with(root)
                                })
                        })
                    })
                }) && local
                    .iter()
                    .all(|plugin| plugin["state"]["status"] == "active");
                let rules = agent["permissions"]
                    .as_array()
                    .context("OpenCode agent permissions missing")?;
                let expected_rules = profile::permissions(context);
                let expected_rules = expected_rules.as_array().expect("permissions array");
                let boundary = rules
                    .windows(expected_rules.len())
                    .rposition(|window| window == expected_rules)
                    .context("OpenCode benchmark permission boundary missing")?
                    + expected_rules.len();
                let safe_tail = rules[boundary..].iter().all(|rule| {
                    rule["effect"] == "deny"
                        && rule["action"].as_str().is_some_and(|action| {
                            !action.contains(['*', '?']) && !action.starts_with("proofstorm_")
                        })
                });
                ensure!(
                    safe_tail
                        && agent["steps"] == 150
                        && agent["system"] == expected["agents"]["benchmark"]["system"],
                    "OpenCode effective benchmark agent differs"
                );
                ensure!(
                    !agents["data"]
                        .as_array()
                        .expect("agents")
                        .iter()
                        .any(|a| a["id"] == "title" || a["id"] == "summary"),
                    "background model agents must be disabled"
                );
                if mcp["data"].as_array().is_some_and(|m| {
                    m.len() == 1
                        && m[0]["name"] == "proofstorm"
                        && m[0]["status"]["status"] == "connected"
                }) && context.work.join("proxy-ready.json").exists()
                    && plugins_ready
                {
                    profile::audit(context, &self.api(context, "config.get")?, expected)?;
                    return Ok(model.clone());
                }
            }
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "OpenCode model/agent/MCP readiness failed; see private preflight evidence"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let start = Instant::now();
        while self.child.try_wait().ok().flatten().is_none()
            && start.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn directories(context: &Context) -> Result<()> {
    for path in [
        context.work.join("agent"),
        context.work.join("opencode-home"),
    ] {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    for path in ["config/opencode", "data", "cache", "state"] {
        fs::create_dir_all(context.work.join("opencode-home").join(path))?;
    }
    Ok(())
}
