//! Fixed, passive commands. Raw RPC output and errors never enter the public view.
use futures::{StreamExt, stream};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, ResourceExt, api::AttachParams};
use proofstorm_kube::{COMPONENT_LABEL, ProofstormLab, ROLLOUT_DIGEST_ANNOTATION};
use proofstorm_view::{BalanceAmount, ComponentBalance, HoldingsObservation, LightningObservation};
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(super) async fn sample(
    lab: &ProofstormLab,
    pods: &Api<Pod>,
    inventory: &[Pod],
) -> Vec<ComponentBalance> {
    stream::iter(
        lab.spec
            .lab
            .components
            .iter()
            .filter(|c| {
                matches!(
                    c.implementation.as_str(),
                    "bitcoin-core"
                        | "lnd"
                        | "cln"
                        | "cdk-cli-wallet"
                        | "cocod-wallet"
                        | "nutshell-wallet"
                )
            })
            .cloned()
            .collect::<Vec<_>>(),
    )
    .map(|component| async move {
        let entry = lab
            .spec
            .lock
            .entries
            .iter()
            .find(|e| e.component_id == component.id);
        let pod = inventory.iter().find(|pod| {
            pod.labels().get(COMPONENT_LABEL) == Some(&component.id)
                && pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                    .is_some_and(|statuses| {
                        statuses.iter().any(|s| s.name == "component" && s.ready)
                    })
                && entry.is_some_and(|entry| {
                    pod.annotations().get(ROLLOUT_DIGEST_ANNOTATION) == Some(&entry.rollout_digest)
                        && matches_adapter(
                            &component.implementation,
                            entry.protocol_action_adapter_version.as_deref(),
                        )
                })
        });
        let mut result = ComponentBalance {
            component: component.id.clone(),
            rollout_digest: entry.map(|e| e.rollout_digest.clone()),
            observed_at_unix: super::now(),
            error: Some("Observation unavailable".into()),
            amounts: vec![],
            block_height: None,
            lightning: matches!(component.implementation.as_str(), "lnd" | "cln").then(|| {
                LightningObservation {
                    error: Some("Channel observation unavailable".into()),
                    ..Default::default()
                }
            }),
            holdings: component
                .implementation
                .ends_with("wallet")
                .then(|| HoldingsObservation {
                    error: Some("Holdings observation unavailable".into()),
                    ..Default::default()
                }),
        };
        if let Some(pod) = pod {
            observe(
                lab,
                pods,
                &pod.name_any(),
                &component.implementation,
                &mut result,
            )
            .await;
        }
        result
    })
    .buffer_unordered(4)
    .collect()
    .await
}
fn matches_adapter(implementation: &str, version: Option<&str>) -> bool {
    match implementation {
        "cdk-cli-wallet" => version == Some("cdk-cli/0.18/observations/v1"),
        "cocod-wallet" => version == Some("cocod/44e5101c/observations/v1"),
        "nutshell-wallet" => version == Some("0.1.0-alpha.1"),
        _ => true,
    }
}
async fn observe(
    lab: &ProofstormLab,
    pods: &Api<Pod>,
    pod: &str,
    implementation: &str,
    result: &mut ComponentBalance,
) {
    match implementation {
        "bitcoin-core" => {
            result.block_height = read(
                pods,
                pod,
                vec![
                    "bitcoin-cli".into(),
                    "-regtest".into(),
                    format!("-rpcuser={}", proofstorm_kube::BITCOIN_RPC_USER),
                    format!("-rpcpassword={}", proofstorm_kube::BITCOIN_RPC_PASSWORD),
                    "getblockchaininfo".into(),
                ],
            )
            .await
            .and_then(|v| v["blocks"].as_u64());
            if result.block_height.is_some() {
                result.error = None;
            }
        }
        "lnd" | "cln" => {
            let lnd = implementation == "lnd";
            let command = |action: &str| {
                let base = if lnd {
                    ["lncli", "--lnddir=/home/lnd/.lnd", "--network=regtest"]
                } else {
                    [
                        "lightning-cli",
                        "--lightning-dir=/home/cln/.lightning",
                        "--network=regtest",
                    ]
                };
                base.into_iter()
                    .chain([action])
                    .map(str::to_owned)
                    .collect()
            };
            let (info, channels, funds) = tokio::join!(
                read(pods, pod, command("getinfo")),
                read(
                    pods,
                    pod,
                    command(if lnd {
                        "listchannels"
                    } else {
                        "listpeerchannels"
                    })
                ),
                read(
                    pods,
                    pod,
                    command(if lnd { "walletbalance" } else { "listfunds" })
                )
            );
            if let Some(observation) = info
                .zip(channels)
                .and_then(|(i, c)| super::channels::project(implementation, &i, &c))
            {
                result.lightning = Some(observation);
            }
            let amounts = funds.and_then(|funds| {
                if lnd {
                    lnd_amounts(&funds, result.lightning.as_ref()?)
                } else {
                    cln_amounts(&funds)
                }
            });
            if let Some(amounts) = amounts {
                result.amounts = amounts;
                result.error = None;
            }
        }
        _ => {
            let data = read(
                pods,
                pod,
                vec![
                    "python3".into(),
                    "-c".into(),
                    super::holdings::script(implementation),
                    result.component.clone(),
                ],
            )
            .await;
            if let Some((amounts, holdings)) =
                data.and_then(|v| super::holdings::project(lab, implementation, &v))
            {
                result.amounts = amounts;
                result.holdings = Some(holdings);
                result.error = None;
            }
        }
    }
}
fn lnd_amounts(funds: &Value, observation: &LightningObservation) -> Option<Vec<BalanceAmount>> {
    if observation.error.is_some() {
        return None;
    }
    let (local, remote) =
        observation
            .channels
            .iter()
            .try_fold((0_u64, 0_u64), |(local, remote), c| {
                Some((
                    local.checked_add(c.local_msat)?,
                    remote.checked_add(c.remote_msat)?,
                ))
            })?;
    Some(vec![
        amount("On-chain", integer(&funds["total_balance"])?),
        amount("Local", local / 1000),
        amount("Remote", remote / 1000),
    ])
}

