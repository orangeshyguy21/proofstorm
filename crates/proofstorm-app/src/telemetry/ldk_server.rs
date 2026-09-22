//! Passive observations from the standalone LDK Server CLI.
use super::{
    balances::{amount, integer, read},
    channels::{checked, outpoint, pubkey},
};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use proofstorm_view::{BalanceAmount, ComponentBalance, LightningObservation, ObservedChannel};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) async fn observe(pods: &Api<Pod>, pod: &str, result: &mut ComponentBalance) {
    let command = |action: &str| {
        [
            "ldk-server-cli",
            "--config",
            "/config/config.toml",
            "--base-url",
            "127.0.0.1:3536",
            action,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    };
    let (info, channels, funds) = tokio::join!(
        read(pods, pod, command("get-node-info")),
        read(pods, pod, command("list-channels")),
        read(pods, pod, command("get-balances")),
    );
    if let Some(observation) = info.zip(channels).and_then(|(i, c)| project(&i, &c)) {
        result.lightning = Some(observation);
    }
    if let Some(amounts) = funds.and_then(|f| balances(&f)) {
        result.amounts = amounts;
        result.error = None;
    }
}

fn balances(funds: &Value) -> Option<Vec<BalanceAmount>> {
    Some(vec![
        amount(
            "Lightning",
            integer(&funds["total_lightning_balance_sats"])?,
        ),
        amount("On-chain", integer(&funds["total_onchain_balance_sats"])?),
    ])
}

fn project(info: &Value, response: &Value) -> Option<LightningObservation> {
    let node_pubkey = pubkey(info["node_id"].as_str()?)?;
    let mut channels = Vec::new();
    let mut ids = BTreeSet::new();
    for c in response["channels"].as_array()? {
        // Pending funding is not an open channel. Disconnected ready channels remain visible.
        if !c["is_channel_ready"].as_bool()? {
            continue;
        }
        let id = c["channel_id"].as_str()?;
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let id = id.to_ascii_lowercase();
        if !ids.insert(id.clone()) {
            return None;
        }
        channels.push(checked(ObservedChannel {
            channel_id: Some(id),
            capacity_only: true,
            funding_outpoint: outpoint(&format!(
                "{}:{}",
                c["funding_txo"]["txid"].as_str()?,
                integer(&c["funding_txo"]["vout"])?
            ))?,
            peer_pubkey: pubkey(c["counterparty_node_id"].as_str()?)?,
            active: c["is_usable"].as_bool()?,
            capacity_msat: integer(&c["channel_value_sats"])?.checked_mul(1000)?,
            local_msat: integer(&c["outbound_capacity_msat"])?,
            remote_msat: integer(&c["inbound_capacity_msat"])?,
        })?);
    }
    Some(LightningObservation {
        node_pubkey: Some(node_pubkey),
        channels,
        observed_at_unix: super::now(),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info() -> Value {
        json!({"node_id":format!("02{}", "a".repeat(64))})
    }
    fn channel() -> Value {
        json!({
            "channel_id":"b".repeat(64),
            "funding_txo":{"txid":"c".repeat(64),"vout":1},
            "counterparty_node_id":format!("03{}", "d".repeat(64)),
            "is_channel_ready":true,"is_usable":false,"channel_value_sats":10,
            "outbound_capacity_msat":6001,"inbound_capacity_msat":2999
        })
    }
    #[test]
    fn ready_channels_preserve_identity_precision_and_disconnected_state() {
        let result = project(
            &info(),
            &json!({"channels":[channel(),{"is_channel_ready":false}]}),
        )
        .unwrap();
        assert_eq!(result.channels.len(), 1);
        let c = &result.channels[0];
        assert_eq!(c.id(), "b".repeat(64));
        assert_eq!(c.funding_outpoint, format!("{}:1", "c".repeat(64)));
        assert_eq!(
            (c.capacity_msat, c.local_msat, c.remote_msat),
            (10_000, 6001, 2999)
        );
        assert!(c.capacity_only);
        assert!(!c.active);
    }
    #[test]
    fn malformed_and_duplicate_channels_are_unavailable_but_empty_is_valid() {
        assert!(
            project(&info(), &json!({"channels":[]}))
                .unwrap()
                .channels
                .is_empty()
        );
        assert!(project(&info(), &json!({"channels":[{}]})).is_none());
        assert!(project(&info(), &json!({"channels":[channel(),channel()]})).is_none());
        for (field, value) in [
            ("outbound_capacity_msat", json!(10001)),
            ("channel_value_sats", json!(u64::MAX)),
            ("channel_id", json!("bad")),
            ("counterparty_node_id", json!("bad")),
            ("is_usable", Value::Null),
        ] {
            let mut c = channel();
            c[field] = value;
            assert!(
                project(&info(), &json!({"channels":[c]})).is_none(),
                "{field}"
            );
        }
    }
    #[test]
    fn node_balance_uses_reported_sats_not_channel_capacity() {
        let amounts = balances(
            &json!({"total_lightning_balance_sats":"12345","total_onchain_balance_sats":67890}),
        )
        .unwrap();
        assert_eq!(
            amounts,
            vec![amount("Lightning", 12345), amount("On-chain", 67890)]
        );
        assert!(balances(&json!({})).is_none());
    }
}
