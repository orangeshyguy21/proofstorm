//! Evidence-based canvas relationships. Declared peer links do not imply channels.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use proofstorm_core::{ComponentKind, LinkKind};
use proofstorm_view::{CellUsage, EnvironmentCell};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Declared,
    BitcoinPeer,
    Channel {
        capacity: u64,
        local: u64,
        remote: u64,
        active: bool,
    },
    Holding {
        sat: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub stale: bool,
    pub lane: usize,
}
#[allow(
    clippy::too_many_lines,
    reason = "single projection reconciles declared links and two independently sampled relationship kinds"
)]
pub fn edges(cell: &EnvironmentCell, usage: Option<&CellUsage>, now: i64) -> Vec<Edge> {
    let kind = |id: &str| {
        cell.components
            .items
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.kind)
    };
    let nodes = crate::canvas_model::nodes(cell);
    let lightning_endpoint = |owner: &str| {
        nodes
            .iter()
            .find(|n| n.owner == owner && n.kind == ComponentKind::Lightning)
            .map(|n| n.id.clone())
    };
    let backend_endpoint = |owner: &str| {
        nodes
            .iter()
            .find(|n| {
                n.owner == owner
                    && n.is_embedded()
                    && matches!(n.kind, ComponentKind::Lightning | ComponentKind::Wallet)
            })
            .map_or_else(|| owner.to_owned(), |n| n.id.clone())
    };
    let resource_parents = crate::canvas_model::resource_parents(cell);
    let mut payment_pairs = BTreeSet::new();
    let mut result = cell
        .links
        .items
        .iter()
        .filter(|l| {
            kind(&l.from) != Some(ComponentKind::Workspace)
                && kind(&l.to) != Some(ComponentKind::Workspace)
                && resource_parents.get(&l.to) != Some(&l.from)
                && !matches!(l.kind, LinkKind::LightningPeer | LinkKind::BitcoinPeer)
                && !matches!(
                    (kind(&l.from), kind(&l.to)),
                    (
                        Some(ComponentKind::Lightning),
                        Some(ComponentKind::Lightning)
                    ) | (Some(ComponentKind::Wallet), Some(ComponentKind::Mint))
                        | (Some(ComponentKind::Mint), Some(ComponentKind::Wallet))
                )
        })
        // Method and unit bindings share one connection between these services.
        // Keep observed channels separate: each one represents its own channel.
        .filter(|l| l.kind != LinkKind::PaymentBackend || payment_pairs.insert((&l.from, &l.to)))
        .map(|l| Edge {
            id: if l.kind == LinkKind::PaymentBackend {
                format!("declared:payment:{}:{}", l.from, l.to)
            } else {
                format!("declared:{}", l.id)
            },
            from: if l.kind == LinkKind::ChainBackend {
                backend_endpoint(&l.from)
            } else {
                l.from.clone()
            },
            to: l.to.clone(),
            kind: EdgeKind::Declared,
            stale: false,
            lane: 0,
        })
        .collect::<Vec<_>>();
    let Some(usage) = usage.filter(|u| cell.layout_id.as_deref() == Some(u.incarnation.as_str()))
    else {
        return result;
    };
    // Ambiguous node identities must never connect the wrong cell components.
    let mut identities = BTreeMap::<&str, Vec<&str>>::new();
    for balance in &usage.balances {
        if lightning_endpoint(&balance.component).is_some() {
            if let Some(key) = balance
                .lightning
                .as_ref()
                .and_then(|o| o.node_pubkey.as_deref())
            {
                identities.entry(key).or_default().push(&balance.component);
            }
        }
    }
    let mut channels = BTreeMap::<String, ((bool, bool, i64), Edge)>::new();
    let mut bitcoin = BTreeMap::<(String, String), Edge>::new();
    for balance in &usage.balances {
        if let Some(observation) = &balance.bitcoin {
            if kind(&balance.component) == Some(ComponentKind::Bitcoin) {
                for peer in &observation.peers {
                    if peer == &balance.component || kind(peer) != Some(ComponentKind::Bitcoin) {
                        continue;
                    }
                    if observation.error.is_some()
                        && usage
                            .balances
                            .iter()
                            .find(|b| &b.component == peer)
                            .and_then(|b| b.bitcoin.as_ref())
                            .is_some_and(|other| {
                                other.error.is_none()
                                    && other.observed_at_unix >= observation.observed_at_unix
                                    && !other.peers.contains(&balance.component)
                            })
                    {
                        continue;
                    }
                    let (from, to) = if &balance.component < peer {
                        (balance.component.clone(), peer.clone())
                    } else {
                        (peer.clone(), balance.component.clone())
                    };
                    let stale = crate::model::observation_freshness(
                        observation.observed_at_unix,
                        now,
                        observation.error.is_some() || usage.error.is_some(),
                        true,
                        crate::model::OBSERVATION_MAX_AGE,
                    ) != crate::model::Freshness::Live;
                    let edge = Edge {
                        id: format!("bitcoin-peer:{from}:{to}"),
                        from: from.clone(),
                        to: to.clone(),
                        kind: EdgeKind::BitcoinPeer,
                        stale,
                        lane: 0,
                    };
                    let entry = bitcoin.entry((from, to)).or_insert(edge.clone());
                    if !stale {
                        *entry = edge;
                    }
                }
            }
        }
        if let Some(observation) = &balance.lightning {
            for channel in &observation.channels {
                let Some(peers) = identities
                    .get(channel.peer_pubkey.as_str())
                    .filter(|ids| ids.len() == 1)
                else {
                    continue;
                };
                let peer = peers[0];
                if observation.error.is_some()
                    && usage
                        .balances
                        .iter()
                        .find(|b| b.component == peer)
                        .and_then(|b| b.lightning.as_ref())
                        .is_some_and(|other| {
                            other.error.is_none()
                                && other.observed_at_unix >= observation.observed_at_unix
                                && !other.channels.iter().any(|c| c.id() == channel.id())
                        })
                {
                    continue;
                }

                if peer == balance.component || lightning_endpoint(&balance.component).is_none() {
                    continue;
                }
                let (from, to, local, remote) = if balance.component.as_str() < peer {
                    (
                        lightning_endpoint(&balance.component).unwrap(),
                        lightning_endpoint(peer).unwrap(),
                        channel.local_msat,
                        channel.remote_msat,
                    )
                } else {
                    (
                        lightning_endpoint(peer).unwrap(),
                        lightning_endpoint(&balance.component).unwrap(),
                        channel.remote_msat,
                        channel.local_msat,
                    )
                };
                let edge = Edge {
                    id: format!("channel:{}", channel.id()),
                    from,
                    to,
                    kind: EdgeKind::Channel {
                        capacity: channel.capacity_msat,
                        local,
                        remote,
                        active: channel.active,
                    },
                    stale: crate::model::observation_freshness(
                        observation.observed_at_unix,
                        now,
                        observation.error.is_some() || usage.error.is_some(),
                        true,
                        crate::model::OBSERVATION_MAX_AGE,
                    ) != crate::model::Freshness::Live,
                    lane: 0,
                };
                // Prefer live balances over rounded capacities; then prefer newer evidence.
                let rank = (
                    !edge.stale,
                    !channel.capacity_only,
                    observation.observed_at_unix,
                );
                let entry = channels
                    .entry(edge.id.clone())
                    .or_insert((rank, edge.clone()));
                if rank > entry.0 {
                    *entry = (rank, edge);
                }
            }
        }
        if kind(&balance.component) == Some(ComponentKind::Wallet) {
            if let Some(observation) = &balance.holdings {
                let mut held = BTreeMap::<String, u64>::new();
                for holding in &observation.mints {
                    if let Some(mint) = holding
                        .mint
                        .as_ref()
                        .filter(|id| kind(id) == Some(ComponentKind::Mint))
                    {
                        *held.entry(mint.clone()).or_default() = held
                            .get(mint)
                            .copied()
                            .unwrap_or(0)
                            .saturating_add(holding.held_sat());
                    }
                }
                result.extend(
                    held.into_iter()
                        .filter(|(_, sat)| *sat > 0)
                        .map(|(mint, sat)| Edge {
                            id: format!("holding:{}:{}", balance.component, mint),
                            from: balance.component.clone(),
                            to: mint,
                            kind: EdgeKind::Holding { sat },
                            stale: crate::model::observation_freshness(
                                observation.observed_at_unix,
                                now,
                                observation.error.is_some() || usage.error.is_some(),
                                true,
                                crate::model::OBSERVATION_MAX_AGE,
                            ) != crate::model::Freshness::Live,
                            lane: 0,
                        }),
                );
            }
        }
    }
    result.extend(bitcoin.into_values());
    result.extend(channels.into_values().map(|(_, edge)| edge));
    result.sort_by(|a, b| a.id.cmp(&b.id));
    let mut lanes = BTreeMap::new();
    for edge in &mut result {
        let lane = lanes
            .entry((edge.from.clone(), edge.to.clone()))
            .or_insert(0);
        edge.lane = *lane;
        *lane += 1;
    }
    result
}

