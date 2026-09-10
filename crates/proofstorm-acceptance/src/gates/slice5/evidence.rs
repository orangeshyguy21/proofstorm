//! Assert the scenario's named operations, not one monolithic action count.
use super::{
    Scenario,
    common::{EXPERIMENT, action_kinds, kinds_by_operation},
    compose::Materialized,
};
use crate::{GateContext, McpClient, json as expect};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn expected(scenario: Scenario) -> BTreeMap<&'static str, &'static str> {
    let operations: &[(&str, &str)] = match scenario {
        Scenario::Smoke => &[
            ("bootstrap", "bootstrap_liquidity"),
            ("wallet-initialize", "wallet_initialize"),
            ("wallet-balance", "wallet_balance"),
            ("wallet-fund", "wallet_fund"),
            ("wallet-balance-before-pay", "wallet_balance"),
            ("round-trip", "wallet_round_trip"),
            ("conservation", "conservation_oracle"),
            ("receiver-initialize", "wallet_initialize"),
            ("wallet-invoice", "wallet_invoice"),
            ("wallet-pay", "wallet_pay"),
        ],
        Scenario::Recovery => &[
            ("bootstrap", "bootstrap_liquidity"),
            ("lost-probe", "reachability_oracle"),
            ("cancelled-probe", "reachability_oracle"),
            ("payer-stop", "node_stop"),
            ("payer-start", "node_start"),
            ("payer-restart", "node_restart"),
        ],
        Scenario::Network => &[
            ("reachability-baseline", "reachability_oracle"),
            ("wallet-mint-partition", "network_partition"),
            ("reachability-wallet-blocked", "reachability_oracle"),
            ("reachability-receiver-open", "reachability_oracle"),
            ("receiver-wallet-mint-partition", "network_partition"),
            ("reachability-receiver-blocked", "reachability_oracle"),
            ("reachability-wallet-reconstructed", "reachability_oracle"),
            ("reachability-receiver-reconstructed", "reachability_oracle"),
            ("wallet-mint-heal", "network_heal"),
            ("reachability-wallet-healed", "reachability_oracle"),
            ("reachability-receiver-still-blocked", "reachability_oracle"),
            ("receiver-wallet-mint-heal", "network_heal"),
            ("reachability-receiver-healed", "reachability_oracle"),
        ],
        Scenario::Channels => &[
            ("bootstrap", "bootstrap_liquidity"),
            ("peer-connect", "peer_connect"),
            ("channel-open", "channel_open"),
            ("cln-peer-connect", "peer_connect"),
            ("cln-channel-open", "channel_open"),
            ("rebalance-bridge-peer-connect", "peer_connect"),
            ("rebalance-bridge-channel-open", "channel_open"),
            ("channel-rebalance", "channel_rebalance"),
            ("rebalance-bridge-channel-close", "channel_close"),
            ("channel-close", "channel_close"),
            ("bootstrap-channel-close", "channel_close"),
            ("peer-disconnect", "peer_disconnect"),
            ("peer-reconnect", "peer_connect"),
            ("force-channel-open", "channel_open"),
            ("channel-force-close", "channel_force_close"),
            ("cln-channel-close", "channel_close"),
            ("cln-peer-disconnect", "peer_disconnect"),
            ("cln-peer-reconnect", "peer_connect"),
            ("cln-force-channel-open", "channel_open"),
            ("cln-channel-force-close", "channel_force_close"),
        ],
    };
    operations.iter().copied().collect()
}

fn validate_journal(journal: &[Value], expected: &BTreeMap<&str, &str>) -> Result<Vec<u64>> {
    let mut ids = BTreeSet::new();
    let mut sequences = Vec::new();
    for action in journal {
        let id = expect::string(action, "/id")?;
        ensure!(
            expected.contains_key(id),
            "unexpected operation in journal: {id}"
        );
        ensure!(ids.insert(id), "duplicate operation in journal: {id}");
        let phase = match id {
            "lost-probe" => "failed",
            "cancelled-probe" => "cancelled",
            _ => "succeeded",
        };
        ensure!(
            expect::string(action, "/phase")? == phase,
            "incorrect phase for {id}: {action}"
        );
        sequences.push(expect::integer(action, "/sequence")?);
    }
    ensure!(
        ids == expected.keys().copied().collect(),
        "journal omitted expected operations"
    );
    ensure!(
        sequences
            .iter()
            .copied()
            .eq(1..=u64::try_from(journal.len())?),
        "journal sequence has gaps, duplicates, or is out of order"
    );
    Ok(sequences)
}

