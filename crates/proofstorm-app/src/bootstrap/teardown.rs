//! Resource receipts shared by runtime creation and explicit installation retirement.
//! Never infer deletion authority from a cluster name or a Docker label alone.
use super::{docker, process};
use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path};

const RECEIPT: &str = "runtime-resources.json";
pub(super) const RETIRED: &str = "runtime-retired.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Resources {
    format_version: u32,
    installation_id: String,
    daemon_id: String,
    containers: BTreeMap<String, String>,
    network_id: Option<String>,
    volumes: BTreeMap<String, Value>,
}

fn container_names(installation: &Installation) -> Vec<String> {
    ["server-0", "agent-0", "serverlb", "registry", "tools"]
        .map(|suffix| format!("{}-{suffix}", installation.context()))
        .to_vec()
}

fn lines(output: &str) -> Vec<&str> {
    output.lines().filter(|line| !line.is_empty()).collect()
}

fn volume_identity(value: &Value) -> Value {
    // Docker volumes have no immutable ID. Bind the name to its creation metadata
    // and check exclusive container use again before deleting it.
    serde_json::json!({"Name":value["Name"],"CreatedAt":value["CreatedAt"],
        "Driver":value["Driver"],"Labels":value["Labels"],"Options":value["Options"]})
}

fn snapshot(
    installation: &Installation,
    run: &impl Fn(&[&str]) -> Result<String>,
) -> Result<Resources> {
    let daemon_id = run(&["info", "--format", "{{.ID}}"])?.trim().to_owned();
    ensure!(!daemon_id.is_empty(), "Docker daemon identity unavailable");
    let all_names = run(&["ps", "-a", "--format", "{{.Names}}"])?;
    let networks = run(&["network", "ls", "--format", "{{.Name}}"])?;
    let network_id = if lines(&networks).contains(&installation.network_name().as_str()) {
        let data: Value =
            serde_json::from_str(&run(&["network", "inspect", &installation.network_name()])?)?;
        ensure!(data[0]["Labels"]["app"] == "k3d", "foreign runtime network");
        Some(
            data[0]["Id"]
                .as_str()
                .context("network ID missing")?
                .to_owned(),
        )
    } else {
        None
    };
    let mut containers = BTreeMap::new();
    let mut volumes = BTreeMap::new();
    for name in container_names(installation) {
        if !lines(&all_names).contains(&name.as_str()) {
            continue;
        }
        // Select only fields needed for ownership; never capture kubeconfig/token labels.
        let data: Value = serde_json::from_str(&run(&[
            "inspect",
            "--type",
            "container",
            "--format",
            r#"{"id":{{json .Id}},"owner":{{json (index .Config.Labels "proofstorm.dev/installation")}},"cluster":{{json (index .Config.Labels "k3d.cluster")}},"mounts":{{json .Mounts}},"networks":{{json .NetworkSettings.Networks}}}"#,
            &name,
        ])?)?;
        let auxiliary = name == installation.registry_name() || name.ends_with("-tools");
        ensure!(
            data["owner"] == installation.id
                || (auxiliary && data["cluster"] == installation.cluster_name()),
            "foreign runtime container {name}"
        );
        let id = data["id"].as_str().context("container ID missing")?;
        ensure!(!id.is_empty(), "empty container ID");
        containers.insert(name, id.to_owned());
        for mount in data["mounts"]
            .as_array()
            .context("container mounts missing")?
        {
            if mount["Type"] != "volume" {
                continue;
            }
            let name = mount["Name"].as_str().context("volume name missing")?;
            let data: Value = serde_json::from_str(&run(&["volume", "inspect", name])?)?;
            ensure!(
                data[0]["CreatedAt"].as_str().is_some(),
                "volume creation identity missing"
            );
            volumes.insert(name.into(), volume_identity(&data[0]));
        }
    }
    // k3d may leave its named image volume unattached after the tools node exits.
    let image_volume = format!("{}-images", installation.context());
    let names = run(&["volume", "ls", "--format", "{{.Name}}"])?;
    if lines(&names).contains(&image_volume.as_str()) {
        let data: Value = serde_json::from_str(&run(&["volume", "inspect", &image_volume])?)?;
        ensure!(
            data[0]["CreatedAt"].as_str().is_some(),
            "image volume creation identity missing"
        );
        volumes.insert(image_volume, volume_identity(&data[0]));
    }
    Ok(Resources {
        format_version: 1,
        installation_id: installation.id.clone(),
        daemon_id,
        containers,
        network_id,
        volumes,
    })
}

