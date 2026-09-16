//! Map live getpeerinfo addresses only to current same-cell Bitcoin workloads.
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;
use proofstorm_kube::{COMPONENT_LABEL, ProofstormCell, ROLLOUT_DIGEST_ANNOTATION};
use proofstorm_view::BitcoinObservation;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
};

pub(super) fn project(
    cell: &ProofstormCell,
    inventory: &[Pod],
    response: &Value,
) -> Option<BitcoinObservation> {
    let mut addresses = BTreeMap::<IpAddr, BTreeSet<String>>::new();
    for pod in inventory
        .iter()
        .filter(|p| p.metadata.deletion_timestamp.is_none())
    {
        let Some(id) = pod.labels().get(COMPONENT_LABEL) else {
            continue;
        };
        if !cell
            .spec
            .cell
            .components
            .iter()
            .any(|c| &c.id == id && c.implementation == "bitcoin-core")
            || !cell.spec.lock.entries.iter().any(|e| {
                &e.component_id == id
                    && pod.annotations().get(ROLLOUT_DIGEST_ANNOTATION) == Some(&e.rollout_digest)
            })
        {
            continue;
        }
        if let Some(status) = &pod.status {
            for address in status
                .pod_ips
                .iter()
                .flatten()
                .map(|a| a.ip.as_str())
                .chain(status.pod_ip.as_deref())
            {
                if let Ok(ip) = address.parse() {
                    addresses.entry(ip).or_default().insert(id.clone());
                }
            }
        }
    }
    Some(BitcoinObservation {
        observed_at_unix: super::now(),
        error: None,
        peers: peer_ids(response, &addresses)?,
    })
}
fn peer_ids(
    response: &Value,
    addresses: &BTreeMap<IpAddr, BTreeSet<String>>,
) -> Option<Vec<String>> {
    let mut peers = BTreeSet::new();
    for peer in response.as_array()? {
        let address = peer["addr"].as_str()?;
        // External peers (including onion addresses) are intentionally unmapped.
        if let Ok(address) = address.parse::<SocketAddr>() {
            if let Some(ids) = addresses.get(&address.ip()).filter(|ids| ids.len() == 1) {
                peers.extend(ids.iter().cloned());
            }
        }
    }
    Some(peers.into_iter().collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_ipv4_ipv6_and_inbound_ephemeral_ports_without_exposing_addresses() {
        let addresses = BTreeMap::from([
            (
                "10.0.0.2".parse().unwrap(),
                BTreeSet::from(["bitcoin-a".into()]),
            ),
            (
                "fd00::2".parse().unwrap(),
                BTreeSet::from(["bitcoin-b".into()]),
            ),
        ]);
        let peers = serde_json::json!([{"addr":"10.0.0.2:48123"},{"addr":"[fd00::2]:18444"},{"addr":"outside.onion:18444"}]);
        assert_eq!(
            peer_ids(&peers, &addresses).unwrap(),
            vec!["bitcoin-a", "bitcoin-b"]
        );
        assert!(peer_ids(&serde_json::json!([{}]), &addresses).is_none());
        assert!(
            peer_ids(&serde_json::json!([]), &addresses)
                .unwrap()
                .is_empty()
        );
    }
}
