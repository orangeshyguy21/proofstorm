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
    if !path.try_exists()? {
        return Ok(Value::Null);
    }
    Ok(json!(format!("{:x}", Sha256::digest(fs::read(path)?))))
}

pub fn snapshot(checkout_home: Option<&Path>) -> Result<Value> {
    let mut containers = BTreeMap::new();
    for id in docker(&["ps", "-a", "--no-trunc", "--format", "{{.ID}}"])?.lines() {
        let mut value: Value = serde_json::from_str(&docker(&[
            "inspect",
            "--type",
            "container",
            "--format",
            r#"{"id":{{json .Id}},"running":{{json .State.Running}},"started":{{json .State.StartedAt}},"restarts":{{json .RestartCount}},"mounts":{{json .Mounts}},"networks":{{json .NetworkSettings.Networks}}}"#,
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
        "preexisting Docker resources or configuration changed; compare preservation-before.json and preservation-after.json (no repair/adoption attempted)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