pub(super) fn amount(label: &str, sat: u64) -> BalanceAmount {
    BalanceAmount {
        label: label.into(),
        sat,
    }
}
pub(super) fn integer(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
pub(super) fn millisats(value: &Value) -> Option<u64> {
    integer(value).or_else(|| value.as_str()?.strip_suffix("msat")?.parse().ok())
}
fn cln_amounts(funds: &Value) -> Option<Vec<BalanceAmount>> {
    let onchain = funds["outputs"]
        .as_array()?
        .iter()
        .filter(|o| o["status"] == "confirmed")
        .try_fold(0_u64, |sum, output| {
            sum.checked_add(millisats(&output["amount_msat"])?)
        })?;
    let (local, remote) = funds["channels"]
        .as_array()?
        .iter()
        .filter(|c| c["state"] == "CHANNELD_NORMAL")
        .try_fold((0_u64, 0_u64), |(local, remote), channel| {
            let ours = millisats(&channel["our_amount_msat"])?;
            let total = millisats(&channel["amount_msat"])?;
            Some((
                local.checked_add(ours)?,
                remote.checked_add(total.checked_sub(ours)?)?,
            ))
        })?;
    Some(vec![
        amount("On-chain", onchain / 1000),
        amount("Local", local / 1000),
        amount("Remote", remote / 1000),
    ])
}

async fn read(pods: &Api<Pod>, pod: &str, command: Vec<String>) -> Option<Value> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut process = pods
            .exec(
                pod,
                command,
                &AttachParams::default()
                    .container("component")
                    .stdin(false)
                    .stdout(true)
                    .stderr(false),
            )
            .await
            .ok()?;
        let mut stdout = process.stdout()?.take(131_073);
        let status = process.take_status()?;
        let (bytes, status) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.ok()?;
                Some(bytes)
            },
            status
        );
        let bytes = bytes?;
        if bytes.len() > 131_072 || status?.status.as_deref() != Some("Success") {
            return None;
        }
        process.join().await.ok()?;
        serde_json::from_slice(&bytes).ok()
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cln_preserves_units_and_excludes_closed_channels_and_unconfirmed_outputs() {
        let values = cln_amounts(&serde_json::json!({"outputs":[{"status":"confirmed","amount_msat":120_000},{"status":"unconfirmed","amount_msat":99000}],"channels":[{"state":"CHANNELD_NORMAL","our_amount_msat":"25000msat","amount_msat":100_000},{"state":"CLOSINGD_COMPLETE","our_amount_msat":999_000,"amount_msat":999_000}]})).unwrap();
        assert_eq!(
            values.iter().map(|v| v.sat).collect::<Vec<_>>(),
            vec![120, 25, 75]
        );
        assert!(cln_amounts(&serde_json::json!({"outputs":[],"channels":[{"state":"CHANNELD_NORMAL","our_amount_msat":2000,"amount_msat":1000}]})).is_none());
    }
}
