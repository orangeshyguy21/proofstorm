//! Read-only evidence that a live run left preexisting Docker resources and
//! user configuration alone. Image-cache growth is expected and not compared.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const LIFECYCLE_FIELDS: [&str; 4] = ["running", "restarting", "started", "restarts"];

/// Normally two observations five seconds apart. A container already in Docker
/// restart backoff gets a bounded chance to demonstrate its next restart. No
/// exemption is inferred from the flag alone, and no resource is modified.
pub fn baseline(
    checkout_home: Option<&Path>,
    mut record: impl FnMut(usize, &Value) -> Result<()>,
) -> Result<(Value, Value)> {
    let first = snapshot(checkout_home)?;
    record(0, &first)?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut excluded = serde_json::Map::new();
    for index in 1.. {
        std::thread::sleep(Duration::from_secs(5));
        let current = snapshot(checkout_home)?;
        record(index, &current)?;
        let changes = exclusions(&first, &current, None)?;
        for (id, fields) in changes.as_object().context("exclusions map")? {
            let entry = excluded.entry(id.clone()).or_insert(json!([]));
            let retained = entry.as_array_mut().context("excluded fields")?;
            for field in fields.as_array().context("excluded fields")? {
                if !retained.contains(field) {
                    retained.push(field.clone());
                }
            }
        }
        if !awaiting_restart(&first, &current) {
            return Ok((current, Value::Object(excluded)));
        }
        ensure!(
            Instant::now() < deadline,
            "preexisting restarting container did not complete baseline observation; no runtime or model started"
        );
    }
    unreachable!("unbounded iterator returns or reaches deadline")
}

fn awaiting_restart(first: &Value, current: &Value) -> bool {
    first["containers"].as_object().is_some_and(|containers| {
        containers.iter().any(|(id, value)| {
            value["restarting"] == true
                && value["restarts"] == current["containers"][id]["restarts"]
                && value["started"] == current["containers"][id]["started"]
        })
    })
}

fn docker(args: &[&str]) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut output = tempfile::tempfile()?;
    let mut command = Command::new("docker");
    crate::client::clear_runtime_environment(&mut command);
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "Docker preservation inventory failed");
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Docker preservation inventory timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut result = String::new();
    output.take(8 * 1024 * 1024).read_to_string(&mut result)?;
    Ok(result)
}

fn configuration(path: &Path) -> Result<Value> {
    let claude = path.file_name().is_some_and(|name| name == ".claude.json");
    let exists = path.try_exists()?;
    if !exists && !claude {
        return Ok(Value::Null);
    }
    let bytes = if exists {
        fs::read(path)?
    } else {
        b"{}".to_vec()
    };
    let bytes = if claude {
        serde_json::to_vec(&claude_mcp_configuration(&serde_json::from_slice(&bytes)?)?)?
    } else {
        bytes
    };
    Ok(json!(format!("{:x}", Sha256::digest(bytes))))
}

