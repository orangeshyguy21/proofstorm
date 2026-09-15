//! Only node-local, open-channel observations; never infer channels from peer links.
use super::balances::{integer, millisats};
use proofstorm_view::{LightningObservation, ObservedChannel};
use serde_json::Value;

pub(super) fn project(
    implementation: &str,
    info: &Value,
    response: &Value,
) -> Option<LightningObservation> {
    let key = if implementation == "lnd" {
        "identity_pubkey"
    } else {
        "id"
    };
    let node_pubkey = pubkey(info[key].as_str()?)?;
    let channels = response["channels"]
        .as_array()?
        .iter()
        .filter(|c| implementation == "lnd" || c["state"] == "CHANNELD_NORMAL")
        .map(|c| {
            if implementation == "lnd" {
                lnd(c)
            } else {
                cln(c)
            }
        })
        .collect::<Option<Vec<_>>>()?;
    Some(LightningObservation {
        node_pubkey: Some(node_pubkey),
        channels,
        observed_at_unix: super::now(),
        error: None,
    })
}
pub(super) fn pubkey(key: &str) -> Option<String> {
    (key.len() == 66
        && (key.starts_with("02") || key.starts_with("03"))
        && key.bytes().all(|b| b.is_ascii_hexdigit()))
    .then(|| key.to_ascii_lowercase())
}
fn outpoint(value: &str) -> Option<String> {
    let (txid, index) = value.split_once(':')?;
    let index: u32 = index.parse().ok()?;
    (txid.len() == 64 && txid.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{}:{index}", txid.to_ascii_lowercase()))
}
fn lnd(c: &Value) -> Option<ObservedChannel> {
    checked(ObservedChannel {
        channel_id: None,
        capacity_only: false,
        funding_outpoint: outpoint(c["channel_point"].as_str()?)?,
        peer_pubkey: pubkey(c["remote_pubkey"].as_str()?)?,
        active: c["active"].as_bool()?,
        capacity_msat: integer(&c["capacity"])?.checked_mul(1000)?,
        local_msat: integer(&c["local_balance"])?.checked_mul(1000)?,
        remote_msat: integer(&c["remote_balance"])?.checked_mul(1000)?,
    })
}
fn cln(c: &Value) -> Option<ObservedChannel> {
    let capacity = millisats(&c["total_msat"])?;
    let local = millisats(&c["to_us_msat"])?;
    checked(ObservedChannel {
        channel_id: None,
        capacity_only: false,
        funding_outpoint: outpoint(&format!(
            "{}:{}",
            c["funding_txid"].as_str()?,
            integer(&c["funding_outnum"])?
        ))?,
        peer_pubkey: pubkey(c["peer_id"].as_str()?)?,
        active: c["peer_connected"].as_bool()?,
        capacity_msat: capacity,
        local_msat: local,
        remote_msat: capacity.checked_sub(local)?,
    })
}
pub(super) fn checked(mut channel: ObservedChannel) -> Option<ObservedChannel> {
    if channel.channel_id.is_none() {
        channel.channel_id = channel_id(&channel.funding_outpoint);
    }
    (channel.capacity_msat > 0
        && channel.local_msat.checked_add(channel.remote_msat)? <= channel.capacity_msat)
        .then_some(channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn channels_preserve_outpoints_inactive_state_and_msat_precision() {
        let peer = format!("02{}", "a".repeat(64));
        let point = format!("{}:1", "b".repeat(64));
        let lnd = json!({"channels":[{"channel_point":point,"remote_pubkey":peer,"active":false,"capacity":"10","local_balance":"6","remote_balance":"3"}]});
        let result = project("lnd", &json!({"identity_pubkey":peer}), &lnd).unwrap();
        assert_eq!(result.channels[0].local_msat, 6000);
        assert!(!result.channels[0].active);
        let cln = json!({"channels":[{"state":"CHANNELD_NORMAL","funding_txid":"b".repeat(64),"funding_outnum":1,"peer_id":peer,"peer_connected":true,"total_msat":10000,"to_us_msat":"6001msat"},{"state":"CLOSINGD_COMPLETE"}]});
        let result = project("cln", &json!({"id":peer}), &cln).unwrap();
        assert_eq!(result.channels.len(), 1);
        assert_eq!(result.channels[0].remote_msat, 3999);
        assert_eq!(result.channels[0].funding_outpoint, point);
    }
    #[test]
    fn funding_outpoint_matches_ldk_wire_order_with_nonzero_output() {
        let txid = (0_u8..32).map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            channel_id(&format!("{txid}:258")).unwrap(),
            "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020002"
        );
        assert!(channel_id(&format!("{txid}:65536")).is_none());
    }
    #[test]
    fn malformed_channels_are_unavailable_instead_of_empty() {
        let peer = format!("02{}", "a".repeat(64));
        assert!(
            project(
                "lnd",
                &json!({"identity_pubkey":peer}),
                &json!({"channels":[{}]})
            )
            .is_none()
        );
        assert!(
            project(
                "lnd",
                &json!({"identity_pubkey":peer}),
                &json!({"channels":[]})
            )
            .unwrap()
            .channels
            .is_empty()
        );
    }
}

// BOLT #2: XOR the funding output index into the last two txid bytes.
fn channel_id(point: &str) -> Option<String> {
    use std::fmt::Write;
    let (txid, index) = point.split_once(':')?;
    let index: u16 = index.parse().ok()?;
    if txid.len() != 64 || !txid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = (0..32)
        .map(|i| u8::from_str_radix(&txid[i * 2..i * 2 + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    bytes.reverse(); // Bitcoin RPC prints txids in reverse of their wire byte order.
    let [high, low] = index.to_be_bytes();
    bytes[30] ^= high;
    bytes[31] ^= low;
    let mut id = String::new();
    for byte in bytes {
        write!(&mut id, "{byte:02x}").ok()?;
    }
    Some(id)
}
