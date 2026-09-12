//! Real spent-proof replay/race checks, not operation-idempotency checks.
use std::{fs, path::Path, thread::sleep, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{EXPERIMENT_CAPABILITIES, GateContext, McpClient, cell, json as expect};

const INSTANCE: &str = "proof-spend";
const DRIVER: &str = include_str!("../../drivers/cashu_double_spend.sh");
const BALANCE_OBSERVER: &str =
    include_str!("../../../proofstorm-kube/drivers/cdk_wallet_balance.py");

fn driver() -> String {
    DRIVER.replace("__PROOFSTORM_BALANCE_OBSERVER__", BALANCE_OBSERVER)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    before: u64,
    first: u64,
    after_replay: u64,
    replay_rc: u32,
    fresh_rc: u32,
    fresh_spent: bool,
    fresh_balance: u64,
    race_rc: [u32; 2],
    race_spent: [bool; 2],
    race_balance: [u64; 2],
    source_after: u64,
}

impl Receipt {
    fn verify(&self) -> Result<()> {
        ensure!(
            self.before == 64 && self.first == 32,
            "issuance or first redemption was not confirmed"
        );
        ensure!(
            self.replay_rc == 1 && self.after_replay == self.first,
            "same-wallet replay was accepted or failed inconclusively"
        );
        ensure!(
            self.fresh_rc == 1 && self.fresh_spent && self.fresh_balance == 0,
            "fresh-wallet replay did not prove a spent-proof rejection"
        );
        let winners = self.race_rc.iter().filter(|&&code| code == 0).count();
        ensure!(
            winners == 1,
            "race requires exactly one redemption; observed {winners}"
        );
        for index in 0..2 {
            if self.race_rc[index] == 0 {
                ensure!(
                    self.race_balance[index] == 16,
                    "race winner was not credited"
                );
            } else {
                ensure!(
                    self.race_rc[index] == 1
                        && self.race_spent[index]
                        && self.race_balance[index] == 0,
                    "race loser was credited or failed for an unverified reason"
                );
            }
        }
        ensure!(
            self.source_after == 16,
            "unexpected source debit; fixture requires zero input fees"
        );
        ensure!(
            self.source_after
                + self.first
                + self.fresh_balance
                + self.race_balance.iter().sum::<u64>()
                == self.before,
            "wallet total changed across transfers/replay/race"
        );
        Ok(())
    }
}

fn document(implementation: &str) -> Value {
    let mut spec = super::cdk_wallet::document(0);
    spec["name"] = json!("cashu-double-spend");
    if implementation == "nutshell" {
        let mint = spec["components"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|component| component["id"] == "mint")
            .unwrap();
        mint["implementation"] = json!("nutshell");
        mint["version"] = json!("0.20.3");
        mint["config_version"] = json!("nutshell-mint/0.20/v1");
    }
    spec
}

fn scoped(id: &str, mut args: Value) -> Value {
    args.as_object_mut().unwrap().extend(
        json!({
            "instance_id":INSTANCE,"experiment_id":"proof-spend","session_id":"proof-spend",
            "operation_id":id,"idempotency_key":id
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    args
}

fn operation(
    client: &mut McpClient,
    directory: &Path,
    tool: &str,
    id: &str,
    args: Value,
) -> Result<Value> {
    client.call(tool, scoped(id, args))?;
    let receipt = cell::wait_operation(client, id, 100)?;
    fs::write(
        directory.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(cell::artifact_content(&receipt)?.clone())
}

fn native_ok(receipt: &Value) -> Result<()> {
    ensure!(
        receipt["exit_code"] == 0
            && receipt["timed_out"] == false
            && receipt["cleanup_verified"] == true
            && receipt["streams_complete"] == true
            && receipt["output_truncated"] == false,
        "native command did not return a complete successful receipt (exit={}, timeout={}, cleanup={})",
        receipt["exit_code"],
        receipt["timed_out"],
        receipt["cleanup_verified"]
    );
    Ok(())
}

fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    directory: &Path,
    namespace: &str,
) -> Result<()> {
    operation(
        client,
        directory,
        "liquidity_bootstrap",
        "bootstrap",
        json!({
            "chain":"chain","mint_lightning":"mint-lnd","payer_lightning":"payer-lnd",
            "funding_sat":50_000_000,"channel_sat":10_000_000,"push_sat":5_000_000
        }),
    )?;
    // One real invoice, kept in the owned wallet volume. Pay it while the native
    // mint command waits; never create a replacement quote on a retry.
    client.call("component_exec_live", scoped("fund", json!({"component":"wallet-a",
        "script":"umask 077; exec cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive mint http://mint:3338 64 --wait-duration 120 >/wallet/proof-spend-fund.log 2>&1",
        "timeout_seconds":150})))?;
    let mut invoice = String::new();
    for _ in 0..60 {
        invoice = context.kubectl.exec(namespace, "deployment/wallet-a", &["sh","-c",
            "if [ -f /wallet/proof-spend-fund.log ]; then grep -oE 'lnbcrt[0-9a-z]+' /wallet/proof-spend-fund.log || true; fi"])?;
        if !invoice.is_empty() {
            break;
        }
        sleep(Duration::from_secs(1));
    }
    ensure!(
        invoice.starts_with("lnbcrt")
            && invoice
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "expected exactly one regtest funding invoice"
    );
    let paid = operation(
        client,
        directory,
        "component_exec_live",
        "pay",
        json!({"component":"payer-lnd",
        "argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","sendpayment","--force","--json","--timeout=30s",format!("--pay_req={invoice}")],
        "timeout_seconds":45,"output":{"mode":"json_fields","fields":["status"]}}),
    )?;
    native_ok(&paid)?;
    ensure!(
        paid["projection_succeeded"] == true && paid["selected_output"]["status"] == "SUCCEEDED",
        "funding payment did not settle"
    );
    let funded = cell::wait_operation(client, "fund", 60)?;
    native_ok(cell::artifact_content(&funded)?)?;
    let balance = operation(
        client,
        directory,
        "wallet_balance",
        "funded-balance",
        json!({"wallet":"wallet-a","mint":"mint"}),
    )?;
    ensure!(
        balance["balance_sat"] == 64
            && balance["pending_sat"] == 0
            && balance["reserved_sat"] == 0
            && balance["pending_spent_sat"] == 0,
        "64-sat funding was not independently confirmed"
    );
    let result = operation(
        client,
        directory,
        "component_exec_live",
        "proof-spend",
        json!({
            "component":"wallet-a","script":driver(),"timeout_seconds":240,"output":{"mode":"public"}
        }),
    )?;
    native_ok(&result)?;
    let receipt: Receipt = serde_json::from_str(expect::string(&result, "/stdout")?)?;
    receipt.verify()
}

pub fn run(context: &GateContext) -> Result<()> {
    for implementation in ["cdk", "nutshell"] {
        println!("{implementation}: preparing isolated cell...");
        let directory = context
            .installation
            .home
            .join("acceptance/cashu-double-spend")
            .join(implementation);
        fs::create_dir_all(&directory)?;
        let mut capabilities = EXPERIMENT_CAPABILITIES.to_vec();
        capabilities.push("component.exec_live");
        let mut client = context.session(
            &format!("proof-spend-{implementation}"),
            "proof-spend-agent",
            &capabilities,
        )?;
        client.call(
            "cell_create",
            json!({"draft_id":INSTANCE,"cell":document(implementation),"idempotency_key":"create"}),
        )?;
        let published = client.call(
            "cell_publish",
            json!({"draft_id":INSTANCE,"expected_version":1,"idempotency_key":"publish"}),
        )?;
        println!("{implementation}: preparing images and materializing...");
        client.call("cell_materialize", json!({"instance_id":INSTANCE,"revision_digest":published["digest"],"idempotency_key":"materialize"}))?;
        let result = (|| -> Result<()> {
            let ready = cell::wait_ready(&mut client, INSTANCE)?;
            println!("{implementation}: funding wallets and checking replay/race...");
            client.call("experiment_create", json!({"experiment_id":INSTANCE,"instance_id":INSTANCE,"idempotency_key":"experiment"}))?;
            client.call(
                "session_start",
                json!({"experiment_id":INSTANCE,"session_id":INSTANCE,"idempotency_key":"session"}),
            )?;
            exercise(
                context,
                &mut client,
                &directory,
                expect::string(&ready, "/instance_namespace")?,
            )
        })();
        println!("{implementation}: removing test cell...");
        let cleanup = (|| -> Result<()> {
            client.call("cell_close", json!({"instance_id":INSTANCE}))?;
            cell::wait_closed(&mut client, INSTANCE)?;
            context.kubectl.assert_no_instance_namespaces()?;
            Ok(())
        })();
        fs::write(
            directory.join("outcome.json"),
            serde_json::to_vec_pretty(&json!({
                "implementation":implementation,"passed":result.is_ok(),"cleanup_passed":cleanup.is_ok(),
                "error":result.as_ref().err().map(ToString::to_string),"cleanup_error":cleanup.as_ref().err().map(ToString::to_string)
            }))?,
        )?;
        if let Err(error) = cleanup {
            bail!("{implementation} cleanup failed: {error}; assertion result: {result:?}");
        }
        result.with_context(|| {
            format!(
                "{implementation} proof-spend gate; evidence: {}",
                directory.display()
            )
        })?;
        println!("{implementation}: identical-proof replay/race and exact balance checks passed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> Value {
        json!({"before":64,"first":32,"after_replay":32,"replay_rc":1,"fresh_rc":1,"fresh_spent":true,
            "fresh_balance":0,"race_rc":[0,1],"race_spent":[false,true],"race_balance":[16,0],"source_after":16})
    }

    #[test]
    fn proof_spend_oracle_rejects_inflation_timeouts_unknown_failures_and_missing_evidence() {
        serde_json::from_value::<Receipt>(receipt())
            .unwrap()
            .verify()
            .unwrap();
        let mut other_winner = receipt();
        other_winner["race_rc"] = json!([1, 0]);
        other_winner["race_spent"] = json!([true, false]);
        other_winner["race_balance"] = json!([0, 16]);
        serde_json::from_value::<Receipt>(other_winner)
            .unwrap()
            .verify()
            .unwrap();
        for (key, value) in [
            ("race_rc", json!([0, 0])),
            ("race_rc", json!([1, 1])),
            ("race_rc", json!([0, 124])),
            ("race_spent", json!([false, false])),
            ("race_balance", json!([16, 16])),
            ("replay_rc", json!(0)),
            ("replay_rc", json!(124)),
            ("fresh_rc", json!(124)),
            ("fresh_spent", json!(false)),
            ("fresh_balance", json!(32)),
            ("source_after", json!(17)),
            ("first", json!(31)),
        ] {
            let mut bad = receipt();
            bad[key] = value;
            assert!(
                serde_json::from_value::<Receipt>(bad)
                    .unwrap()
                    .verify()
                    .is_err(),
                "{key}"
            );
        }
        let mut missing = receipt();
        missing.as_object_mut().unwrap().remove("fresh_spent");
        assert!(serde_json::from_value::<Receipt>(missing).is_err());
    }

    #[test]
    fn driver_uses_the_shared_passive_observer_without_unresolved_placeholders() {
        let script = driver();
        assert_eq!(DRIVER.matches("__PROOFSTORM_BALANCE_OBSERVER__").count(), 1);
        assert!(script.contains(BALANCE_OBSERVER));
        assert!(!script.contains("__PROOFSTORM_BALANCE_OBSERVER__"));
        assert!(script.contains("result[\"balance_sat\"]"));
    }

    #[test]
    fn fixtures_resolve_both_real_mints_with_explicit_zero_fees() {
        for implementation in ["cdk", "nutshell"] {
            let spec: proofstorm_core::CellSpec =
                serde_json::from_value(document(implementation)).unwrap();
            let lock =
                proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).unwrap();
            assert!(!lock.entries.is_empty());
            let mint = spec.components.iter().find(|c| c.id == "mint").unwrap();
            assert_eq!(mint.config["input_fee_ppk"], json!(0));
        }
    }
}
