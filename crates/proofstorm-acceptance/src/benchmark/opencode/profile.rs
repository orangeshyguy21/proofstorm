//! Explicit, private provider profiles. No personal agent/MCP settings are inherited.
use super::{Context, Harness, read, save};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

fn bytes(path: &Path) -> Result<Vec<u8>> {
    let stat = fs::symlink_metadata(path)?;
    ensure!(
        stat.is_file() && stat.len() <= 2 * 1024 * 1024,
        "profile files must be bounded regular files"
    );
    Ok(fs::read(path)?)
}

fn files(root: &Path, directory: &Path, output: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    ensure!(
        fs::symlink_metadata(directory)?.is_dir(),
        "plugin directory must not be a symlink"
    );
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            ensure!(
                path.strip_prefix(root)?.components().count() <= 8,
                "plugin nesting limit"
            );
            files(root, &path, output)?;
        } else {
            ensure!(output.len() < 64, "plugin file limit");
            output.insert(
                path.strip_prefix(root)?
                    .to_str()
                    .context("plugin path encoding")?
                    .into(),
                bytes(&path)?,
            );
        }
    }
    Ok(())
}

struct Profile {
    config: Value,
    plugins: Vec<BTreeMap<String, Vec<u8>>>,
}

fn load(path: &Path) -> Result<Profile> {
    ensure!(path.is_absolute(), "OpenCode profile path must be absolute");
    let config: Value = serde_json::from_slice(&bytes(path)?)?;
    ensure!(
        config
            .as_object()
            .is_some_and(|m| m.keys().all(|k| matches!(
                k.as_str(),
                "$schema" | "providers" | "plugins" | "diagnostics"
            ))),
        "OpenCode profile accepts only providers, explicit local plugins and diagnostics"
    );
    ensure!(
        config["providers"].is_object(),
        "OpenCode profile requires native v2 providers"
    );
    super::diagnostics::controls(&config)?;
    let mut plugins = Vec::new();
    if let Some(list) = config.get("plugins") {
        let list = list
            .as_array()
            .context("profile plugins must be an array")?;
        ensure!(list.len() <= 8, "plugin count limit");
        for item in list {
            let reference = Path::new(
                item.as_str()
                    .context("plugins must name local directories")?,
            );
            ensure!(
                reference.is_absolute() || reference.starts_with("."),
                "plugins must use explicit local paths"
            );
            let root = path.parent().context("profile parent")?.join(reference);
            let mut entries = BTreeMap::new();
            files(&root, &root, &mut entries)?;
            let package: Value = serde_json::from_slice(
                entries
                    .get("package.json")
                    .context("plugin package.json missing")?,
            )?;
            ensure!(
                ["dependencies", "optionalDependencies", "devDependencies"]
                    .iter()
                    .all(|key| package
                        .get(key)
                        .is_none_or(|v| v.as_object().is_some_and(serde_json::Map::is_empty))),
                "benchmark plugins must be self-contained; external dependencies are not pinned"
            );
            plugins.push(entries);
        }
    }
    Ok(Profile { config, plugins })
}

fn digest(profile: &Profile) -> String {
    let plugins: Vec<_> = profile
        .plugins
        .iter()
        .map(|files| {
            files
                .iter()
                .map(|(name, bytes)| (name, format!("{:x}", Sha256::digest(bytes))))
                .collect::<BTreeMap<_, _>>()
        })
        .collect();
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({"config":profile.config,"plugins":plugins}))
                .expect("profile JSON")
        )
    )
}

/// Pins profile content and every local plugin file, without resolving credentials.
pub(crate) fn fingerprint(path: &Path) -> Result<String> {
    Ok(digest(&load(path)?))
}

fn resolve(value: &mut Value, base: &Path) -> Result<()> {
    match value {
        Value::Object(map) => {
            for item in map.values_mut() {
                resolve(item, base)?;
            }
        }
        Value::Array(list) => {
            for item in list {
                resolve(item, base)?;
            }
        }
        Value::String(text) => {
            if let Some(name) = text.strip_prefix("{env:").and_then(|s| s.strip_suffix('}')) {
                *text = std::env::var(name).context("profile environment reference is missing")?;
            } else if let Some(name) = text
                .strip_prefix("{file:")
                .and_then(|s| s.strip_suffix('}'))
            {
                let path = if let Some(rest) = name.strip_prefix("~/") {
                    PathBuf::from(std::env::var_os("HOME").context("HOME missing")?).join(rest)
                } else {
                    base.join(name)
                };
                *text = String::from_utf8(bytes(&path)?)?.trim().into();
            }
        }
        _ => (),
    }
    Ok(())
}

pub(super) fn prepare(context: &Context) -> Result<(Value, String)> {
    let Harness::OpenCode {
        profile: Some(path),
        ..
    } = &context.harness
    else {
        anyhow::bail!(
            "OpenCode 2 requires --benchmark-opencode-profile (private providers/plugins JSON)"
        )
    };
    let profile = load(path)?;
    let fingerprint = digest(&profile);
    let (provider, model) = context
        .model
        .split_once('/')
        .context("expected provider/model")?;
    let model = model.split('#').next().context("model ID")?;
    let mut selected = profile.config["providers"][provider].clone();
    ensure!(
        selected.is_object() && selected["models"][model].is_object(),
        "requested provider/model missing from private profile; no substitution"
    );
    let entry = selected["models"][model].clone();
    selected["models"] = json!({model:entry});
    resolve(&mut selected, path.parent().context("profile parent")?)?;
    let mut plugins = Vec::new();
    for (index, files) in profile.plugins.iter().enumerate() {
        let root = context.work.join(format!("opencode-home/plugin-{index}"));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        for (name, contents) in files {
            let target = root.join(name);
            fs::create_dir_all(target.parent().context("plugin parent")?)?;
            fs::write(target, contents)?;
        }
        plugins.push(root.canonicalize()?);
    }
    // Last hook observes the final request after caller compatibility plugins.
    plugins.push(super::diagnostics::install(context, &profile.config)?);
    Ok((
        json!({"providers":{provider:selected},"plugins":plugins}),
        fingerprint,
    ))
}

