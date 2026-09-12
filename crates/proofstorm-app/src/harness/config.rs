use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table};

pub(super) fn read(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "refusing linked, non-file or oversized configuration: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.nlink() == 1,
            "hard-linked configuration refused: {}",
            path.display()
        );
    }
    Ok(Some(fs::read_to_string(path).with_context(|| {
        format!("cannot read configuration {}", path.display())
    })?))
}

pub(super) fn document(path: &Path, text: &str) -> Result<DocumentMut> {
    text.parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid TOML in {}; no configuration was changed",
            path.display()
        )
    })
}

pub(super) fn value(document: &DocumentMut) -> Result<Value> {
    toml_edit::de::from_document(document.clone())
        .map_err(|_| anyhow::anyhow!("unsupported TOML configuration; no changes made"))
}

pub(super) fn directory(path: &Path, create: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_dir(),
            "refusing linked or non-directory path: {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn proofstorm_server(name: &str, value: &Value) -> bool {
    name == "proofstorm"
        || value["command"].as_str().is_some_and(|s| {
            Path::new(s)
                .file_name()
                .is_some_and(|name| name == "proofstorm-mcp")
        })
}

pub(super) fn inherited(project: &Path, codex_home: &Path, system: &Path) -> Result<()> {
    let mut paths = BTreeSet::from([codex_home.join("config.toml"), system.to_path_buf()]);
    for parent in project.ancestors().skip(1) {
        paths.insert(parent.join(".codex/config.toml"));
    }
    // A selected profile can add another MCP server. Never print its contents.
    if let Some(text) = read(&codex_home.join("config.toml"))? {
        let global = value(&document(&codex_home.join("config.toml"), &text)?)?;
        if let Some(profile) = global["profile"].as_str() {
            ensure!(
                !profile.is_empty()
                    && profile
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                "unsupported Codex profile name; review attachment manually"
            );
            paths.insert(codex_home.join(format!("{profile}.config.toml")));
        }
    }
    for path in paths {
        if path == project.join(".codex/config.toml") {
            bail!("project attachment must not target the global Codex configuration");
        }
        if let Some(text) = read(&path)? {
            let data = value(&document(&path, &text)?)?;
            if data["mcp_servers"].as_object().is_some_and(|servers| {
                servers
                    .iter()
                    .any(|(name, entry)| proofstorm_server(name, entry))
            }) {
                bail!(
                    "inherited Proofstorm MCP configuration in {}; keep it or resolve that conflict explicitly before project attachment (nothing overwritten)",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

pub(super) fn merge(
    path: &Path,
    original: Option<&str>,
    entry: &Value,
    owned: &[Value],
) -> Result<String> {
    let mut doc = document(path, original.unwrap_or(""))?;
    let data = value(&doc)?;
    if let Some(servers) = data["mcp_servers"].as_object() {
        for (name, server) in servers {
            if name != "proofstorm" && proofstorm_server(name, server) {
                bail!("another project MCP entry already starts Proofstorm; resolve it explicitly");
            }
        }
        if let Some(existing) = servers.get("proofstorm") {
            ensure!(
                owned.contains(existing),
                "the project Proofstorm entry is manual or was changed; refusing to overwrite it"
            );
            if existing == entry {
                return Ok(original.unwrap_or("").into());
            }
        }
    }
    if doc.get("mcp_servers").is_none() {
        let mut table = Table::new();
        table.set_implicit(true);
        doc["mcp_servers"] = Item::Table(table);
    }
    ensure!(
        doc["mcp_servers"].is_table(),
        "mcp_servers must be a regular TOML table; no changes made"
    );
    let generated =
        toml_edit::ser::to_string(&serde_json::json!({"mcp_servers":{"proofstorm":entry}}))?;
    let generated = document(path, &generated)?;
    let position = doc["mcp_servers"]
        .get("proofstorm")
        .and_then(Item::as_table)
        .and_then(Table::position)
        .unwrap_or(usize::MAX);
    let mut server = generated["mcp_servers"]["proofstorm"]
        .clone()
        .into_table()
        .map_err(|_| anyhow::anyhow!("generated MCP entry must be a table"))?;
    // Append new connections without reordering the user's existing sections.
    server.set_position(position);
    doc["mcp_servers"]["proofstorm"] = Item::Table(server);
    let result = doc.to_string();
    ensure!(
        value(&document(path, &result)?)?["mcp_servers"]["proofstorm"] == *entry,
        "generated MCP configuration did not round-trip"
    );
    Ok(result)
}

pub(super) fn save(path: &Path, bytes: &[u8], preserve_mode: bool) -> Result<()> {
    read(path)?; // Refuse symlinks/non-files even for managed receipts.
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("file parent missing")?)?;
    if preserve_mode && path.exists() {
        file.as_file()
            .set_permissions(fs::metadata(path)?.permissions())?;
    }
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub(super) fn backup(path: &Path, original: &str) -> Result<PathBuf> {
    let target = path.with_file_name(format!(
        "{}.proofstorm-backup-{}",
        path.file_name()
            .context("config filename")?
            .to_string_lossy(),
        super::hash(original.as_bytes())
    ));
    if let Some(old) = read(&target)? {
        ensure!(old == original, "backup conflict; nothing overwritten");
    } else {
        let mut file = tempfile::NamedTempFile::new_in(path.parent().context("config parent")?)?;
        file.write_all(original.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&target)?;
    }
    Ok(target)
}
