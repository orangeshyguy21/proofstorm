use super::common::{
    component_pod, observe_mint_reachability, pod_can_reach_mint, scoped, submit_idempotent,
    wait_reachability,
};
use super::support::ControllerPause;
use crate::{GateContext, McpClient, cell, json as expect};
use anyhow::{Result, bail};
use serde_json::json;
use std::{thread::sleep, time::Duration};

pub(super) fn run(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    let kubectl = &context.kubectl;
    // --- network partition, restart recovery, selective heal ---------------
    let wallet_pod = component_pod(context, namespace, "wallet")?;
    let receiver_pod = component_pod(context, namespace, "receiver-wallet")?;
    let mut observations: Vec<String> = Vec::new();

    if !pod_can_reach_mint(context, namespace, &wallet_pod)?
        || !pod_can_reach_mint(context, namespace, &receiver_pod)?
    {
        bail!("wallets could not reach mint before the requested partitions");
    }
    observe_mint_reachability(
        client,
        "reachability-baseline",
        "wallet",
        true,
        &mut observations,
    )?;

    submit_idempotent(
        client,
        "network_partition",
        scoped(
            "wallet-mint-partition",
            json!({"from_component": "wallet", "to_component": "mint", "idempotency_key": "wallet-mint-partition-slice5"}),
        ),
        "network partition",
    )?;
    let partitioned = cell::wait_operation(client, "wallet-mint-partition", 120)?;
    let partition_content = cell::artifact_content(&partitioned)?;
    if !expect::boolean(partition_content, "/partitioned")?
        || expect::string(partition_content, "/from_component")? != "wallet"
        || expect::string(partition_content, "/to_component")? != "mint"
        || expect::integer(partition_content, "/active_partition_count")? != 1
    {
        bail!("network partition artifact is invalid: {partitioned}");
    }
    wait_reachability(
        context,
        namespace,
        &wallet_pod,
        false,
        "CNI continued to pass wallet-to-mint traffic after partition",
    )?;
    if !pod_can_reach_mint(context, namespace, &receiver_pod)? {
        bail!("wallet-to-mint partition also blocked the independent receiver wallet");
    }
    observe_mint_reachability(
        client,
        "reachability-wallet-blocked",
        "wallet",
        false,
        &mut observations,
    )?;
    observe_mint_reachability(
        client,
        "reachability-receiver-open",
        "receiver-wallet",
        true,
        &mut observations,
    )?;

    client.call(
        "network_partition",
        scoped(
            "receiver-wallet-mint-partition",
            json!({"from_component": "receiver-wallet", "to_component": "mint", "idempotency_key": "receiver-wallet-mint-partition-slice5"}),
        ),
    )?;
    let receiver_partitioned = cell::wait_operation(client, "receiver-wallet-mint-partition", 120)?;
    let receiver_content = cell::artifact_content(&receiver_partitioned)?;
    if !expect::boolean(receiver_content, "/partitioned")?
        || expect::string(receiver_content, "/from_component")? != "receiver-wallet"
        || expect::string(receiver_content, "/to_component")? != "mint"
        || expect::integer(receiver_content, "/active_partition_count")? != 2
    {
        bail!("overlapping network partition artifact is invalid: {receiver_partitioned}");
    }
    wait_reachability(
        context,
        namespace,
        &receiver_pod,
        false,
        "CNI continued to pass receiver-wallet-to-mint traffic after partition",
    )?;
    observe_mint_reachability(
        client,
        "reachability-receiver-blocked",
        "receiver-wallet",
        false,
        &mut observations,
    )?;

    let controller_before = kubectl.controller_pod_uid()?;
    let pause = ControllerPause::stop(kubectl)?;
    kubectl.run(&[
        "delete",
        "networkpolicy",
        "default-deny-all",
        "wallet",
        "receiver-wallet",
        "mint",
        "-n",
        namespace,
        "--wait=true",
    ])?;
    let mut restored = false;
    for _ in 0..30 {
        if pod_can_reach_mint(context, namespace, &wallet_pod)?
            && pod_can_reach_mint(context, namespace, &receiver_pod)?
        {
            restored = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !restored {
        bail!("removing fault policies while proofstormd was stopped did not restore traffic");
    }
    pause.resume()?;
    if kubectl.controller_pod_uid()? == controller_before {
        bail!("network-fault persistence check did not replace the proofstormd pod");
    }
    let mut reconstructed = false;
    for _ in 0..30 {
        if !pod_can_reach_mint(context, namespace, &wallet_pod)?
            && !pod_can_reach_mint(context, namespace, &receiver_pod)?
        {
            reconstructed = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !reconstructed {
        bail!("proofstormd restart did not reconstruct both active partitions");
    }
    observe_mint_reachability(
        client,
        "reachability-wallet-reconstructed",
        "wallet",
        false,
        &mut observations,
    )?;
    observe_mint_reachability(
        client,
        "reachability-receiver-reconstructed",
        "receiver-wallet",
        false,
        &mut observations,
    )?;

    client.call(
        "network_heal",
        scoped(
            "wallet-mint-heal",
            json!({"partition_operation_id": "wallet-mint-partition", "idempotency_key": "wallet-mint-heal-slice5"}),
        ),
    )?;
    let healed = cell::wait_operation(client, "wallet-mint-heal", 120)?;
    let heal_content = cell::artifact_content(&healed)?;
    if !expect::boolean(heal_content, "/healed")?
        || expect::string(heal_content, "/partition_operation_id")? != "wallet-mint-partition"
        || expect::integer(heal_content, "/active_partition_count")? != 1
    {
        bail!("network heal artifact is invalid: {healed}");
    }
    wait_reachability(
        context,
        namespace,
        &wallet_pod,
        true,
        "wallet-to-mint traffic did not recover after heal",
    )?;
    if pod_can_reach_mint(context, namespace, &receiver_pod)? {
        bail!("healing one partition also healed the overlapping receiver partition");
    }
    observe_mint_reachability(
        client,
        "reachability-wallet-healed",
        "wallet",
        true,
        &mut observations,
    )?;
    observe_mint_reachability(
        client,
        "reachability-receiver-still-blocked",
        "receiver-wallet",
        false,
        &mut observations,
    )?;

    client.call(
        "network_heal",
        scoped(
            "receiver-wallet-mint-heal",
            json!({"partition_operation_id": "receiver-wallet-mint-partition", "idempotency_key": "receiver-wallet-mint-heal-slice5"}),
        ),
    )?;
    let receiver_healed = cell::wait_operation(client, "receiver-wallet-mint-heal", 120)?;
    let receiver_heal_content = cell::artifact_content(&receiver_healed)?;
    if !expect::boolean(receiver_heal_content, "/healed")?
        || expect::string(receiver_heal_content, "/partition_operation_id")?
            != "receiver-wallet-mint-partition"
        || expect::integer(receiver_heal_content, "/active_partition_count")? != 0
    {
        bail!("overlapping network heal artifact is invalid: {receiver_healed}");
    }
    let mut both_back = false;
    for _ in 0..30 {
        if pod_can_reach_mint(context, namespace, &wallet_pod)?
            && pod_can_reach_mint(context, namespace, &receiver_pod)?
        {
            both_back = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !both_back {
        bail!("receiver-wallet-to-mint traffic did not recover after its targeted heal");
    }
    observe_mint_reachability(
        client,
        "reachability-receiver-healed",
        "receiver-wallet",
        true,
        &mut observations,
    )?;

    Ok(())
}