pub(super) fn permissions(context: &Context) -> Value {
    let mut rules = vec![json!({"action":"*","resource":"*","effect":"deny"})];
    rules.extend(context.task.allowed_tools.iter().map(
        |name| json!({"action":format!("proofstorm_{name}"),"resource":"*","effect":"allow"}),
    ));
    json!(rules)
}

pub(super) fn config(context: &Context, provider: &Value) -> Result<Value> {
    let executable = std::env::current_exe()?;
    let mut environment = serde_json::Map::new();
    for key in [
        "HOME",
        "PATH",
        "TMPDIR",
        "DOCKER_CONFIG",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.into(), json!(value));
        }
    }
    let mut entry = json!({"update":"disable","snapshots":false,"model":context.model,
        "permissions":permissions(context),"default_agent":"benchmark",
        "agents":{"benchmark":{"mode":"primary","system":"Complete the user's task autonomously using only the supplied Proofstorm MCP tools. Follow the checkpoint and cleanup contract.","steps":150,"permissions":permissions(context)},
        "title":{"disabled":true},"summary":{"disabled":true}},
        "mcp":{"servers":{"proofstorm":{"type":"local","codemode":false,"command":[executable,"--benchmark-proxy",context.work.join("benchmark-context.json")],"environment":environment,"timeout":{"startup":120_000,"catalog":120_000,"execution":120_000}}}},
        "providers":provider["providers"],"plugins":provider["plugins"]});
    // Compaction uses the explicitly selected model with the same denied tools.
    entry["agents"]["compaction"] =
        json!({"model":context.model,"permissions":permissions(context)});
    Ok(entry)
}

pub(super) fn audit(context: &Context, sources: &Value, expected: &Value) -> Result<()> {
    let project = context.work.join("agent/opencode.json");
    let directory = context.work.join("opencode-home/config/opencode");
    let sources = sources
        .as_array()
        .context("OpenCode config sources missing")?;
    ensure!(
        sources.len() == 2,
        "unexpected OpenCode configuration sources"
    );
    ensure!(
        sources
            .iter()
            .any(|s| s["type"] == "directory" && s["path"] == json!(directory)),
        "unexpected global config directory"
    );
    let documents: Vec<_> = sources.iter().filter(|s| s["type"] == "document").collect();
    ensure!(
        documents.len() == 1 && documents[0]["path"] == json!(project),
        "unexpected OpenCode config document"
    );
    let info = &documents[0]["info"];
    for field in [
        "permissions",
        "mcp",
        "plugins",
        "providers",
        "snapshots",
        "update",
        "default_agent",
    ] {
        ensure!(
            info[field] == expected[field],
            "OpenCode changed controlled configuration: {field}"
        );
    }
    for field in ["system", "steps", "permissions", "mode"] {
        ensure!(
            info["agents"]["benchmark"][field] == expected["agents"]["benchmark"][field],
            "OpenCode changed benchmark agent: {field}"
        );
    }
    save(
        &context.work.join("harness-config.private.json"),
        &json!({"sources":sources,"profile":read(&project)?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_hash_pins_plugin_bytes_and_rejects_other_config() -> Result<()> {
        let work = tempfile::tempdir()?;
        let path = work.path().join("profile.json");
        fs::create_dir(work.path().join("compat"))?;
        fs::write(
            work.path().join("compat/package.json"),
            r#"{"name":"fixture","type":"module"}"#,
        )?;
        fs::write(work.path().join("compat/index.ts"), "export default {}")?;
        let config =
            json!({"providers":{"fixture":{"models":{"model":{}}}},"plugins":["./compat"]});
        save(&path, &config)?;
        let first = fingerprint(&path)?;
        fs::write(
            work.path().join("compat/index.ts"),
            "export default { changed: true }",
        )?;
        assert_ne!(first, fingerprint(&path)?);
        let mut config = config;
        config["permissions"] = json!([]);
        save(&path, &config)?;
        assert!(fingerprint(&path).is_err());
        Ok(())
    }

    #[test]
    fn plugin_links_and_unpinned_dependencies_are_rejected() -> Result<()> {
        let work = tempfile::tempdir()?;
        let path = work.path().join("profile.json");
        fs::create_dir(work.path().join("compat"))?;
        save(&path, &json!({"providers":{},"plugins":["./compat"]}))?;
        let package = work.path().join("compat/package.json");
        fs::write(&package, r#"{"dependencies":{"external":"latest"}}"#)?;
        assert!(fingerprint(&path).is_err());
        fs::write(&package, "{}")?;
        std::os::unix::fs::symlink(&package, work.path().join("compat/linked.json"))?;
        assert!(fingerprint(&path).is_err());
        Ok(())
    }
}