#[derive(Clone, Default, PartialEq)]
pub struct Geometry {
    pub path: String,
    pub extent: (f64, f64),
}
#[cfg(test)]
pub fn geometry(from: (f64, f64), to: (f64, f64), lane: usize) -> Geometry {
    sized_geometry(from, (260.0, 144.0), to, (260.0, 144.0), lane)
}
pub fn node_geometry(
    from: &crate::canvas_model::CanvasNode,
    to: &crate::canvas_model::CanvasNode,
    positions: &crate::canvas_model::Positions,
    lane: usize,
) -> Geometry {
    sized_geometry(
        crate::canvas_model::world_position(from, positions),
        (from.width(), from.height()),
        crate::canvas_model::world_position(to, positions),
        (to.width(), to.height()),
        lane,
    )
}
fn sized_geometry(
    from: (f64, f64),
    from_size: (f64, f64),
    to: (f64, f64),
    to_size: (f64, f64),
    lane: usize,
) -> Geometry {
    let offset = f64::from(u32::try_from(lane).unwrap_or(0)) * 48.0;
    if (to.0 - from.0).abs() > 280.0 {
        let (start, end) = if to.0 > from.0 {
            (
                (from.0 + from_size.0, from.1 + from_size.1 / 2.0),
                (to.0, to.1 + to_size.1 / 2.0),
            )
        } else {
            (
                (from.0, from.1 + from_size.1 / 2.0),
                (to.0 + to_size.0, to.1 + to_size.1 / 2.0),
            )
        };
        let middle = (
            f64::midpoint(start.0, end.0),
            f64::midpoint(start.1, end.1) - offset,
        );
        Geometry {
            path: format!(
                "M {} {} Q {} {} {} {} T {} {}",
                start.0,
                start.1,
                middle.0,
                start.1 - offset,
                middle.0,
                middle.1,
                end.0,
                end.1
            ),
            extent: middle,
        }
    } else {
        let same_column = (from.0 - to.0).abs() < 100.0;
        let (start, end, route) = if same_column {
            (
                (from.0, from.1 + from_size.1 / 2.0),
                (to.0, to.1 + to_size.1 / 2.0),
                from.0.min(to.0) - 140.0 - offset,
            )
        } else {
            (
                (from.0 + from_size.0, from.1 + from_size.1 / 2.0),
                (to.0 + to_size.0, to.1 + to_size.1 / 2.0),
                from.0.max(to.0) + 400.0 + offset,
            )
        };
        Geometry {
            path: format!(
                "M {} {} C {route} {}, {route} {}, {} {}",
                start.0, start.1, start.1, end.1, end.0, end.1
            ),
            extent: (
                (start.0 + 6.0 * route + end.0) / 8.0,
                f64::midpoint(start.1, end.1),
            ),
        }
    }
}
pub fn msat(value: u64) -> String {
    let whole = crate::model::sat(value / 1000);
    let remainder = value % 1000;
    if remainder == 0 {
        whole
    } else {
        format!("{whole}.{remainder:03}")
            .trim_end_matches('0')
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_view::{
        BalanceAmount, ComponentBalance, HoldingsObservation, LightningObservation, MintHolding,
        ObservedChannel,
    };
    use serde_json::json;
    fn cell() -> EnvironmentCell {
        let component = |id, kind| json!({"id":id,"kind":kind,"implementation":"test","conditions":[],"endpoints":[]});
        serde_json::from_value(json!({"id":"cell","layout_id":"cell:one","journal_read_at_unix":10,
            "runtime":{"state":"available","fetched_at_unix":10},
            "components":{"items":[component("alice","lightning"),component("bob","lightning"),component("wallet","wallet"),component("mint","mint")]},
            "links":{"items":[{"id":"peer","from":"alice","to":"bob","kind":"lightning_peer"},{"id":"declared-wallet","from":"wallet","to":"mint","kind":"network_path"}]},
            "sessions":{"items":[]},"activity":{"items":[]}})).unwrap()
    }
    fn balance(id: &str) -> ComponentBalance {
        ComponentBalance {
            bitcoin: None,
            component: id.into(),
            rollout_digest: Some("v1".into()),
            observed_at_unix: 10,
            error: None,
            amounts: vec![],
            block_height: None,
            lightning: None,
            holdings: None,
        }
    }
    fn usage() -> CellUsage {
        let channel = |point: &str, peer: &str, local, remote| ObservedChannel {
            channel_id: None,
            capacity_only: false,
            funding_outpoint: point.into(),
            peer_pubkey: peer.into(),
            active: true,
            capacity_msat: 100_000,
            local_msat: local,
            remote_msat: remote,
        };
        let mut alice = balance("alice");
        alice.lightning = Some(LightningObservation {
            observed_at_unix: 10,
            error: None,
            node_pubkey: Some("alice-key".into()),
            channels: vec![
                channel("point:0", "bob-key", 60_000, 39_000),
                channel("point:1", "bob-key", 20_000, 79_000),
            ],
        });
        let mut bob = balance("bob");
        bob.lightning = Some(LightningObservation {
            observed_at_unix: 10,
            error: None,
            node_pubkey: Some("bob-key".into()),
            channels: vec![channel("point:0", "alice-key", 39_000, 60_000)],
        });
        let mut wallet = balance("wallet");
        wallet.holdings = Some(HoldingsObservation {
            observed_at_unix: 10,
            error: None,
            mints: vec![MintHolding {
                id: "held".into(),
                mint: Some("mint".into()),
                amounts: vec![
                    BalanceAmount {
                        label: "Spendable".into(),
                        sat: 7,
                    },
                    BalanceAmount {
                        label: "Reserved".into(),
                        sat: 3,
                    },
                    BalanceAmount {
                        label: "Pending".into(),
                        sat: 900,
                    },
                ],
            }],
        });
        CellUsage {
            incarnation: "cell:one".into(),
            balances: vec![bob, wallet, alice],
            ..Default::default()
        }
    }
    #[test]
    fn peer_links_do_not_create_channels_and_observed_channels_are_deduplicated() {
        let cell = cell();
        assert!(edges(&cell, None, 10).is_empty());
        let result = edges(&cell, Some(&usage()), 10);
        assert_eq!(result.len(), 3);
        assert_eq!(
            result
                .iter()
                .filter(|e| matches!(e.kind, EdgeKind::Channel { .. }))
                .count(),
            2
        );
        assert!(
            result
                .iter()
                .any(|e| e.kind == EdgeKind::Holding { sat: 10 })
        );
        let first = result.iter().find(|e| e.id == "channel:point:0").unwrap();
        assert_eq!(first.from, "alice");
        assert_eq!(
            first.kind,
            EdgeKind::Channel {
                capacity: 100_000,
                local: 60_000,
                remote: 39_000,
                active: true
            }
        );
        assert_ne!(result[0].lane, result[1].lane);
    }
    #[test]
    fn zero_holdings_remove_edges_while_failed_observations_remain_stale() {
        let mut usage = usage();
        let wallet = usage
            .balances
            .iter_mut()
            .find(|b| b.component == "wallet")
            .unwrap();
        wallet.holdings.as_mut().unwrap().error = Some("failed".into());
        assert!(
            edges(&cell(), Some(&usage), 10)
                .iter()
                .any(|e| e.id.starts_with("holding:") && e.stale)
        );
        let held = usage
            .balances
            .iter_mut()
            .find(|b| b.component == "wallet")
            .unwrap()
            .holdings
            .as_mut()
            .unwrap();
        held.error = None;
        held.mints[0].amounts.retain(|a| a.label == "Pending");
        assert!(
            edges(&cell(), Some(&usage), 10)
                .iter()
                .all(|e| !matches!(e.kind, EdgeKind::Holding { .. }))
        );
        usage.incarnation = "cell:new".into();
        assert!(edges(&cell(), Some(&usage), 10).is_empty());
    }
    #[test]
    fn parallel_channels_have_separate_paths() {
        let a = geometry((380.0, 40.0), (380.0, 280.0), 0);
        let b = geometry((380.0, 40.0), (380.0, 280.0), 1);
        assert!((a.extent.0 - b.extent.0).abs() >= 36.0);
    }
    #[test]
    fn newer_peer_observation_retires_stale_closed_channel() {
        let mut usage = usage();
        for balance in &mut usage.balances {
            if balance.component == "alice" {
                balance.lightning.as_mut().unwrap().error = Some("failed".into());
            }
            if balance.component == "bob" {
                balance.lightning.as_mut().unwrap().channels.clear();
            }
        }
        assert!(
            edges(&cell(), Some(&usage), 10)
                .iter()
                .all(|e| !matches!(e.kind, EdgeKind::Channel { .. }))
        );
        assert_eq!(msat(123_456), "123.456");
    }
    #[test]
    fn embedded_lightning_channels_use_child_endpoints_and_shared_channel_ids() {
        let mut cell = cell();
        let mint = cell
            .components
            .items
            .iter_mut()
            .find(|c| c.id == "mint")
            .unwrap();
        mint.details = Some(serde_json::from_value(json!({"resolved_version":"0.18", "image":"test", "adapter_version":"1", "embedded":[{"id":"ldk-node","name":"LDK Node","kind":"lightning"}]})).unwrap());
        cell.links.items.push(
            serde_json::from_value(
                json!({"id":"backend","from":"mint","to":"alice","kind":"chain_backend"}),
            )
            .unwrap(),
        );
        let mut usage = usage();
        let mut ldk = balance("mint");
        ldk.lightning = usage
            .balances
            .iter()
            .find(|b| b.component == "bob")
            .unwrap()
            .lightning
            .clone();
        usage.balances.retain(|b| b.component != "bob");
        usage.balances.push(ldk);
        for balance in &mut usage.balances {
            if let Some(o) = &mut balance.lightning {
                for channel in &mut o.channels {
                    if channel.funding_outpoint == "point:0" {
                        channel.channel_id = Some("shared-id".into());
                    }
                    if balance.component == "mint" {
                        channel.funding_outpoint.clear();
                        channel.capacity_only = true;
                    }
                }
            }
        }
        let result = edges(&cell, Some(&usage), 10);
        let child = crate::canvas_model::embedded_id("mint", "ldk-node");
        assert_eq!(
            result
                .iter()
                .filter(|e| e.id == "channel:shared-id")
                .count(),
            1
        );
        assert_eq!(
            result
                .iter()
                .find(|e| e.id == "channel:shared-id")
                .unwrap()
                .to,
            child
        );
        assert_eq!(
            result
                .iter()
                .find(|e| e.id == "declared:backend")
                .unwrap()
                .from,
            child
        );
        let items = crate::canvas_model::nodes(&cell);
        let from = items.iter().find(|n| n.id == "alice").unwrap();
        let to = items.iter().find(|n| n.id == child).unwrap();
        let positions = crate::canvas_model::Positions::from([
            ("alice".into(), (0.0, 0.0)),
            ("mint".into(), (680.0, 0.0)),
            (child, (14.0, 164.0)),
        ]);
        let path = node_geometry(from, to, &positions, 0).path;
        assert!(path.ends_with("694 208"), "{path}");
    }
    #[test]
    fn runtime_bitcoin_peers_deduplicate_and_retire_when_disconnected() {
        let mut cell = cell();
        for component in &mut cell.components.items {
            if matches!(component.id.as_str(), "alice" | "bob") {
                component.kind = ComponentKind::Bitcoin;
            }
        }
        let mut usage = usage();
        for balance in &mut usage.balances {
            balance.lightning = None;
            if matches!(balance.component.as_str(), "alice" | "bob") {
                balance.bitcoin = Some(proofstorm_view::BitcoinObservation {
                    observed_at_unix: 10,
                    error: None,
                    peers: vec![if balance.component == "alice" {
                        "bob".into()
                    } else {
                        "alice".into()
                    }],
                });
            }
        }
        let result = edges(&cell, Some(&usage), 10);
        assert_eq!(
            result
                .iter()
                .filter(|e| e.kind == EdgeKind::BitcoinPeer)
                .count(),
            1
        );
        for balance in &mut usage.balances {
            if let Some(observation) = &mut balance.bitcoin {
                if balance.component == "alice" {
                    observation.error = Some("failed".into());
                } else {
                    observation.peers.clear();
                }
            }
        }
        assert!(
            edges(&cell, Some(&usage), 10)
                .iter()
                .all(|e| e.kind != EdgeKind::BitcoinPeer)
        );
    }
    #[test]
    fn two_embedded_ldk_nodes_connect_without_any_standalone_lightning_node() {
        let mut cell = cell();
        for c in &mut cell.components.items {
            if matches!(c.id.as_str(), "alice" | "bob") {
                c.kind = ComponentKind::Mint;
                c.details=Some(serde_json::from_value(json!({"resolved_version":"0.18","image":"test","adapter_version":"1","embedded":[{"id":"ldk-node","name":"LDK Node","kind":"lightning"}]})).unwrap());
            }
        }
        let result = edges(&cell, Some(&usage()), 10);
        let channels = result
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Channel { .. }))
            .collect::<Vec<_>>();
        assert_eq!(channels.len(), 2);
        assert!(channels.iter().all(|e| e.from
            == crate::canvas_model::embedded_id("alice", "ldk-node")
            && e.to == crate::canvas_model::embedded_id("bob", "ldk-node")));
    }
}
