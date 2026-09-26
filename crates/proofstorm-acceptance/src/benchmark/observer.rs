//! Reads runtime/payment state using runner-owned credentials before teardown.
use super::{Context, read, save};
use crate::{Kubectl, McpClient, cell, http};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) fn content(event: &Value) -> Value {
    let result = &event["response"]["result"];
    result
        .get("structuredContent")
        .cloned()
        .or_else(|| {
            result["content"][0]["text"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
        })
        .unwrap_or(Value::Null)
}

fn operation_ids(events: &[Value]) -> Result<BTreeSet<String>> {
    events
        .iter()
        .filter(|event| {
            event["kind"] == "end" && event["tool"] == "cell_exec" && event["success"] == true
        })
        .map(|event| {
            content(event)["operation_id"]
                .as_str()
                .map(str::to_owned)
                .context("submitted operation identity missing")
        })
        .collect()
}

/// Cell teardown removes its operation records. Retain their terminal state first.
pub(super) fn retain_terminal(config: &Context, client: &mut McpClient) -> Result<()> {
    let path = config.work.join("terminal-observation.json");
    if path.exists() {
        return Ok(());
    }
    let ids = operation_ids(&super::events(&config.work)?)?;
    let mut observed = Vec::new();
    for id in &ids {
        observed.push(
            client
                .call("operation_status", json!({"operation_id":id}))
                .unwrap_or_else(
                    |error| json!({"operation_id":id,"observer_error":format!("{error:#}")}),
                ),
        );
    }
    let verified = !ids.is_empty()
        && observed.iter().all(|value| {
            value["terminal"] == true && value["native_result"]["cleanup_verified"] == true
        });
    save(
        &path,
        &json!({"operation_ids":ids,"operations":observed,"verified":verified}),
    )
}

pub(super) fn terminal_assertion(config: &Context, events: &[Value]) -> bool {
    let record = read(&config.work.join("terminal-observation.json")).unwrap_or(Value::Null);
    record["verified"] == true
        && operation_ids(events)
            .is_ok_and(|ids| !ids.is_empty() && json!(ids) == record["operation_ids"])
}

fn identifier(value: &Value, key: &str) -> Result<String> {
    let id = value[key].as_str().context("missing quote identity")?;
    ensure!(
        !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-'),
        "invalid quote identity"
    );
    Ok(id.into())
}
fn holdings(task: &super::task::Task, kube: &Kubectl, ns: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(&kube.exec(
        ns,
        &format!("deployment/{}", task.component("nutshell-wallet", 0)),
        &[
            "/opt/proofstorm/driver",
            "holdings",
            "nutshell-wallet",
            task.component("nutshell-wallet", 0),
        ],
    )?)?;
    let rows = value["mints"]
        .as_array()
        .context("holdings mints missing")?;
    let selected: Vec<_> = rows
        .iter()
        .filter(|r| r["mint_url"] == task.mint_url())
        .collect();
    ensure!(selected.len() == 1, "expected exactly one mint holding");
    Ok(selected[0].clone())
}
pub fn checkpoint(config: &Context, client: &mut McpClient, args: &Value) -> Result<Value> {
    let task = &config.task;
    let stage = args["stage"].as_str().context("stage required")?;
    ensure!(matches!(stage, "funded" | "paid"), "unknown checkpoint");
    let path = config.work.join(format!("{stage}.json"));
    ensure!(
        !path.exists(),
        "checkpoint already retained; continue with the next task step"
    );
    let runtime = cell::status(client, &task.cell_name)?;
    ensure!(
        runtime["phase"] == "ready",
        "cell must be ready at checkpoint"
    );
    let ns = runtime["instance_namespace"]
        .as_str()
        .context("namespace missing")?;
    let document =
        cell::read_document(client, &json!({"name":task.cell_name}), "configuration", "")?;
    let lock = cell::read_document(client, &json!({"name":task.cell_name}), "lock", "")?;
    let installation = proofstorm_app::installation::Installation::load(&config.home)?;
    let kube = Kubectl::for_installation(&installation)?;
    let wallet = holdings(task, &kube, ns)?;
    let mut forward = http::PortForward::open(
        &kube,
        ns,
        &format!("service/{}", task.component("cdk", 0)),
        3338,
    )?;
    let quote = identifier(args, "mint_quote_id")?;
    let mint = http::get_json_retrying(&mut forward, &format!("/v1/mint/quote/bolt11/{quote}"), 5)?;
    let receive: Value = serde_json::from_str(&kube.exec(
        ns,
        &format!("deployment/{}", task.component("nutshell-wallet", 0)),
        &[
            "env",
            "HOME=/wallet",
            &format!("PROOFSTORM_WALLET={}", task.component("nutshell-wallet", 0)),
            &format!("PROOFSTORM_MINT={}", task.component("cdk", 0)),
            &format!("PROOFSTORM_EXPECTED_MINT_URL={}", task.mint_url()),
            "PROOFSTORM_OBSERVATION_ROLE=payment_receive",
            &format!("PROOFSTORM_MINT_QUOTE_ID={quote}"),
            "/opt/proofstorm/driver",
            "quote",
            "observe-receive",
        ],
    )?)?;
    let mut evidence = json!({"stage":stage,"claims":args,"runtime":runtime,"document":document,"lock":lock,
        "wallet":wallet,"mint_quote":mint,"wallet_receive":receive});
    if stage == "paid" {
        let before = read(&config.work.join("funded.json"))?;
        ensure!(
            before["claims"]["mint_quote_id"] == args["mint_quote_id"],
            "funding identity changed"
        );
        ensure!(
            before["runtime"]["instance_key"] == runtime["instance_key"],
            "cell incarnation changed"
        );
        let melt_id = identifier(args, "melt_quote_id")?;
        let melt =
            http::get_json_retrying(&mut forward, &format!("/v1/melt/quote/bolt11/{melt_id}"), 5)?;
        let hash = args["payment_hash"]
            .as_str()
            .context("payment_hash required")?;
        ensure!(
            hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid payment hash"
        );
        let receiver: Value = serde_json::from_str(&kube.exec(
            ns,
            &format!("statefulset/{}", task.component("lnd", 1)),
            &[
                "lncli",
                "--lnddir=/home/lnd/.lnd",
                "--network=regtest",
                "--rpcserver=127.0.0.1:10009",
                "lookupinvoice",
                "--rhash",
                hash,
            ],
        )?)?;
        let invoice = receiver["payment_request"]
            .as_str()
            .context("receiver invoice missing")?;
        let wallet_melt: Value = serde_json::from_str(&kube.exec(
            ns,
            &format!("deployment/{}", task.component("nutshell-wallet", 0)),
            &[
                "env",
                "HOME=/wallet",
                &format!("PROOFSTORM_WALLET={}", task.component("nutshell-wallet", 0)),
                &format!("PROOFSTORM_MINT={}", task.component("cdk", 0)),
                &format!("PROOFSTORM_EXPECTED_MINT_URL={}", task.mint_url()),
                &format!("PROOFSTORM_INVOICE={invoice}"),
                "/opt/proofstorm/driver",
                "quote",
                "observe-melt",
            ],
        )?)?;
        evidence["mint_melt"] = melt;
        evidence["recipient"] = receiver;
        evidence["wallet_melt"] = wallet_melt;
        for (field, command) in [
            ("payer_payments", "listpayments"),
            ("recipient_invoices", "listinvoices"),
        ] {
            let value: Value = serde_json::from_str(&kube.exec(
                ns,
                &format!("statefulset/{}", task.component("lnd", 1)),
                &[
                    "lncli",
                    "--lnddir=/home/lnd/.lnd",
                    "--network=regtest",
                    "--rpcserver=127.0.0.1:10009",
                    command,
                ],
            )?)?;
            evidence[field] = value;
        }
    }
    save(&path, &evidence)?;
    Ok(
        json!({"retained":true,"stage":stage,"next":if stage=="funded" {format!("melt {} sat to {}",task.amounts.melt_sat,task.component("lnd",1))} else {"remove the cell, wait for verified closure, then give the final JSON report".to_owned()}}),
    )
}

pub fn assertions(task: &super::task::Task, funded: &Value, paid: &Value) -> Value {
    let components = paid["document"]["components"].as_array();
    let components_ok = components.is_some_and(|actual| {
        actual.len() == task.components.len()
            && task.components.iter().all(|expected| {
                actual.iter().any(|component| {
                    ["id", "implementation", "version", "config_version"]
                        .iter()
                        .all(|key| component[*key] == expected[*key])
                })
            })
    });
    let bindings = paid["document"]["links"].as_array().is_some_and(|actual| {
        actual.len() == task.links.len()
            && task.links.iter().all(|expected| {
                actual.iter().any(|link| {
                    ["from", "to", "kind", "binding"]
                        .iter()
                        .all(|key| link[*key] == expected[*key])
                })
            })
    });
    let mint = funded["mint_quote"]["state"] == "ISSUED"
        && funded["mint_quote"]["amount"] == task.amounts.mint_sat
        && funded["wallet_receive"]["state"] == "ISSUED"
        && funded["wallet_receive"]["quote_id"] == funded["claims"]["mint_quote_id"]
        && funded["wallet"]["balance_sat"] == task.amounts.mint_sat
        && funded["wallet"]["reserved_sat"] == 0;
    let receiver = paid["recipient"]["settled"] == true
        && sat_string(&paid["recipient"]["amt_paid_sat"], task.amounts.melt_sat)
        && paid["recipient"]["r_hash"] == paid["claims"]["payment_hash"];
    let melt = paid["mint_melt"]["state"] == "PAID"
        && paid["mint_melt"]["amount"] == task.amounts.melt_sat
        && paid["mint_melt"]["quote"] == paid["claims"]["melt_quote_id"]
        && paid["wallet_melt"]["state"] == "PAID"
        && paid["wallet_melt"]["amount_sat"] == task.amounts.melt_sat
        && paid["wallet_melt"]["quote_id"] == paid["claims"]["melt_quote_id"];
    let after = paid["wallet"]["balance_sat"].as_u64();
    let accounting = single_payment_flow(task, funded, paid)
        && after.is_some_and(|n| task.remaining().contains(&n))
        && paid["wallet"]["reserved_sat"] == 0
        && mint
        && melt
        && receiver;
    json!({"components":components_ok,"bindings":bindings,"mint_settled":mint,
        "recipient_settled":receiver&&melt,"accounting":accounting,
        "evidence":mint&&melt&&receiver&&funded["runtime"]["instance_key"].is_string()&&funded["runtime"]["instance_key"]==paid["runtime"]["instance_key"]&&funded["document"]==paid["document"],
        "report":paid["claims"]["minted_sat"]==task.amounts.mint_sat&&paid["claims"]["paid_sat"]==task.amounts.melt_sat&&after.is_some()&&paid["claims"]["remaining_sat"]==paid["wallet"]["balance_sat"]})
}

fn sat_string(value: &Value, amount: u64) -> bool {
    let expected = amount.to_string();
    value.as_str() == Some(expected.as_str())
}

fn single_payment_flow(task: &super::task::Task, funded: &Value, paid: &Value) -> bool {
    let Some(payments) = paid["payer_payments"]["payments"].as_array() else {
        return false;
    };
    let Some(invoices) = paid["recipient_invoices"]["invoices"].as_array() else {
        return false;
    };
    // A fresh O1 node has one successful funding payment and one settled recipient
    // invoice. Require an unpaginated one-flow result; offsets expose hidden rows.
    if payments.len() != 1
        || invoices.len() >= 100
        || paid["payer_payments"]["last_index_offset"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > 100)
        || paid["recipient_invoices"]["last_index_offset"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > 100)
    {
        return false;
    }
    let settled: Vec<_> = invoices.iter().filter(|v| v["settled"] == true).collect();
    payments[0]["status"] == "SUCCEEDED"
        && sat_string(&payments[0]["value_sat"], task.amounts.mint_sat)
        && funded["mint_quote"]["request"].is_string()
        && payments[0]["payment_request"] == funded["mint_quote"]["request"]
        && settled.len() == 1
        && sat_string(&settled[0]["amt_paid_sat"], task.amounts.melt_sat)
        && settled[0]["r_hash"] == paid["claims"]["payment_hash"]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_evidence_survives_teardown_but_does_not_cover_new_operations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = Context {
            root: directory.path().into(),
            work: directory.path().into(),
            home: directory.path().into(),
            mcp: "unused".into(),
            model: "fixture".into(),
            harness: super::super::harness::Harness::Reference,
            task: super::super::task::o1().clone(),
        };
        let event = |id: &str| json!({"kind":"end","tool":"cell_exec","success":true,"response":{"result":{"structuredContent":{"operation_id":id}}}});
        save(
            &config.work.join("terminal-observation.json"),
            &json!({"operation_ids":["one"],"verified":true}),
        )?;
        assert!(terminal_assertion(&config, &[event("one")]));
        assert!(!terminal_assertion(&config, &[event("one"), event("two")]));
        save(
            &config.work.join("terminal-observation.json"),
            &json!({"operation_ids":["one"],"verified":false}),
        )?;
        assert!(!terminal_assertion(&config, &[event("one")]));
        Ok(())
    }
    fn fixture() -> (Value, Value) {
        let document = super::super::task::o1().document();
        let funded = json!({"document":document,"runtime":{"instance_key":"original"},"claims":{"mint_quote_id":"aa"},"mint_quote":{"amount":1000,"state":"ISSUED","request":"invoice"},"wallet_receive":{"state":"ISSUED","quote_id":"aa"},"wallet":{"balance_sat":1000,"reserved_sat":0}});
        let paid = json!({"document":document,"runtime":{"instance_key":"original"},"claims":{"mint_quote_id":"aa","melt_quote_id":"bb","payment_hash":"cc","minted_sat":1000,"paid_sat":100,"remaining_sat":900},"mint_melt":{"amount":100,"state":"PAID","quote":"bb"},"wallet_melt":{"state":"PAID","amount_sat":100,"quote_id":"bb"},"recipient":{"settled":true,"amt_paid_sat":"100","r_hash":"cc"},"wallet":{"balance_sat":900,"reserved_sat":0},"payer_payments":{"payments":[{"status":"SUCCEEDED","value_sat":"1000","payment_request":"invoice"}]},"recipient_invoices":{"invoices":[{"settled":true,"amt_paid_sat":"100","r_hash":"cc"}]}});
        (funded, paid)
    }
    #[test]
    fn good_payment_and_independent_bad_controls() {
        let (funded, paid) = fixture();
        assert!(
            assertions(super::super::task::o1(), &funded, &paid)
                .as_object()
                .unwrap()
                .values()
                .all(|v| v == true)
        );
        for (pointer, value, assertion) in [
            ("/recipient/settled", json!(false), "recipient_settled"),
            ("/recipient/r_hash", json!("different"), "recipient_settled"),
            ("/mint_melt/state", json!("UNPAID"), "recipient_settled"),
            (
                "/wallet_melt/quote_id",
                json!("different"),
                "recipient_settled",
            ),
            ("/wallet/balance_sat", json!(1000), "accounting"),
            ("/wallet/reserved_sat", json!(1), "accounting"),
            ("/claims/remaining_sat", json!(899), "report"),
            ("/runtime/instance_key", json!("replaced"), "evidence"),
            (
                "/payer_payments/payments",
                json!([{"status":"SUCCEEDED","value_sat":"1000","payment_request":"invoice"},{"status":"SUCCEEDED","value_sat":"100"}]),
                "accounting",
            ),
            (
                "/recipient_invoices/invoices",
                json!([{"settled":true,"amt_paid_sat":"100","r_hash":"cc"},{"settled":true,"amt_paid_sat":"100","r_hash":"extra"}]),
                "accounting",
            ),
        ] {
            let mut bad = paid.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                assertions(super::super::task::o1(), &funded, &bad)[assertion],
                false,
                "{pointer}"
            );
        }
        assert!(
            assertions(super::super::task::o1(), &Value::Null, &Value::Null)
                .as_object()
                .unwrap()
                .values()
                .all(|v| v == false)
        );
    }

    #[test]
    fn task_topology_and_derived_fee_window_control_the_oracle() {
        let (funded, mut paid) = fixture();
        let mut task = super::super::task::o1().clone();
        paid["wallet"]["balance_sat"] = json!(890);
        assert_eq!(assertions(&task, &funded, &paid)["accounting"], true);
        task.amounts.maximum_fee_sat = 9;
        assert_eq!(assertions(&task, &funded, &paid)["accounting"], false);
        task.components[0]["config_version"] = json!("different");
        assert_eq!(assertions(&task, &funded, &paid)["components"], false);
        task.links[0]["binding"]["network"] = json!("other");
        assert_eq!(assertions(&task, &funded, &paid)["bindings"], false);
    }
}
