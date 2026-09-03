use std::collections::BTreeSet;

pub const MAX_ACTIVE_PROTOCOL_PROBER_LABS: usize = 4;
pub const MAX_PROTOCOL_PROBES_PER_LAB: usize = 64;
pub const MAX_GLOBAL_PROTOCOL_PROBES: usize =
    MAX_ACTIVE_PROTOCOL_PROBER_LABS * MAX_PROTOCOL_PROBES_PER_LAB;
pub const PROTOCOL_PROBE_LEASE_SECONDS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolProbeSchedule {
    pub active_instance_keys: BTreeSet<String>,
    pub lease_id: String,
    pub epoch: i64,
    pub seconds_until_boundary: u64,
}

/// Select a deterministic bounded rotating window of probe-bearing labs.
#[must_use]
pub fn schedule_protocol_probers(
    candidate_instance_keys: impl IntoIterator<Item = String>,
    now_unix: i64,
) -> ProtocolProbeSchedule {
    let candidates = candidate_instance_keys
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let now = now_unix.max(0);
    let epoch = now / PROTOCOL_PROBE_LEASE_SECONDS;
    let elapsed = now % PROTOCOL_PROBE_LEASE_SECONDS;
    let seconds_until_boundary = u64::try_from(PROTOCOL_PROBE_LEASE_SECONDS - elapsed).unwrap_or(1);
    let mut active_instance_keys = BTreeSet::new();
    if !candidates.is_empty() {
        let limit = MAX_ACTIVE_PROTOCOL_PROBER_LABS.min(candidates.len());
        let start = usize::try_from(
            (i128::from(epoch) * i128::try_from(limit).unwrap_or(0))
                % i128::try_from(candidates.len()).unwrap_or(1),
        )
        .unwrap_or_default();
        for offset in 0..limit {
            active_instance_keys.insert(candidates[(start + offset) % candidates.len()].clone());
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

    fn candidates(count: usize) -> Vec<String> {
        (0..count)
            .map(|index| format!("instance-{index}"))
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
        assert_eq!(MAX_GLOBAL_PROTOCOL_PROBES, 256);
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
        assert_eq!(observed, candidate_set.into_iter().collect());

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
                .filter(|candidate| candidate != &removed),
            0,
        );
        assert_eq!(after.active_instance_keys.len(), 4);
        assert!(!after.active_instance_keys.contains(&removed));
    }
}
