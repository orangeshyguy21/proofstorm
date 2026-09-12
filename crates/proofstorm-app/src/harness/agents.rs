use super::{Harness, SERVER_NAME, config, json_config};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(super) fn key(harness: Harness) -> &'static str {
    match harness {
        Harness::Codex => "mcp_servers",
        Harness::Opencode => "mcp",
        Harness::Claude => "mcpServers",
    }
}
pub(super) fn path(harness: Harness, project: &Path) -> Result<PathBuf> {
    Ok(match harness {
        Harness::Codex => {
            config::directory(&project.join(".codex"), false)?;
            project.join(".codex/config.toml")
        }
        Harness::Claude => project.join(".mcp.json"),
        Harness::Opencode => {
            let json = config::read(&project.join("opencode.json"))?;
            let jsonc = config::read(&project.join("opencode.jsonc"))?;
            ensure!(
                json.is_none() || jsonc.is_none(),
                "both opencode.json and opencode.jsonc exist; choose one explicitly before attachment"
            );
            project.join(if jsonc.is_some() {
                "opencode.jsonc"
            } else {
                "opencode.json"
            })
        }
    })
}
pub(super) fn entry(harness: Harness, normalized: &Value) -> Result<Value> {
    // These clients expand config strings. Never let a literal filesystem path
    // become an environment/file substitution at agent startup.
    for value in [
        normalized["command"].as_str(),
        normalized["cwd"].as_str(),
        normalized["args"][1].as_str(),
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            match harness {
                Harness::Codex => true,
                Harness::Claude => !value.contains("${"),
                Harness::Opencode => !value.contains("{env:") && !value.contains("{file:"),
            },
            "path contains an agent configuration expansion marker; use a literal project/install path"
        );
    }
    Ok(match harness {
        Harness::Codex => normalized.clone(),
        Harness::Claude => {
            json!({"type":"stdio", "command":normalized["command"], "args":normalized["args"]})
        }
        Harness::Opencode => {
            let mut command = vec![normalized["command"].clone()];
            command.extend(
                normalized["args"]
                    .as_array()
                    .context("MCP arguments")?
                    .iter()
                    .cloned(),
            );
            json!({"type":"local", "command":command, "enabled":true, "timeout":60000})
        }
    })
}
pub(super) fn existing(harness: Harness, path: &Path, text: &str) -> Result<Option<Value>> {
    let value = match harness {
        Harness::Codex => config::value(&config::document(path, text)?)?,
        _ => json_config::value(text, harness == Harness::Opencode)?,
    };
    Ok(value[key(harness)]
        .get(SERVER_NAME)
        .or_else(|| value[key(harness)].get("proofstorm"))
        .cloned())
}
pub(super) fn merge(
    harness: Harness,
    path: &Path,
    text: Option<&str>,
    entry: &Value,
    owned: &[Value],
) -> Result<String> {
    if harness == Harness::Codex {
        config::merge(path, text, entry, owned)
    } else {
        json_config::merge(
            text,
            key(harness),
            entry,
            owned,
            harness == Harness::Opencode,
        )
        .with_context(|| {
            format!(
                "cannot safely update {}; no configuration changed",
                path.display()
            )
        })
    }
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is missing")?,
    ))
}
fn check(value: &Value, path: &Path) -> Result<()> {
    for name in ["mcp", "mcpServers"] {
        if let Some(servers) = value[name].as_object() {
            ensure!(
                !servers
                    .iter()
                    .any(|(n, v)| json_config::is_proofstorm(n, v)),
                "inherited Proofstorm MCP configuration in {}; resolve that conflict explicitly (nothing overwritten)",
                path.display()
            );
        }
    }
    Ok(())
}
pub(super) fn inherited(harness: Harness, project: &Path) -> Result<()> {
    if harness == Harness::Codex {
        return config::inherited(
            project,
            &super::codex_home()?,
            Path::new("/etc/codex/config.toml"),
        );
    }
    let home = home()?;
    let mut paths = BTreeSet::new();
    match harness {
        Harness::Opencode => {
            // Ambient overrides can hide a project attachment. Do not remove them
            // or silently override an operator's chosen configuration.
            for key in [
                "OPENCODE_CONFIG",
                "OPENCODE_CONFIG_CONTENT",
                "OPENCODE_CONFIG_DIR",
                "OPENCODE_DISABLE_PROJECT_CONFIG",
                "OPENCODE_MANAGED_CONFIG_DIR",
            ] {
                ensure!(
                    std::env::var_os(key).is_none_or(|s| s.is_empty()),
                    "{key} is set; use a normal OpenCode configuration before project attachment"
                );
            }
            let global = std::env::var_os("XDG_CONFIG_HOME")
                .map_or_else(|| home.join(".config"), PathBuf::from);
            ensure!(global.is_absolute(), "XDG_CONFIG_HOME must be absolute");
            let mut dirs = vec![
                global.join("opencode"),
                home.join(".opencode"),
                PathBuf::from("/Library/Application Support/opencode"),
                PathBuf::from("/etc/opencode"),
            ];
            paths.insert(global.join("opencode/config.json"));
            for parent in project.ancestors() {
                config::directory(&parent.join(".opencode"), false)?;
                dirs.push(parent.join(".opencode"));
                if parent != project {
                    dirs.push(parent.into());
                }
            }
            for dir in dirs {
                for file in ["opencode.json", "opencode.jsonc"] {
                    paths.insert(dir.join(file));
                }
            }
        }
        Harness::Claude => {
            let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map_or_else(|| home.join(".claude"), PathBuf::from);
            ensure!(dir.is_absolute(), "CLAUDE_CONFIG_DIR must be absolute");
            // Local Code sessions also inherit Desktop-chat MCP servers, which
            // take precedence over project entries with the same name.
            paths
                .insert(home.join("Library/Application Support/Claude/claude_desktop_config.json"));
            paths.insert(home.join(".claude.json"));
            paths.insert(dir.join(".claude.json"));
            paths.insert(PathBuf::from(
                "/Library/Application Support/ClaudeCode/managed-mcp.json",
            ));
            paths.insert(PathBuf::from("/etc/claude-code/managed-mcp.json"));
            for parent in project.ancestors().skip(1) {
                paths.insert(parent.join(".mcp.json"));
            }
        }
        Harness::Codex => unreachable!(),
    }
    check_paths(harness, project, paths)
}

pub(super) fn check_paths(
    harness: Harness,
    project: &Path,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<()> {
    for path in paths {
        if let Some(text) = config::read(&path)? {
            let value = json_config::value(&text, harness == Harness::Opencode)
                .with_context(|| format!("cannot inspect {}; no changes made", path.display()))?;
            check(&value, &path)?;
            if harness == Harness::Claude {
                for parent in project.ancestors() {
                    if let Some(local) = value["projects"].get(parent.to_string_lossy().as_ref()) {
                        check(local, &path)?;
                    }
                }
            }
        }
    }
    Ok(())
}