// Claude updates session metadata independently. Attach can only change these
// MCP maps; empty project entries are equivalent to absent entries.
fn claude_mcp_configuration(config: &Value) -> Result<Value> {
    fn servers(value: Option<&Value>) -> Result<Value> {
        match value {
            None => Ok(json!({})),
            Some(Value::Object(map)) => Ok(canonical(&json!(map))),
            _ => anyhow::bail!("Claude MCP configuration is not an object"),
        }
    }
    ensure!(config.is_object(), "Claude configuration is not an object");
    let mut projects = BTreeMap::new();
    if let Some(value) = config.get("projects") {
        for (path, project) in value
            .as_object()
            .context("Claude projects is not an object")?
        {
            ensure!(project.is_object(), "Claude project is not an object");
            let mcp = servers(project.get("mcpServers"))?;
            if mcp != json!({}) {
                projects.insert(path, mcp);
            }
        }
    }
    Ok(json!({"mcpServers":servers(config.get("mcpServers"))?,"projects":projects}))
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => json!(
            map.iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<BTreeMap<_, _>>()
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// Only lifecycle fields observed changing before the run can be excluded.
/// Ownership, identities, mounts, networks and stable lifecycle fields stay strict.
pub fn exclusions(first: &Value, second: &Value, run_owner: Option<&str>) -> Result<Value> {
    let mut excluded = serde_json::Map::new();
    for (id, old) in first["containers"]
        .as_object()
        .context("container inventory missing")?
    {
        let Some(new) = second["containers"].get(id) else {
            continue;
        };
        if run_owner.is_some_and(|owner| {
            old["owner"].as_str() == Some(owner) || new["owner"].as_str() == Some(owner)
        }) {
            continue;
        }
        let fields: Vec<_> = LIFECYCLE_FIELDS
            .into_iter()
            .filter(|field| {
                old.get(*field).is_some() && new.get(*field).is_some() && old[*field] != new[*field]
            })
            .collect();
        if !fields.is_empty() {
            excluded.insert(id.clone(), json!(fields));
        }
    }
    let excluded = Value::Object(excluded);
    verify_with_exclusions(first, second, &excluded)?;
    Ok(excluded)
}

pub fn verify_with_exclusions(before: &Value, after: &Value, excluded: &Value) -> Result<()> {
    fn normalize(snapshot: &Value, excluded: &Value) -> Result<Value> {
        let mut result = snapshot.clone();
        for (id, fields) in excluded
            .as_object()
            .context("invalid preservation exclusions")?
        {
            for field in fields.as_array().context("invalid excluded fields")? {
                let field = field.as_str().context("invalid excluded field")?;
                ensure!(
                    LIFECYCLE_FIELDS.contains(&field),
                    "cannot exclude resource identity or configuration"
                );
                // A missing container/field must remain detectable, not be fabricated.
                if let Some(container) = result["containers"]
                    .get_mut(id)
                    .and_then(Value::as_object_mut)
                {
                    if let Some(value) = container.get_mut(field) {
                        *value = Value::Null;
                    }
                }
            }
        }
        Ok(result)
    }
    verify(&normalize(before, excluded)?, &normalize(after, excluded)?)
}

pub fn snapshot(checkout_home: Option<&Path>) -> Result<Value> {
    let mut containers = BTreeMap::new();
    for id in docker(&["ps", "-a", "--no-trunc", "--format", "{{.ID}}"])?.lines() {
        let mut value: Value = serde_json::from_str(&docker(&[
            "inspect",
            "--type",
            "container",
            "--format",
            r#"{"id":{{json .Id}},"owner":{{json (index .Config.Labels "proofstorm.dev/installation")}},"running":{{json .State.Running}},"restarting":{{json .State.Restarting}},"started":{{json .State.StartedAt}},"restarts":{{json .RestartCount}},"mounts":{{json .Mounts}},"networks":{{json .NetworkSettings.Networks}}}"#,
            id,
        ])?)?;
        // Mount ordering is not part of identity.
        value["mounts"]
            .as_array_mut()
            .context("mount inventory missing")?
            .sort_by_cached_key(Value::to_string);
        containers.insert(id.to_owned(), value);
    }
    let mut networks: Vec<_> = docker(&["network", "ls", "--no-trunc", "--format", "{{.ID}}"])?
        .lines()
        .map(str::to_owned)
        .collect();
    networks.sort();
    let mut volumes: Vec<_> = docker(&["volume", "ls", "--format", "{{.Name}}"])?
        .lines()
        .map(str::to_owned)
        .collect();
    volumes.sort();
    let mut files = BTreeMap::new();
    if let Some(home) = std::env::var_os("HOME") {
        for name in [
            ".kube/config",
            ".codex/config.toml",
            ".claude.json",
            ".config/opencode/opencode.json",
        ] {
            let path = Path::new(&home).join(name);
            files.insert(path.display().to_string(), configuration(&path)?);
        }
    }
    if let Some(home) = checkout_home {
        for name in [
            "installation.json",
            "runtime-owner.json",
            "kubeconfig",
            "proofstorm.sqlite3",
            "proofstorm.sqlite3-wal",
        ] {
            let path = home.join(name);
            files.insert(path.display().to_string(), configuration(&path)?);
        }
    }
    Ok(
        json!({"containers":containers,"networks":networks,"volumes":volumes,"configuration_sha256":files}),
    )
}

pub fn verify(before: &Value, after: &Value) -> Result<()> {
    ensure!(
        before == after,
        "preexisting Docker resources or configuration changed: {}; compare private preservation snapshots for identities",
        differences(before, after)
    );
    Ok(())
}

/// Counts only: resource identities, paths and configuration contents stay private.
fn differences(before: &Value, after: &Value) -> Value {
    let mut summary = serde_json::Map::new();
    for key in ["containers", "networks", "volumes", "configuration_sha256"] {
        let mut added = 0;
        let mut removed = 0;
        let mut changed = 0;
        match (&before[key], &after[key]) {
            (Value::Object(old), Value::Object(new)) => {
                added = new.keys().filter(|id| !old.contains_key(*id)).count();
                removed = old.keys().filter(|id| !new.contains_key(*id)).count();
                changed = old
                    .iter()
                    .filter(|(id, value)| new.get(*id).is_some_and(|new| new != *value))
                    .count();
            }
            (Value::Array(old), Value::Array(new)) => {
                added = new.iter().filter(|id| !old.contains(id)).count();
                removed = old.iter().filter(|id| !new.contains(id)).count();
            }
            (old, new) if old != new => changed = 1,
            _ => {}
        }
        if added + removed + changed > 0 {
            summary.insert(
                key.into(),
                json!({"added":added,"removed":removed,"changed":changed}),
            );
        }
    }
    summary.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_session_metadata_is_ignored_but_every_mcp_map_is_preserved() {
        let before = json!({"mcpServers":{"global":{"command":"proofstorm","env":{"B":"2","A":"1"}}},"projects":{"/one":{"mcpServers":{"local":{"args":["a","b"]}},"lastSessionId":"old"}},"numStartups":1});
        let mut after = before.clone();
        after["numStartups"] = json!(2);
        after["projects"]["/one"]["lastSessionId"] = json!("new");
        after["projects"]["/new-session"] = json!({"mcpServers":{},"lastCost":3});
        assert_eq!(
            claude_mcp_configuration(&before).unwrap(),
            claude_mcp_configuration(&after).unwrap()
        );
        for pointer in [
            "/mcpServers/global/command",
            "/projects/~1one/mcpServers/local/args",
        ] {
            let mut changed = after.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("changed");
            assert_ne!(
                claude_mcp_configuration(&before).unwrap(),
                claude_mcp_configuration(&changed).unwrap()
            );
        }
        after["projects"]["/new-session"]["mcpServers"] = json!({"new":{"command":"other"}});
        assert_ne!(
            claude_mcp_configuration(&before).unwrap(),
            claude_mcp_configuration(&after).unwrap()
        );
        assert!(claude_mcp_configuration(&json!({"projects":[]})).is_err());
        assert!(claude_mcp_configuration(&json!({"mcpServers":"invalid"})).is_err());
    }

    #[test]
    fn claude_hash_is_normalized_while_other_files_remain_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let empty = configuration(&path).unwrap();
        fs::write(&path, r#"{"numStartups":1,"projects":{"/new":{}}}"#).unwrap();
        assert_eq!(empty, configuration(&path).unwrap());
        fs::write(
            &path,
            r#"{"mcpServers":{"b":{},"a":{"env":{"X":"1","Y":"2"}}},"numStartups":1}"#,
        )
        .unwrap();
        let before = configuration(&path).unwrap();
        fs::write(
            &path,
            r#"{ "numStartups":2,"mcpServers":{"a":{"env":{"Y":"2","X":"1"}},"b":{}}}"#,
        )
        .unwrap();
        assert_eq!(before, configuration(&path).unwrap());
        let other = dir.path().join("config.toml");
        fs::write(&other, "first").unwrap();
        let before = configuration(&other).unwrap();
        fs::write(&other, "second").unwrap();
        assert_ne!(before, configuration(&other).unwrap());
    }

    fn inventory(restarts: u64, started: &str) -> Value {
        json!({"containers":{"external":{"id":"external","owner":null,"running":true,"restarts":restarts,"started":started,"mounts":["volume"],"networks":{"net":{}}},"stable":{"id":"stable","restarts":0,"started":"original"}},"networks":["net"],"volumes":["volume"],"configuration_sha256":{}})
    }

    #[test]
    fn only_preobserved_external_lifecycle_drift_is_excluded() {
        let first = inventory(1, "first");
        let before = inventory(2, "second");
        let excluded = exclusions(&first, &before, None).unwrap();
        assert_eq!(excluded, json!({"external":["started","restarts"]}));
        let after = inventory(9, "later");
        verify_with_exclusions(&before, &after, &excluded).unwrap();
        for pointer in [
            "/containers/external/id",
            "/containers/external/owner",
            "/containers/external/mounts",
            "/containers/external/networks",
            "/containers/external/running",
            "/containers/stable/restarts",
            "/volumes",
            "/networks",
        ] {
            let mut changed = after.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("drift");
            assert!(
                verify_with_exclusions(&before, &changed, &excluded).is_err(),
                "{pointer}"
            );
        }
        let mut missing = after.clone();
        missing["containers"]
            .as_object_mut()
            .unwrap()
            .remove("external");
        assert!(verify_with_exclusions(&before, &missing, &excluded).is_err());
        assert!(verify_with_exclusions(&before, &after, &json!({"external":["mounts"]})).is_err());
    }

    #[test]
    fn stable_and_owned_resources_never_gain_exclusions() {
        let first = inventory(1, "first");
        assert_eq!(exclusions(&first, &first, None).unwrap(), json!({}));
        assert!(verify_with_exclusions(&first, &inventory(2, "second"), &json!({})).is_err());
        let mut owned = first.clone();
        owned["containers"]["external"]["owner"] = json!("this-run");
        let mut changed = owned.clone();
        changed["containers"]["external"]["restarts"] = json!(2);
        assert!(exclusions(&owned, &changed, Some("this-run")).is_err());
        let mut structural = first.clone();
        structural["containers"]["external"]["mounts"] = json!([]);
        assert!(exclusions(&first, &structural, None).is_err());
    }

    #[test]
    fn restart_backoff_needs_an_observed_transition_not_an_inferred_exemption() {
        let mut first = inventory(1, "first");
        first["containers"]["external"]["restarting"] = json!(true);
        assert!(awaiting_restart(&first, &first));
        assert_eq!(exclusions(&first, &first, None).unwrap(), json!({}));
        let mut next = first.clone();
        next["containers"]["external"]["restarts"] = json!(2);
        next["containers"]["external"]["started"] = json!("second");
        assert!(!awaiting_restart(&first, &next));
        assert_eq!(
            exclusions(&first, &next, None).unwrap(),
            json!({"external":["started","restarts"]})
        );
        first["containers"]["external"]["restarting"] = json!(false);
        assert!(!awaiting_restart(&first, &first));
    }
    #[test]
    fn drift_is_reported_not_repaired_or_ignored() {
        let before = json!({"containers":{"id":{"restarts":0}},"configuration_sha256":{"config":"original"}});
        verify(&before, &before).unwrap();
        let mut after = before.clone();
        after["containers"]["id"]["restarts"] = json!(1);
        assert!(verify(&before, &after).is_err());
        let mut after = before.clone();
        after["configuration_sha256"]["config"] = json!("changed");
        assert!(verify(&before, &after).is_err());
    }

    #[test]
    fn drift_summary_identifies_leaks_without_exposing_private_values() {
        let before = json!({"containers":{},"volumes":["private-volume"],"networks":[],"configuration_sha256":{"private-path":"private-hash"}});
        let after = json!({"containers":{},"volumes":["private-volume","private-leak"],"networks":[],"configuration_sha256":{"private-path":"changed-private-hash"}});
        assert_eq!(
            differences(&before, &after),
            json!({"volumes":{"added":1,"removed":0,"changed":0},"configuration_sha256":{"added":0,"removed":0,"changed":1}})
        );
        let message = verify(&before, &after).unwrap_err().to_string();
        for private in [
            "private-volume",
            "private-leak",
            "private-path",
            "private-hash",
        ] {
            assert!(!message.contains(private));
        }
    }
}
