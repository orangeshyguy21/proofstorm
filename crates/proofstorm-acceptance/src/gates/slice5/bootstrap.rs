use super::common::{EXPERIMENT, INSTANCE, action_kinds};
use crate::{GateContext, McpClient, gate::CONTROL_NAMESPACE, json as expect, native};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{thread::sleep, time::Duration};

pub(super) fn bootstrap(
    context: &GateContext,
    client: &mut McpClient,
    namespace: &str,
    instance_key: &str,
    restart_controller: bool,
) -> Result<(String, Vec<String>)> {
    let mut operations = Vec::new();
    if restart_controller {
        // A marker fails a replay that starts the same command a second time.
        // Keep the supervised process alive while its controller restarts.
        let script = format!(
            "set -eu; marker=/tmp/proofstorm-recovery-once; test ! -e \"$marker\"; touch \"$marker\"; sleep 30; {} getblockchaininfo",
            native::BITCOIN_ROOT
        );
        let accepted = native::submit(
            client,
            INSTANCE,
            EXPERIMENT,
            "chain",
            "native-recovery",
            &script,
        )?;
        let (resource, execution) = wait_execution(context, instance_key)?;
        let mut launched = false;
        for _ in 0..60 {
            let (exists, _, _) = context.kubectl.try_run(&[
                "exec",
                "-n",
                namespace,
                expect::string(&execution, "/pod")?,
                "-c",
                expect::string(&execution, "/container")?,
                "--",
                "test",
                "-f",
                "/tmp/proofstorm-recovery-once",
            ])?;
            if exists {
                launched = true;
                break;
            }
            sleep(Duration::from_millis(250));
        }
        ensure!(
            launched,
            "native recovery command was fenced but never launched"
        );
        context
            .kubectl
            .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
        native::wait(client, "native-recovery")?;
        let retried = native::submit(
            client,
            INSTANCE,
            EXPERIMENT,
            "chain",
            "native-recovery",
            &script,
        )?;
        ensure!(
            retried["operation_id"] == accepted["operation_id"]
                && retried["sequence"] == accepted["sequence"],
            "restart changed native operation identity"
        );
        let action = context.kubectl.get_json(&[
            "get",
            "proofstormcellaction",
            &resource,
            "-n",
            CONTROL_NAMESPACE,
        ])?;
        ensure!(
            action["status"]["nativeExecution"] == execution,
            "controller restart replaced the supervised execution"
        );
        let jobs = context.kubectl.get_json(&[
            "get",
            "jobs",
            "-n",
            namespace,
            "-l",
            &format!("proofstorm.dev/action={resource}"),
        ])?;
        ensure!(
            expect::array(&jobs, "/items")?.is_empty(),
            "native execution created a workflow Job"
        );
        operations.push("native-recovery".to_owned());
    }
    let (point, bootstrap_operations) = native::bootstrap(
        client,
        INSTANCE,
        EXPERIMENT,
        "bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        5_000_000,
    )?;
    operations.extend(bootstrap_operations);
    Ok((point, operations))
}

fn wait_execution(context: &GateContext, instance_key: &str) -> Result<(String, Value)> {
    for _ in 0..60 {
        let actions = action_kinds(context, instance_key)?;
        let matches: Vec<_> = expect::array(&actions, "/items")?
            .iter()
            .filter(|action| action["spec"]["operationId"] == "native-recovery")
            .collect();
        ensure!(
            matches.len() <= 1,
            "native retry created multiple controller actions"
        );
        if let Some(action) = matches.first()
            && let Some(reference) = action.pointer("/status/nativeExecution")
        {
            return Ok((
                expect::string(action, "/metadata/name")?.to_owned(),
                reference.clone(),
            ));
        }
        sleep(Duration::from_millis(250));
    }
    anyhow::bail!("native recovery command never acquired its execution identity")
}