/// Record resources immediately after a create attempt, including failed setup.
/// The caller has already refused every preexisting runtime name.
pub(super) fn record_created(installation: &Installation, old_volumes: &str) -> Result<()> {
    let path = installation.home.join(RECEIPT);
    ensure!(
        !path.try_exists()?,
        "runtime resource receipt already exists; refusing replacement"
    );
    let resources = snapshot(installation, &|args| docker(&installation.home, args, 30))?;
    for name in resources.volumes.keys() {
        ensure!(
            !lines(old_volumes).contains(&name.as_str()),
            "runtime used a preexisting volume {name}; refusing cleanup authority"
        );
    }
    process::save(&path, &serde_json::to_vec(&resources)?)
}

fn read_receipt(installation: &Installation) -> Result<Resources> {
    let path = installation.home.join(RECEIPT);
    let metadata = fs::symlink_metadata(&path)
        .context("runtime resource receipt missing; refusing to adopt resources for deletion")?;
    ensure!(
        metadata.is_file() && metadata.len() < 1024 * 1024,
        "invalid runtime resource receipt"
    );
    let receipt: Resources = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        receipt.format_version == 1 && receipt.installation_id == installation.id,
        "runtime resource receipt belongs to another installation"
    );
    ensure!(
        receipt
            .containers
            .keys()
            .all(|name| container_names(installation).contains(name)),
        "unexpected container in runtime receipt"
    );
    Ok(receipt)
}

fn unchanged(expected: &Resources, current: &Resources) -> Result<()> {
    ensure!(
        expected.installation_id == current.installation_id
            && expected.daemon_id == current.daemon_id,
        "installation or Docker daemon changed; refusing runtime deletion"
    );
    for (name, id) in &current.containers {
        ensure!(
            expected.containers.get(name) == Some(id),
            "foreign or replaced container {name}"
        );
    }
    if current.network_id.is_some() {
        ensure!(
            expected.network_id == current.network_id,
            "foreign or replaced runtime network"
        );
    }
    for (name, identity) in &current.volumes {
        ensure!(
            expected.volumes.get(name) == Some(identity),
            "foreign or replaced volume {name}"
        );
    }
    Ok(())
}

fn exclusive(expected: &Resources, run: &impl Fn(&[&str]) -> Result<String>) -> Result<()> {
    if let Some(network) = &expected.network_id {
        let networks = run(&["network", "ls", "--no-trunc", "--format", "{{.ID}}"])?;
        if lines(&networks).contains(&network.as_str()) {
            let data: Value = serde_json::from_str(&run(&["network", "inspect", network])?)?;
            for id in data[0]["Containers"]
                .as_object()
                .context("network member inventory missing")?
                .keys()
            {
                ensure!(
                    expected.containers.values().any(|owned| owned == id),
                    "foreign container joined the runtime network"
                );
            }
        }
    }
    // Inspect every recorded volume even after its last container was removed.
    let volumes = run(&["volume", "ls", "--format", "{{.Name}}"])?;
    for (name, identity) in &expected.volumes {
        if !lines(&volumes).contains(&name.as_str()) {
            continue;
        }
        let data: Value = serde_json::from_str(&run(&["volume", "inspect", name])?)?;
        ensure!(
            volume_identity(&data[0]) == *identity,
            "foreign or replaced volume {name}"
        );
        let users = run(&[
            "ps",
            "-a",
            "--no-trunc",
            "--filter",
            &format!("volume={name}"),
            "--format",
            "{{.ID}}",
        ])?;
        ensure!(
            lines(&users)
                .iter()
                .all(|id| expected.containers.values().any(|owned| owned == id)),
            "foreign container uses runtime volume {name}"
        );
    }
    Ok(())
}