pub(super) fn verify(
    context: &GateContext,
    client: &mut McpClient,
    state: &Materialized,
    scenario: Scenario,
) -> Result<()> {
    let wanted = expected(scenario);
    let runtime = kinds_by_operation(&action_kinds(context, &state.instance_key)?)?;
    // Conservation is computed from immutable recorded receipts in the control
    // client. It belongs in the journal/evidence but must create no runtime Job.
    let runtime_wanted = wanted
        .iter()
        .filter(|(_, kind)| **kind != "conservation_oracle")
        .map(|(id, kind)| (*id, *kind))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        runtime
            .iter()
            .map(|(id, kind)| (id.as_str(), kind.as_str()))
            .collect::<BTreeMap<_, _>>()
            == runtime_wanted,
        "scenario did not create exactly its expected typed runtime actions: {runtime:?}"
    );
    let mut journal = Vec::new();
    let mut after = 0;
    loop {
        let page = client.call(
            "action_list",
            json!({"experiment_id":EXPERIMENT,"after_sequence":after,"limit":100}),
        )?;
        let items = expect::array(&page, "/actions")?;
        if items.is_empty() {
            break;
        }
        let next = expect::integer(items.last().unwrap(), "/sequence")?;
        ensure!(next > after, "journal pagination did not advance");
        after = next;
        journal.extend_from_slice(items);
        ensure!(
            journal.len() <= wanted.len(),
            "journal contains excess operations"
        );
    }
    let sequences = validate_journal(&journal, &wanted)?;
    client.call(
        "experiment_close",
        json!({"experiment_id":EXPERIMENT,"idempotency_key":"close-evidence-run"}),
    )?;
    let explicit = if matches!(scenario, Scenario::Smoke) {
        vec!["wallet-pay"]
    } else {
        vec![]
    };
    let request = json!({"experiment_id":EXPERIMENT,"include_oracle_artifacts":true,
        "artifact_operation_ids":explicit,"include_content":true});
    let evidence = client.call("artifact_export", request.clone())?;
    let replay = client.call("artifact_export", request)?;
    ensure!(
        evidence["digest"] == replay["digest"]
            && evidence["byte_length"] == replay["byte_length"]
            && evidence["content"] == replay["content"],
        "evidence export is not deterministic"
    );
    let bytes = expect::integer(&evidence, "/byte_length")?;
    ensure!(
        expect::string(&evidence, "/media_type")?
            == "application/vnd.proofstorm.evidence.v1alpha1+json"
            && expect::string(&evidence, "/digest")?.starts_with("sha256:")
            && (1..=512 * 1024).contains(&bytes)
            && expect::string(&evidence, "/content/api_version")? == "proofstorm/evidence/v1alpha1"
            && expect::string(&evidence, "/content/instance/revision_digest")?
                == state.revision_digest
            && expect::string(&evidence, "/content/instance/lock_digest")? == state.lock_digest,
        "evidence identity or size is invalid"
    );
    let exported = expect::array(&evidence, "/content/journal")?;
    let exported_sequences = exported
        .iter()
        .map(|a| expect::integer(a, "/sequence"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        exported_sequences == sequences,
        "evidence journal does not match the live journal"
    );
    let artifacts = expect::array(&evidence, "/content/artifacts")?
        .iter()
        .map(|a| expect::string(a, "/operation_id"))
        .collect::<Result<Vec<_>>>()?;
    let artifact_ids: BTreeSet<_> = artifacts.iter().copied().collect();
    let expected_artifacts: BTreeSet<_> = wanted
        .iter()
        .filter_map(|(id, kind)| {
            (kind.ends_with("_oracle") || explicit.contains(id)).then_some(*id)
        })
        .collect();
    ensure!(
        artifact_ids == expected_artifacts && artifact_ids.len() == artifacts.len(),
        "evidence artifact selection is incomplete or duplicated"
    );
    let encoded = serde_json::to_string(&evidence)?.to_lowercase();
    for forbidden in [
        "resource_name",
        "instance_key",
        "lnbcrt",
        "payment_request",
        "adapter_quote",
        "mnemonic",
    ] {
        ensure!(
            !encoded.contains(forbidden),
            "private or runtime-only material crossed evidence export: {forbidden}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_is_complete_unique_ordered_and_terminal() {
        let wanted = BTreeMap::from([("first", "node_start"), ("second", "node_stop")]);
        let valid = vec![
            json!({"id":"first","sequence":1,"phase":"succeeded"}),
            json!({"id":"second","sequence":2,"phase":"succeeded"}),
        ];
        assert!(validate_journal(&valid, &wanted).is_ok());
        assert!(validate_journal(&valid[..1], &wanted).is_err());
        for (field, value) in [
            ("id", json!("first")),
            ("sequence", json!(1)),
            ("phase", json!("running")),
        ] {
            let mut invalid = valid.clone();
            invalid[1][field] = value;
            assert!(validate_journal(&invalid, &wanted).is_err());
        }
    }
}
