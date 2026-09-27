use super::{Context, Harness, save};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

pub(super) fn controlled_model(catalog: &Value, model: &str) -> Result<Value> {
    let models: Vec<_> = catalog["models"]
        .as_array()
        .context("Codex catalog missing models")?
        .iter()
        .filter(|entry| entry["slug"] == model)
        .collect();
    ensure!(
        models.len() == 1,
        "requested model absent or ambiguous in Codex catalog; no substitution"
    );
    let original = models[0];
    let mut controlled = original.clone();
    for key in [
        "apply_patch_tool_type",
        "multi_agent_version",
        "multi_agent_reasoning_effort",
    ] {
        controlled[key] = Value::Null;
    }
    controlled["experimental_supported_tools"] = json!([]);
    controlled["node_repl_disabled"] = json!(true);
    for key in [
        "include_apps_usage_instructions",
        "include_plugin_usage_instructions",
        "include_skills_usage_instructions",
    ] {
        controlled[key] = json!(false);
    }
    Ok(json!({"models":[controlled],"original_model":original}))
}

pub(super) fn write(context: &Context, catalog: Option<&Path>) -> Result<()> {
    let q = |s: &str| serde_json::to_string(s).expect("string JSON");
    let executable = std::env::current_exe()?;
    let mut text = format!(
        r#"model = {model}
model_provider = "openai"
approval_policy = "never"
sandbox_mode = "read-only"
project_doc_max_bytes = 0
web_search = "disabled"
cli_auth_credentials_store = "file"
suppress_unstable_features_warning = true
"#,
        model = q(&context.model)
    );
    if let Some(catalog) = catalog {
        writeln!(
            text,
            "model_catalog_json = {}",
            q(catalog.to_str().context("catalog path")?)
        )?;
    }
    text.push_str(
        r#"
[features]
shell_tool = false
unified_exec = false
apps = false
plugins = false
hooks = false
multi_agent = false
view_image = false
image_generation = false
browser_use = false
computer_use = false
code_mode = true
code_mode_host = true
goals = false
sleep_tool = false
skill_search = false
skip_host_skill_discovery = true
shell_snapshot = false
daemon_auto_start = false
[analytics]
enabled = false
[mcp_servers.proofstorm]
enabled = true
required = true
default_tools_approval_mode = "approve"
startup_timeout_sec = 30
tool_timeout_sec = 120
"#,
    );
    write!(
        text,
        "command = {}\nargs = [{}, {}]\nenabled_tools = {}\n",
        q(executable.to_str().context("runner path")?),
        q("--benchmark-proxy"),
        q(context
            .work
            .join("benchmark-context.json")
            .to_str()
            .context("context path")?),
        serde_json::to_string(&context.task.allowed_tools)?
    )?;
    // Retain the exact config independently of mutable CLI state.
    for path in [
        context.work.join("codex-config.private.toml"),
        context.work.join("codex-home/config.toml"),
    ] {
        let mut file = tempfile::NamedTempFile::new_in(path.parent().context("config parent")?)?;
        file.write_all(text.as_bytes())?;
        file.persist(path)?;
    }
    Ok(())
}

pub(super) fn copy_auth(context: &Context) -> Result<()> {
    let Harness::Codex { auth_file, .. } = &context.harness else {
        unreachable!("Codex auth")
    };
    let auth = if auth_file.is_none()
        && let Ok(key) = std::env::var("CODEX_API_KEY")
    {
        ensure!(!key.trim().is_empty(), "empty CODEX_API_KEY");
        json!({"OPENAI_API_KEY":key})
    } else {
        let path = if let Some(path) = auth_file {
            path.clone()
        } else {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
                .context("select --benchmark-codex-auth or set CODEX_API_KEY")?;
            home.join("auth.json")
        };
        let stat = fs::symlink_metadata(&path).context("Codex file auth missing; select --benchmark-codex-auth or set CODEX_API_KEY (keyring-only login is not copied)")?;
        ensure!(
            stat.is_file() && stat.len() <= 1024 * 1024,
            "Codex auth must be a bounded regular file"
        );
        serde_json::from_slice::<Value>(&fs::read(path)?)?
    };
    ensure!(
        auth.is_object()
            && (auth["OPENAI_API_KEY"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
                || auth["tokens"].is_object()),
        "unrecognized Codex file authentication"
    );
    save(&context.work.join("codex-home/auth.json"), &auth)
}

pub(super) fn audit_mcp(context: &Context, listed: &Value) -> Result<()> {
    let entries = listed.as_array().context("Codex MCP list missing")?;
    ensure!(
        entries.len() == 1 && entries[0]["name"] == "proofstorm" && entries[0]["enabled"] == true,
        "additional or disabled Codex MCP configuration; model not started"
    );
    let expected = std::env::current_exe()?;
    let transport = &entries[0]["transport"];
    ensure!(
        transport["type"] == "stdio"
            && transport["command"] == expected.to_string_lossy().as_ref()
            && transport["args"]
                == json!([
                    "--benchmark-proxy",
                    context.work.join("benchmark-context.json")
                ]),
        "Codex MCP command changed during configuration resolution"
    );
    ensure!(
        transport["env"].is_null()
            || transport["env"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
        "unexpected Codex MCP environment"
    );
    ensure!(
        transport["env_vars"].as_array().is_none_or(Vec::is_empty) && transport["cwd"].is_null(),
        "unexpected Codex MCP environment/cwd"
    );
    Ok(())
}

/// Change only the capture path while preserving the runner-selected transport.
pub(super) fn proxy_context(context: &Context, path: &Path) -> Result<()> {
    let target = context.work.join("codex-home/config.toml");
    let mut doc: toml_edit::DocumentMut = fs::read_to_string(&target)?.parse()?;
    let mut args = toml_edit::Array::new();
    args.push("--benchmark-proxy");
    args.push(path.to_str().context("proxy context path")?);
    doc["mcp_servers"]["proofstorm"]["args"] = toml_edit::value(args);
    let mut file = tempfile::NamedTempFile::new_in(target.parent().context("config parent")?)?;
    file.write_all(doc.to_string().as_bytes())?;
    file.persist(target)?;
    Ok(())
}
