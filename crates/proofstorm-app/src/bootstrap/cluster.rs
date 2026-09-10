use super::{docker, process, tool, tools};
use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    net::{Ipv4Addr, TcpListener},
};

pub(super) fn nodes(installation: &Installation) -> Vec<String> {
    vec![
        format!("{}-server-0", installation.context()),
        format!("{}-agent-0", installation.context()),
    ]
}

fn names(installation: &Installation) -> Vec<String> {
    let mut names = nodes(installation);
    names.extend([
        format!("{}-serverlb", installation.context()),
        installation.registry_name(),
    ]);
    names
}

fn inventory(installation: &Installation) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for name in names(installation) {
        // Select only the ownership label, never dump k3d's credential-bearing labels.
        let output = docker(
            &installation.home,
            &[
                "inspect",
                "--type",
                "container",
                "--format",
                "{{.Id}}|{{index .Config.Labels \"proofstorm.dev/installation\"}}|{{.State.Running}}",
                &name,
            ],
            15,
        )?;
        let values: Vec<_> = output.trim().split('|').collect();
        ensure!(
            values.len() == 3 && values[2] == "true",
            "runtime container {name} is stopped or malformed; inspect it before retrying"
        );
        if name != installation.registry_name() {
            ensure!(
                values[1] == installation.id,
                "refusing foreign runtime container {name}"
            );
        }
        result.insert(name, values[0].into());
    }
    Ok(result)
}

fn network(installation: &Installation) -> Result<String> {
    Ok(docker(
        &installation.home,
        &[
            "network",
            "inspect",
            "--format",
            "{{.Id}}",
            &installation.network_name(),
        ],
        15,
    )?
    .trim()
    .into())
}

pub(super) fn owned(installation: &Installation) -> Result<()> {
    let receipt: Value = serde_json::from_slice(
        &fs::read(installation.home.join("runtime-owner.json")).context(
            "runtime ownership receipt missing; setup will not adopt existing Docker resources",
        )?,
    )?;
    ensure!(
        receipt["installation_id"] == installation.id
            && receipt["containers"] == serde_json::to_value(inventory(installation)?)?
            && receipt["network_id"] == network(installation)?,
        "runtime ownership changed; refusing to modify foreign or replaced resources"
    );
    Ok(())
}

pub(super) fn verify_kubeconfig(installation: &Installation) -> Result<()> {
    let receipt: Value =
        serde_json::from_slice(&fs::read(installation.home.join("runtime-owner.json"))?)?;
    ensure!(
        receipt["installation_id"] == installation.id
            && fs::symlink_metadata(installation.kubeconfig())?.is_file()
            && receipt["kubeconfig_sha256"] == tools::hash(&installation.kubeconfig())?,
        "private kubeconfig changed; refusing any Kubernetes operation"
    );
    Ok(())
}

pub(super) fn create(installation: &Installation, progress: &dyn Fn(&str)) -> Result<()> {
    let home = &installation.home;
    let receipt_path = home.join("runtime-owner.json");
    if receipt_path.exists() {
        progress("Verifying existing Kubernetes runtime");
        owned(installation)?;
    } else {
        progress("Checking local runtime ports and ownership");
        // Successful inventory queries are required before interpreting absence.
        let containers = docker(home, &["ps", "-a", "--format", "{{.Names}}"], 15)?;
        let networks = docker(home, &["network", "ls", "--format", "{{.Name}}"], 15)?;
        ensure!(
            !names(installation)
                .iter()
                .any(|name| containers.lines().any(|line| line == name))
                && !networks
                    .lines()
                    .any(|line| line == installation.network_name()),
            "unrecorded runtime resources already exist (possibly interrupted creation); refusing adoption or deletion; inspect before retrying"
        );
        let api = TcpListener::bind((Ipv4Addr::LOCALHOST, installation.api_port))
            .context("saved API port is occupied")?;
        let registry = TcpListener::bind((Ipv4Addr::LOCALHOST, installation.registry_port))
            .context("saved registry port is occupied")?;
        drop((api, registry));
        progress("Starting Kubernetes; waiting for nodes");
        process::run(
            home,
            &tool(home, "k3d")?,
            &[
                "cluster",
                "create",
                "--config",
                installation
                    .cluster_config_path()
                    .to_str()
                    .context("config path")?,
                "--kubeconfig-update-default=false",
                "--kubeconfig-switch-context=false",
            ],
            180,
        )?;
        let receipt = json!({"format_version":1,"installation_id":installation.id,
            "containers":inventory(installation)?,"network_id":network(installation)?});
        process::save(&receipt_path, &serde_json::to_vec(&receipt)?)?;
    }
    let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    progress("Verifying private cluster connection");
    if !receipt["kubeconfig_sha256"].is_null() {
        verify_kubeconfig(installation)?;
        return Ok(());
    }
    ensure!(
        !installation.kubeconfig().exists(),
        "unrecorded kubeconfig exists; refusing replacement"
    );
    let config = process::run(
        home,
        &tool(home, "k3d")?,
        &["kubeconfig", "get", &installation.cluster_name()],
        15,
    )?;
    process::save(&installation.kubeconfig(), config.as_bytes())?;
    receipt["kubeconfig_sha256"] = json!(tools::hash(&installation.kubeconfig())?);
    process::save(&receipt_path, &serde_json::to_vec(&receipt)?)?;
    Ok(())
}