fn remove(
    expected: &Resources,
    installation: &Installation,
    run: &impl Fn(&[&str]) -> Result<String>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    unchanged(expected, &snapshot(installation, run)?)?;
    exclusive(expected, run)?;
    for (name, id) in &expected.containers {
        let current = snapshot(installation, run)?;
        unchanged(expected, &current)?;
        exclusive(expected, run)?;
        if current.containers.contains_key(name) {
            progress("Removing owned runtime container");
            run(&["rm", "--force", id])?; // No -v: volumes are verified separately.
        }
    }
    unchanged(expected, &snapshot(installation, run)?)?;
    exclusive(expected, run)?;
    if let Some(network) = &expected.network_id {
        let networks = run(&["network", "ls", "--no-trunc", "--format", "{{.ID}}"])?;
        if lines(&networks).contains(&network.as_str()) {
            progress("Removing owned runtime network");
            run(&["network", "rm", network])?;
        }
    }
    for name in expected.volumes.keys() {
        exclusive(expected, run)?;
        let volumes = run(&["volume", "ls", "--format", "{{.Name}}"])?;
        if lines(&volumes).contains(&name.as_str()) {
            progress("Removing owned runtime storage");
            run(&["volume", "rm", name])?;
        }
    }
    let current = snapshot(installation, run)?;
    unchanged(expected, &current)?;
    ensure!(
        current.containers.is_empty() && current.network_id.is_none() && current.volumes.is_empty(),
        "runtime resources remain after deletion"
    );
    Ok(())
}

/// Permanently retire an explicitly identified installation's Docker runtime.
/// State and receipts remain for diagnostics; this home cannot be set up again.
/// Missing receipts fail closed. Retrying a partially completed delete is safe.
pub fn retire(home: &Path, installation_id: &str, progress: &dyn Fn(&str)) -> Result<()> {
    let installation = Installation::load(home)?;
    ensure!(
        installation.id == installation_id,
        "installation identity does not match deletion request"
    );
    let _guard = Installation::lock(&installation.home)?;
    ensure!(
        !installation.home.join("gui-process.json").exists(),
        "stop this installation's GUI before retiring its runtime"
    );
    let expected = read_receipt(&installation)?;
    let owner = installation.home.join("runtime-owner.json");
    if owner.exists() {
        let value: Value = serde_json::from_slice(&fs::read(owner)?)?;
        ensure!(
            value["installation_id"] == installation.id,
            "runtime owner changed"
        );
        if value["kubeconfig_sha256"].is_null() {
            ensure!(
                !installation.kubeconfig().exists(),
                "unrecorded kubeconfig; refusing deletion"
            );
        } else {
            super::cluster::verify_kubeconfig(&installation)?;
        }
    } else {
        ensure!(
            !installation.kubeconfig().exists(),
            "unrecorded kubeconfig; refusing deletion"
        );
    }
    let run = |args: &[&str]| docker(&installation.home, args, 30);
    unchanged(&expected, &snapshot(&installation, &run)?)?;
    exclusive(&expected, &run)?;
    // Save intent before mutating; setup must never restart a partially retired home.
    process::save(
        &installation.home.join(RETIRED),
        &serde_json::to_vec(&serde_json::json!({
        "installation_id":installation.id,"state":"deleting"}))?,
    )?;
    remove(
        &expected,
        &installation,
        &|args| docker(&installation.home, args, 30),
        progress,
    )?;
    process::save(
        &installation.home.join(RETIRED),
        &serde_json::to_vec(&serde_json::json!({
        "installation_id":installation.id,"state":"deleted"}))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
