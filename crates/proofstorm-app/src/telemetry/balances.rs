//! Fixed, passive commands. Raw RPC output and errors never enter the public view.
use futures::{StreamExt, stream};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, ResourceExt, api::AttachParams};
use proofstorm_kube::{COMPONENT_LABEL, ProofstormLab, ROLLOUT_DIGEST_ANNOTATION};
use proofstorm_view::{BalanceAmount, ComponentBalance};
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const CDK_READER: &str = include_str!("../../../proofstorm-kube/drivers/cdk_wallet_balance.py");
const COCO_READER: &str = include_str!("../../../proofstorm-kube/drivers/cocod_wallet_balance.py");

pub(super) async fn sample(
    lab: &ProofstormLab,
    pods: &Api<Pod>,
    inventory: &[Pod],
) -> Vec<ComponentBalance> {
    stream::iter(lab.spec.lab.components.iter().filter(|c| {
        matches!(
            c.implementation.as_str(),
            "lnd" | "cln" | "cdk-cli-wallet" | "cocod-wallet" | "nutshell-wallet"
        )
    }))
    .map(|component| async move {
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
                && lab
                    .spec
                    .lock
                    .entries
                    .iter()
                    .find(|entry| entry.component_id == component.id)
                    .is_some_and(|entry| {
                        pod.annotations().get(ROLLOUT_DIGEST_ANNOTATION)
                            == Some(&entry.rollout_digest)
                    })
        });
        let amounts = if let Some(pod) = pod {
            observe(
                pods,
                &pod.name_any(),
                &component.id,
                &component.implementation,
            )
            .await
        } else {
            None
        };
        ComponentBalance {
            component: component.id.clone(),
            observed_at_unix: super::now(),
            error: amounts.is_none().then(|| "Balance unavailable".into()),
            amounts: amounts.unwrap_or_default(),
        }
    })
    .buffer_unordered(4)
    .collect()
    .await
}

async fn observe(
    pods: &Api<Pod>,
    pod: &str,
    component: &str,
    implementation: &str,
) -> Option<Vec<BalanceAmount>> {
    match implementation {
        "lnd" => {
            let base = ["lncli", "--lnddir=/home/lnd/.lnd", "--network=regtest"];
            let wallet = read(
                pods,
                pod,
                base.into_iter()
                    .chain(["walletbalance"])
                    .map(str::to_owned)
                    .collect(),
            )
            .await?;
            let channels = read(
                pods,
                pod,
                base.into_iter()
                    .chain(["channelbalance"])
                    .map(str::to_owned)
                    .collect(),
            )
            .await?;
            Some(vec![
                amount("On-chain", integer(&wallet["total_balance"])?),
                amount("Local", integer(&channels["local_balance"]["sat"])?),
                amount("Remote", integer(&channels["remote_balance"]["sat"])?),
            ])
        }
        "cln" => {
            let funds = read(
                pods,
                pod,
                [
                    "lightning-cli",
                    "--lightning-dir=/home/cln/.lightning",
                    "--network=regtest",
                    "listfunds",
                ]
                .map(str::to_owned)
                .to_vec(),
            )
            .await?;
            cln_amounts(&funds)
        }
        "cdk-cli-wallet" | "cocod-wallet" => {
            let (reader, database, query) = if implementation == "cdk-cli-wallet" {
                (
                    CDK_READER,
                    "/wallet/cdk/cdk-cli.sqlite",
                    "SELECT DISTINCT mint_url FROM proof WHERE unit='sat'",
                )
            } else {
                (
                    COCO_READER,
                    "/wallet/.cocod/coco.db",
                    "SELECT mintUrl FROM coco_cashu_mints",
                )
            };
            // Reuse the pinned readers, invoking their functions without their CLI entrypoints.
            let script = format!(
                "__name__='dashboard_reader'\n{reader}\nimport sys\ndatabase={database:?}\nwith sqlite3.connect(Path(database).as_uri()+'?mode=ro', uri=True, timeout=1) as db:\n urls=[r[0] for r in db.execute({query:?})]\nrows=[observe(database,sys.argv[1],'',url) for url in urls]\nprint(json.dumps({{k:sum(r.get(k,0) for r in rows) for k in ['balance_sat','reserved_sat','pending_sat','inflight_sat']}}))"
            );
            let result = read(
                pods,
                pod,
                vec!["python3".into(), "-c".into(), script, component.into()],
            )
            .await?;
            Some(vec![
                amount("Spendable", integer(&result["balance_sat"])?),
                amount("Reserved", integer(&result["reserved_sat"])?),
                amount(
                    "Pending",
                    integer(&result["pending_sat"])?
                        .checked_add(integer(&result["inflight_sat"])?)?,
                ),
            ])
        }
        "nutshell-wallet" => {
            let result = read(
                pods,
                pod,
                vec![
                    "python3".into(),
                    "-c".into(),
                    include_str!("nutshell_balance.py").into(),
                    component.into(),
                ],
            )
            .await?;
            Some(vec![
                amount("Spendable", integer(&result["balance_sat"])?),
                amount("Reserved", integer(&result["reserved_sat"])?),
            ])
        }
        _ => None,
    }
}

fn amount(label: &str, sat: u64) -> BalanceAmount {
    BalanceAmount {
        label: label.into(),
        sat,
    }
}
fn integer(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn millisats(value: &Value) -> Option<u64> {
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
        let values = cln_amounts(&serde_json::json!({"outputs":[{"status":"confirmed","amount_msat":120000},{"status":"unconfirmed","amount_msat":99000}],"channels":[{"state":"CHANNELD_NORMAL","our_amount_msat":"25000msat","amount_msat":100000},{"state":"CLOSINGD_COMPLETE","our_amount_msat":999000,"amount_msat":999000}]})).unwrap();
        assert_eq!(
            values.iter().map(|v| v.sat).collect::<Vec<_>>(),
            vec![120, 25, 75]
        );
        assert!(cln_amounts(&serde_json::json!({"outputs":[],"channels":[{"state":"CHANNELD_NORMAL","our_amount_msat":2000,"amount_msat":1000}]})).is_none());
    }
}
