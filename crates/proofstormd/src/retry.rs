//! Per-object exponential backoff for failed reconciles.
//!
//! kube-runtime requeues after the error policy's delay, but any earlier
//! trigger reconciles sooner: a watch event, or a prober notification sent as
//! soon as `apply` registers the cell. A persistently failing object could
//! therefore loop as fast as it re-triggers itself. The controllers consult this
//! state before doing any work, so the delay holds whatever the trigger. A spec
//! change (new fingerprint) is reconciled immediately and restarts the schedule.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use kube::{Resource, ResourceExt, runtime::reflector::ObjectRef};

pub(crate) const INITIAL_DELAY: Duration = Duration::from_secs(1);
pub(crate) const MAXIMUM_DELAY: Duration = Duration::from_secs(60);

/// Delay after `failures` consecutive failures (the first failure is 1).
pub(crate) fn delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(16);
    INITIAL_DELAY
        .saturating_mul(1 << exponent)
        .min(MAXIMUM_DELAY)
}

struct Entry {
    failures: u32,
    fingerprint: String,
    not_before: Instant,
}

#[derive(Default)]
pub(crate) struct Backoff {
    entries: Mutex<HashMap<String, Entry>>,
}

impl Backoff {
    /// Time left before this object may reconcile again with the same spec.
    pub(crate) fn remaining(&self, key: &str, fingerprint: &str, now: Instant) -> Option<Duration> {
        let entries = self.entries.lock().expect("retry state lock");
        let entry = entries.get(key)?;
        if entry.fingerprint != fingerprint {
            return None;
        }
        let left = entry.not_before.saturating_duration_since(now);
        (!left.is_zero()).then_some(left)
    }

    /// Record a failure and return the delay before the next attempt.
    pub(crate) fn failed(&self, key: &str, fingerprint: &str, now: Instant) -> Duration {
        let mut entries = self.entries.lock().expect("retry state lock");
        let failures = match entries.get(key) {
            Some(entry) if entry.fingerprint == fingerprint => entry.failures.saturating_add(1),
            _ => 1,
        };
        let wait = delay(failures);
        entries.insert(
            key.to_owned(),
            Entry {
                failures,
                fingerprint: fingerprint.to_owned(),
                not_before: now + wait,
            },
        );
        wait
    }

    pub(crate) fn succeeded(&self, key: &str) {
        self.entries.lock().expect("retry state lock").remove(key);
    }
}

/// Stable object identity and the spec fingerprint that resets its backoff:
/// generation, the cell desired-generation annotation, and deletion.
pub(crate) fn identity<K>(object: &K) -> (String, String)
where
    K: Resource<DynamicType = ()>,
{
    let fingerprint = format!(
        "{}:{}:{}",
        object.meta().generation.unwrap_or_default(),
        object
            .annotations()
            .get("proofstorm.dev/desired-generation")
            .map_or("", String::as_str),
        object.meta().deletion_timestamp.is_some()
    );
    (ObjectRef::from_obj(object).to_string(), fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_doubles_from_one_second_and_caps_at_one_minute() {
        let schedule = (1..=9).map(delay).collect::<Vec<_>>();
        assert_eq!(
            schedule,
            [1, 2, 4, 8, 16, 32, 60, 60, 60].map(Duration::from_secs)
        );
        assert_eq!(delay(0), INITIAL_DELAY);
        assert_eq!(delay(u32::MAX), MAXIMUM_DELAY);
    }

    #[test]
    fn failures_hold_triggers_until_due_and_reset_on_success_or_spec_change() {
        let backoff = Backoff::default();
        let start = Instant::now();
        assert_eq!(backoff.remaining("cell", "g1", start), None);

        assert_eq!(backoff.failed("cell", "g1", start), Duration::from_secs(1));
        // An immediate re-trigger (prober or watch event) is held back.
        assert_eq!(
            backoff.remaining("cell", "g1", start),
            Some(Duration::from_secs(1))
        );
        let later = start + Duration::from_secs(1);
        assert_eq!(backoff.remaining("cell", "g1", later), None);
        assert_eq!(backoff.failed("cell", "g1", later), Duration::from_secs(2));
        assert_eq!(backoff.failed("cell", "g1", later), Duration::from_secs(4));

        // Other objects are independent.
        assert_eq!(backoff.remaining("other", "g1", later), None);

        // An edited spec is reconciled at once and restarts the schedule.
        assert_eq!(backoff.remaining("cell", "g2", later), None);
        assert_eq!(backoff.failed("cell", "g2", later), Duration::from_secs(1));

        backoff.succeeded("cell");
        assert_eq!(backoff.remaining("cell", "g2", later), None);
        assert_eq!(backoff.failed("cell", "g2", later), Duration::from_secs(1));
    }
}
