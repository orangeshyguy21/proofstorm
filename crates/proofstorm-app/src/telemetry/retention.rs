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
}
