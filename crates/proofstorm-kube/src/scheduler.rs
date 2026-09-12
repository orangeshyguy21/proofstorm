use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ACTIVE_PROTOCOL_PROBER_CELLS: usize = 4;
/// A scheduling target, not a cell admission limit. An oversized cell runs alone.
pub const PROTOCOL_PROBE_SCHEDULING_BUDGET: usize = 256;
pub const PROTOCOL_PROBE_LEASE_SECONDS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolProbeSchedule {
    pub active_instance_keys: BTreeSet<String>,
    pub lease_id: String,
    pub epoch: i64,
    pub seconds_until_boundary: u64,
}

/// Select a fair rotating window, accounting for each cell's actual probe count.
/// Every cell gets a turn, including a cell larger than the scheduling budget.
#[must_use]
pub fn schedule_protocol_probers(
    candidate_instance_keys: impl IntoIterator<Item = (String, usize)>,
    now_unix: i64,
) -> ProtocolProbeSchedule {
    let candidates = candidate_instance_keys
        .into_iter()
        .filter(|(_, probes)| *probes > 0)
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect::<Vec<_>>();
    let now = now_unix.max(0);
    let epoch = now / PROTOCOL_PROBE_LEASE_SECONDS;
    let elapsed = now % PROTOCOL_PROBE_LEASE_SECONDS;
    let seconds_until_boundary = u64::try_from(PROTOCOL_PROBE_LEASE_SECONDS - elapsed).unwrap_or(1);
    let mut active_instance_keys = BTreeSet::new();
    if !candidates.is_empty() {
        // Advance by one: advancing by the slot count can starve a heavy cell
        // when only part of a window fits and the lengths share a divisor.
        let start =
            usize::try_from(i128::from(epoch) % i128::try_from(candidates.len()).unwrap_or(1))
                .unwrap_or_default();
        let mut probes = 0_usize;
        for offset in 0..candidates.len() {
            let (key, count) = &candidates[(start + offset) % candidates.len()];
            if active_instance_keys.len() == MAX_ACTIVE_PROTOCOL_PROBER_CELLS {
                break;
            }
            if active_instance_keys.is_empty()
                || probes.saturating_add(*count) <= PROTOCOL_PROBE_SCHEDULING_BUDGET
            {
                probes = probes.saturating_add(*count);
                active_instance_keys.insert(key.clone());
            }
        }
    }
    let lease_id = proofstorm_core::digest_json(&active_instance_keys);
    ProtocolProbeSchedule {
        active_instance_keys,
        lease_id,
        epoch,
        seconds_until_boundary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(count: usize) -> Vec<(String, usize)> {
        (0..count)
            .map(|index| (format!("instance-{index}"), 64))
            .collect()
    }

    #[test]
    fn schedule_is_globally_bounded_deterministic_and_order_independent() {
        let candidates = candidates(10);
        let first = schedule_protocol_probers(candidates.clone(), 31);
        let mut reversed = candidates;
        reversed.reverse();
        let repeated = schedule_protocol_probers(reversed, 31);
        assert_eq!(first, repeated);
        assert_eq!(first.active_instance_keys.len(), 4);
        assert_eq!(PROTOCOL_PROBE_SCHEDULING_BUDGET, 256);
        assert_eq!(first.seconds_until_boundary, 29);
    }

    #[test]
    fn rotating_windows_are_fair_and_small_sets_do_not_churn() {
        let candidate_set = candidates(10);
        let observed = (0..10)
            .flat_map(|epoch| {
                schedule_protocol_probers(
                    candidate_set.clone(),
                    epoch * PROTOCOL_PROBE_LEASE_SECONDS,
                )
                .active_instance_keys
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            observed,
            candidate_set.into_iter().map(|(key, _)| key).collect()
        );

        let small = candidates(3);
        let first = schedule_protocol_probers(small.clone(), 0);
        let later = schedule_protocol_probers(small, 300);
        assert_eq!(first.active_instance_keys, later.active_instance_keys);
        assert_eq!(first.lease_id, later.lease_id);
    }

    #[test]
    fn removing_a_candidate_immediately_fills_the_available_slot() {
        let before = schedule_protocol_probers(candidates(5), 0);
        let removed = before
            .active_instance_keys
            .iter()
            .next()
            .expect("active candidate")
            .clone();
        let after = schedule_protocol_probers(
            candidates(5)
                .into_iter()
                .filter(|(candidate, _)| candidate != &removed),
            0,
        );
        assert_eq!(after.active_instance_keys.len(), 4);
        assert!(!after.active_instance_keys.contains(&removed));
    }

    #[test]
    fn weighted_windows_admit_large_cells_without_starving_any_candidate() {
        let candidates = vec![
            ("a".into(), 300),
            ("b".into(), 200),
            ("c".into(), 100),
            ("d".into(), 65),
            ("e".into(), 1),
            ("f".into(), 1),
            ("g".into(), 1),
            ("h".into(), 1),
        ];
        let mut observed = BTreeSet::new();
        for epoch in 0..8 {
            let schedule = schedule_protocol_probers(candidates.clone(), epoch * 30);
            let active = &schedule.active_instance_keys;
            let probes: usize = candidates
                .iter()
                .filter(|(key, _)| active.contains(key))
                .map(|(_, probes)| probes)
                .sum();
            assert!(probes <= PROTOCOL_PROBE_SCHEDULING_BUDGET || active.len() == 1);
            observed.extend(active.iter().cloned());
        }
        assert_eq!(observed.len(), candidates.len());
        assert_eq!(
            schedule_protocol_probers(candidates, 0).active_instance_keys,
            BTreeSet::from(["a".into()])
        );
    }
}
