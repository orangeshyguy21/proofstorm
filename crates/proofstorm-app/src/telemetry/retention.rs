//! Retain last successful relationship observations only within the same rollout.
use proofstorm_view::SystemView;

pub(super) fn retain(next: &mut SystemView, previous: &SystemView) {
    if next.error.is_some() {
        next.cells.clone_from(&previous.cells);
        for cell in &mut next.cells {
            cell.error = Some("Observation unavailable".into());
        }
    }
    for cell in &mut next.cells {
        let Some(old) = previous
            .cells
            .iter()
            .find(|old| !cell.incarnation.is_empty() && old.incarnation == cell.incarnation)
        else {
            continue;
        };
        if cell.error.is_some() {
            cell.balances.clone_from(&old.balances);
        }
        for balance in &mut cell.balances {
            let Some(old) = old.balances.iter().find(|old| {
                old.component == balance.component
                    && old.rollout_digest.is_some()
                    && old.rollout_digest == balance.rollout_digest
            }) else {
                continue;
            };
            if let Some(current) = &mut balance.bitcoin {
                if current.error.is_some() || cell.error.is_some() {
                    if let Some(old) = &old.bitcoin {
                        *current = old.clone();
                    }
                    current.error = Some("Peer observation unavailable".into());
                }
            }
            if let Some(current) = &mut balance.lightning {
                if current.error.is_some() || cell.error.is_some() {
                    if let Some(old) = &old.lightning {
                        *current = old.clone();
                    }
                    current.error = Some("Channel observation unavailable".into());
                }
            }
            if let Some(current) = &mut balance.holdings {
                if current.error.is_some() || cell.error.is_some() {
                    if let Some(old) = &old.holdings {
                        *current = old.clone();
                    }
                    current.error = Some("Holdings observation unavailable".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_view::{
        BalanceAmount, CellUsage, ComponentBalance, HoldingsObservation, MintHolding,
    };
    fn snapshot() -> SystemView {
        SystemView {
            cells: vec![CellUsage {
                incarnation: "cell:one".into(),
                balances: vec![ComponentBalance {
                    bitcoin: None,
                    component: "wallet".into(),
                    rollout_digest: Some("v1".into()),
                    observed_at_unix: 10,
                    error: None,
                    amounts: vec![],
                    block_height: None,
                    lightning: None,
                    holdings: Some(HoldingsObservation {
                        observed_at_unix: 10,
                        error: None,
                        mints: vec![MintHolding {
                            id: "mint".into(),
                            mint: Some("mint".into()),
                            amounts: vec![BalanceAmount {
                                label: "Spendable".into(),
                                sat: 42,
                            }],
                        }],
                    }),
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn failed_reads_keep_last_holdings_but_zero_and_new_rollouts_do_not() {
        let old = snapshot();
        let mut next = old.clone();
        next.cells[0].balances[0].holdings = Some(HoldingsObservation {
            error: Some("failed".into()),
            ..Default::default()
        });
        retain(&mut next, &old);
        let held = next.cells[0].balances[0].holdings.as_ref().unwrap();
        assert_eq!(held.mints[0].held_sat(), 42);
        assert_eq!(held.observed_at_unix, 10);
        assert!(held.error.is_some());
        next.cells[0].balances[0].holdings = Some(HoldingsObservation {
            observed_at_unix: 20,
            ..Default::default()
        });
        retain(&mut next, &old);
        assert!(
            next.cells[0].balances[0]
                .holdings
                .as_ref()
                .unwrap()
                .mints
                .is_empty()
        );
        next.cells[0].balances[0].holdings.as_mut().unwrap().error = Some("failed".into());
        next.cells[0].balances[0].rollout_digest = Some("v2".into());
        retain(&mut next, &old);
        assert!(
            next.cells[0].balances[0]
                .holdings
                .as_ref()
                .unwrap()
                .mints
                .is_empty()
        );
    }
    #[test]
    fn peer_and_channel_failures_keep_evidence_only_within_the_same_rollout() {
        let mut old = snapshot();
        let b = &mut old.cells[0].balances[0];
        b.bitcoin = Some(proofstorm_view::BitcoinObservation {
            observed_at_unix: 10,
            error: None,
            peers: vec!["peer".into()],
        });
        b.lightning = Some(proofstorm_view::LightningObservation {
            observed_at_unix: 10,
            node_pubkey: Some("node".into()),
            ..Default::default()
        });
        let mut next = old.clone();
        next.cells[0].balances[0].bitcoin = Some(proofstorm_view::BitcoinObservation {
            error: Some("failed".into()),
            ..Default::default()
        });
        next.cells[0].balances[0].lightning = Some(proofstorm_view::LightningObservation {
            error: Some("failed".into()),
            ..Default::default()
        });
        let failed = next.clone();
        retain(&mut next, &old);
        let b = &next.cells[0].balances[0];
        assert_eq!(b.bitcoin.as_ref().unwrap().peers, vec!["peer"]);
        assert_eq!(b.bitcoin.as_ref().unwrap().observed_at_unix, 10);
        assert!(b.bitcoin.as_ref().unwrap().error.is_some());
        assert_eq!(
            b.lightning.as_ref().unwrap().node_pubkey.as_deref(),
            Some("node")
        );
        next = failed;
        next.cells[0].balances[0].rollout_digest = Some("replacement".into());
        retain(&mut next, &old);
        assert!(
            next.cells[0].balances[0]
                .bitcoin
                .as_ref()
                .unwrap()
                .peers
                .is_empty()
        );
        assert!(
            next.cells[0].balances[0]
                .lightning
                .as_ref()
                .unwrap()
                .node_pubkey
                .is_none()
        );
    }
}
