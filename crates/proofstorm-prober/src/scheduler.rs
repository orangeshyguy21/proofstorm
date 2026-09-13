//! Fair scheduling of bounded batches. This module performs no I/O and has no cell-size cap.
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
};

use crate::{MAX_BATCH_TARGETS, Observation, PROTOCOL_VERSION, Request, Response, Target};

pub const MAX_INFLIGHT_BATCHES: usize = 8;
pub const CHECK_INTERVAL_MILLIS: u64 = 5_000;
pub const OBSERVATION_TTL_MILLIS: u64 = 30_000;

/// A worker's UID is part of the observation identity, not merely its reusable Pod name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub instance_key: String,
    pub incarnation: String,
    pub revision_digest: String,
    pub worker_uid: String,
}

/// A check is eligible only after the controller has observed its current workload
/// and ready Service endpoints. Runtime identity changes invalidate only that target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTarget {
    pub probe: Target,
    pub runtime_digest: String,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub identity: Identity,
    pub request: Request,
    pub started_at_millis: u64,
    id: u64,
    generation: u64,
    target_generations: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct CachedObservation {
    pub observation: Observation,
    pub runtime_digest: String,
    /// Conservative lower bound on check time in the controller's monotonic clock.
    /// Never rely on synchronized worker/controller wall clocks.
    pub observed_at_millis: u64,
}

impl CachedObservation {
    #[must_use]
    pub fn is_fresh(&self, now_millis: u64) -> bool {
        now_millis >= self.observed_at_millis
            && now_millis - self.observed_at_millis < OBSERVATION_TTL_MILLIS
    }
}

struct Cell {
    identity: Identity,
    generation: u64,
    targets: BTreeMap<String, ScheduledTarget>,
    target_generations: BTreeMap<String, u64>,
    due: BinaryHeap<Reverse<(u64, String)>>,
    observations: BTreeMap<String, CachedObservation>,
}

/// A single owner dispatches jobs and returns each completed or cancelled job exactly once.
/// Removed jobs retain global slots until their transport actually finishes or is cancelled.
#[derive(Default)]
pub struct Scheduler {
    cells: BTreeMap<String, Cell>,
    order: VecDeque<String>,
    inflight: BTreeMap<u64, String>,
    sequence: u64,
}

impl Scheduler {
    /// Register the complete desired target set. Unchanged registration is a no-op.
    ///
    /// # Errors
    /// Rejects duplicate or invalid targets without changing an existing cell.
    pub fn register(
        &mut self,
        identity: Identity,
        targets: Vec<ScheduledTarget>,
        now_millis: u64,
    ) -> Result<bool, &'static str> {
        if !crate::dns_label(&identity.instance_key)
            || [
                &identity.incarnation,
                &identity.revision_digest,
                &identity.worker_uid,
            ]
            .iter()
            .any(|value| value.is_empty() || value.len() > 128)
        {
            return Err("invalid_probe_identity");
        }
        let mut ids = BTreeSet::new();
        for chunk in targets.chunks(MAX_BATCH_TARGETS) {
            Request {
                protocol_version: PROTOCOL_VERSION,
                instance_key: identity.instance_key.clone(),
                revision_digest: identity.revision_digest.clone(),
                batch_id: "validation".into(),
                keep_alive: false,
                targets: chunk.iter().map(|target| target.probe.clone()).collect(),
            }
            .validate(&identity.instance_key)?;
            if chunk.iter().any(|target| {
                !ids.insert(target.probe.component.clone())
                    || target.runtime_digest.is_empty()
                    || target.runtime_digest.len() > 128
            }) {
                return Err("duplicate_probe_target");
            }
        }
        let targets: BTreeMap<_, _> = targets
            .into_iter()
            .map(|target| (target.probe.component.clone(), target))
            .collect();
        if self
            .cells
            .get(&identity.instance_key)
            .is_some_and(|cell| cell.identity == identity && cell.targets == targets)
        {
            return Ok(false);
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("probe_identity_exhausted")?;
        let key = identity.instance_key.clone();
        if !self.cells.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        if let Some(cell) = self
            .cells
            .get_mut(&key)
            .filter(|cell| cell.identity == identity)
        {
            let changed: BTreeSet<_> = cell
                .targets
                .keys()
                .chain(targets.keys())
                .filter(|key| cell.targets.get(*key) != targets.get(*key))
                .cloned()
                .collect();
            cell.observations.retain(|key, _| !changed.contains(key));
            cell.due.retain(|Reverse((_, key))| !changed.contains(key));
            cell.target_generations
                .retain(|key, _| !changed.contains(key));
            for key in changed.iter().filter(|key| targets.contains_key(*key)) {
                cell.target_generations.insert(key.clone(), self.sequence);
                cell.due.push(Reverse((now_millis, key.clone())));
            }
            cell.targets = targets;
            return Ok(true);
        }
        self.cells.insert(
            key,
            Cell {
                identity,
                generation: self.sequence,
                due: targets
                    .keys()
                    .map(|key| Reverse((now_millis, key.clone())))
                    .collect(),
                target_generations: targets
                    .keys()
                    .map(|key| (key.clone(), self.sequence))
                    .collect(),
                targets,
                observations: BTreeMap::new(),
            },
        );
        Ok(true)
    }

    pub fn remove(&mut self, instance_key: &str) {
        self.cells.remove(instance_key);
        self.order.retain(|key| key != instance_key);
    }

    #[must_use]
    pub fn job_is_current(&self, job: &Job) -> bool {
        self.cells
            .get(&job.identity.instance_key)
            .is_some_and(|cell| {
                cell.generation == job.generation
                    && job.target_generations.iter().any(|(key, generation)| {
                        cell.target_generations.get(key) == Some(generation)
                    })
            })
    }

    /// Take at most one due batch per cell in each turn of the fair queue.
    /// A large cell may use several slots; every cell gets a turn before it gets another.
    pub fn dispatch(&mut self, now_millis: u64) -> Option<Job> {
        if self.inflight.len() >= MAX_INFLIGHT_BATCHES {
            return None;
        }
        let next_id = self.sequence.checked_add(1)?;
        for _ in 0..self.order.len() {
            let key = self.order.pop_front()?;
            self.order.push_back(key.clone());
            if self
                .inflight
                .values()
                .filter(|active| *active == &key)
                .count()
                >= crate::MAX_BATCHES_PER_WORKER
            {
                continue;
            }
            let cell = self.cells.get_mut(&key)?;
            let mut targets = Vec::with_capacity(MAX_BATCH_TARGETS);
            while targets.len() < MAX_BATCH_TARGETS
                && cell
                    .due
                    .peek()
                    .is_some_and(|Reverse((due, _))| *due <= now_millis)
            {
                let Reverse((_, component)) = cell.due.pop()?;
                targets.push(cell.targets.get(&component)?.probe.clone());
            }
            if targets.is_empty() {
                continue;
            }
            self.sequence = next_id;
            let id = self.sequence;
            self.inflight.insert(id, key.clone());
            return Some(Job {
                identity: cell.identity.clone(),
                generation: cell.generation,
                target_generations: targets
                    .iter()
                    .map(|target| {
                        (
                            target.component.clone(),
                            cell.target_generations[&target.component],
                        )
                    })
                    .collect(),
                request: Request {
                    protocol_version: PROTOCOL_VERSION,
                    instance_key: key,
                    revision_digest: cell.identity.revision_digest.clone(),
                    batch_id: id.to_string(),
                    keep_alive: false,
                    targets,
                },
                started_at_millis: now_millis,
                id,
            });
        }
        None
    }

    /// Release a job's capacity and accept only a complete response for its current identity.
    /// Malformed, missing or rejected responses clear the selected observations to Unknown.
    /// Returns whether the current cell was affected; late results never recreate deleted state.
    pub fn complete(&mut self, job: &Job, response: Option<Response>, now_millis: u64) -> bool {
        if self.inflight.remove(&job.id).is_none() || !self.job_is_current(job) {
            return false;
        }
        let Some(cell) = self.cells.get_mut(&job.identity.instance_key) else {
            return false;
        };
        let observations = response.and_then(|response| validate_response(job, response));
        let current: BTreeSet<_> = job
            .target_generations
            .iter()
            .filter(|(key, generation)| cell.target_generations.get(*key) == Some(*generation))
            .map(|(key, _)| key.clone())
            .collect();
        for target in job
            .request
            .targets
            .iter()
            .filter(|target| current.contains(&target.component))
        {
            cell.observations.remove(&target.component);
            // Deterministic per-target jitter spreads subsequent due times without delaying first checks.
            let jitter = target.component.bytes().fold(0_u64, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(u64::from(byte))
            }) % 501;
            cell.due.push(Reverse((
                now_millis.saturating_add(CHECK_INTERVAL_MILLIS + jitter),
                target.component.clone(),
            )));
        }
        if let Some(observations) = observations {
            for observation in observations
                .into_iter()
                .filter(|observation| current.contains(&observation.component))
            {
                cell.observations.insert(
                    observation.component.clone(),
                    CachedObservation {
                        runtime_digest: cell.targets[&observation.component].runtime_digest.clone(),
                        observation,
                        observed_at_millis: job.started_at_millis,
                    },
                );
            }
        }
        true
    }

    #[must_use]
    pub fn observations(
        &self,
        identity: &Identity,
        now_millis: u64,
    ) -> BTreeMap<String, CachedObservation> {
        self.cells
            .get(&identity.instance_key)
            .filter(|cell| cell.identity == *identity)
            .map(|cell| {
                cell.observations
                    .iter()
                    .filter(|(_, observation)| observation.is_fresh(now_millis))
                    .map(|(component, observation)| (component.clone(), observation.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn inflight_batches(&self) -> usize {
        self.inflight.len()
    }
}

fn validate_response(job: &Job, response: Response) -> Option<Vec<Observation>> {
    let Response::Complete {
        protocol_version,
        instance_key,
        revision_digest,
        batch_id,
        observations,
    } = response
    else {
        return None;
    };
    if protocol_version != PROTOCOL_VERSION
        || instance_key != job.identity.instance_key
        || revision_digest != job.identity.revision_digest
        || batch_id != job.request.batch_id
        || observations.len() != job.request.targets.len()
    {
        return None;
    }
    let mut seen = BTreeSet::new();
    for observation in &observations {
        if !seen.insert(&observation.component)
            || !job.request.targets.iter().any(|target| {
                target.component == observation.component
                    && target.rollout_digest == observation.rollout_digest
            })
        {
            return None;
        }
    }
    Some(observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Outcome;

    fn identity(key: &str) -> Identity {
        Identity {
            instance_key: key.into(),
            incarnation: "cell-uid".into(),
            revision_digest: "revision".into(),
            worker_uid: "worker-uid".into(),
        }
    }
    fn targets(count: usize) -> Vec<ScheduledTarget> {
        (0..count)
            .map(|index| Target {
                component: format!("node-{index:05}"),
                rollout_digest: "rollout".into(),
                port: 80,
                http_path: None,
            })
            .map(|probe| ScheduledTarget {
                probe,
                runtime_digest: "runtime".into(),
            })
            .collect()
    }
    fn response(job: &Job) -> Response {
        Response::Complete {
            protocol_version: PROTOCOL_VERSION,
            instance_key: job.identity.instance_key.clone(),
            revision_digest: job.identity.revision_digest.clone(),
            batch_id: job.request.batch_id.clone(),
            observations: job
                .request
                .targets
                .iter()
                .map(|target| Observation {
                    component: target.component.clone(),
                    rollout_digest: target.rollout_digest.clone(),
                    outcome: Outcome::Reachable,
                    elapsed_micros: 12,
                    age_millis: 0,
                    http_status: None,
                })
                .collect(),
        }
    }

    #[test]
    fn large_cell_progresses_in_batches_without_starving_small_cells() {
        let mut scheduler = Scheduler::default();
        scheduler
            .register(identity("large"), targets(10_000), 0)
            .unwrap();
        for index in 0..20 {
            scheduler
                .register(identity(&format!("small-{index}")), targets(1), 0)
                .unwrap();
        }
        let mut observed = BTreeSet::new();
        let mut large_targets = BTreeSet::new();
        // Complete instantaneously so no second sweep is due during the test.
        for _ in 0..400 {
            let jobs = std::iter::from_fn(|| scheduler.dispatch(0)).collect::<Vec<_>>();
            assert!(jobs.len() <= MAX_INFLIGHT_BATCHES);
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                observed.insert(job.identity.instance_key.clone());
                if job.identity.instance_key == "large" {
                    large_targets.extend(
                        job.request
                            .targets
                            .iter()
                            .map(|target| target.component.clone()),
                    );
                }
                assert!(job.request.targets.len() <= MAX_BATCH_TARGETS);
                scheduler.complete(&job, Some(response(&job)), 0);
            }
            if large_targets.len() <= MAX_BATCH_TARGETS * 3 {
                assert!(observed.len() >= large_targets.len() / MAX_BATCH_TARGETS);
            }
        }
        assert_eq!(observed.len(), 21);
        assert_eq!(large_targets.len(), 10_000);
    }

    #[test]
    fn obsolete_jobs_hold_capacity_until_cancelled_but_cannot_write_into_a_replacement() {
        let mut scheduler = Scheduler::default();
        let first = identity("cell");
        scheduler.register(first.clone(), targets(1), 0).unwrap();
        let old = scheduler.dispatch(0).unwrap();
        let mut replacement = first.clone();
        replacement.worker_uid = "new-worker".into();
        scheduler
            .register(replacement.clone(), targets(1), 0)
            .unwrap();
        assert!(!scheduler.job_is_current(&old));
        assert_eq!(scheduler.inflight_batches(), 1);
        assert!(!scheduler.complete(&old, Some(response(&old)), 1));
        let current = scheduler.dispatch(1).unwrap();
        assert!(scheduler.complete(&current, Some(response(&current)), 2));
        assert_eq!(scheduler.observations(&replacement, 2).len(), 1);
        assert!(scheduler.observations(&first, 2).is_empty());
        scheduler.remove("cell");
        assert!(!scheduler.complete(&current, Some(response(&current)), 3));
        assert!(scheduler.observations(&replacement, 3).is_empty());
    }

    #[test]
    fn expired_and_invalid_observations_are_unknown_without_extending_freshness() {
        let mut scheduler = Scheduler::default();
        let identity = identity("cell");
        scheduler.register(identity.clone(), targets(2), 0).unwrap();
        let job = scheduler.dispatch(0).unwrap();
        scheduler.complete(&job, Some(response(&job)), 2_000);
        assert_eq!(
            scheduler
                .observations(&identity, OBSERVATION_TTL_MILLIS - 1)
                .len(),
            2
        );
        assert!(
            scheduler
                .observations(&identity, OBSERVATION_TTL_MILLIS)
                .is_empty()
        );
        let job = scheduler.dispatch(OBSERVATION_TTL_MILLIS).unwrap();
        let mut invalid = response(&job);
        if let Response::Complete { observations, .. } = &mut invalid {
            observations[1] = observations[0].clone();
        }
        scheduler.complete(&job, Some(invalid), OBSERVATION_TTL_MILLIS);
        assert!(
            scheduler
                .observations(&identity, OBSERVATION_TTL_MILLIS)
                .is_empty()
        );
        assert!(!scheduler.complete(&job, Some(response(&job)), OBSERVATION_TTL_MILLIS));
    }

    #[test]
    fn failed_batch_does_not_block_other_cells_and_retry_is_delayed() {
        let mut scheduler = Scheduler::default();
        for index in 0..=MAX_INFLIGHT_BATCHES {
            scheduler
                .register(identity(&format!("cell-{index}")), targets(1), 0)
                .unwrap();
        }
        let jobs = std::iter::from_fn(|| scheduler.dispatch(0)).collect::<Vec<_>>();
        assert_eq!(jobs.len(), MAX_INFLIGHT_BATCHES);
        assert!(scheduler.dispatch(0).is_none());
        scheduler.complete(&jobs[0], None, 2_000);
        let next = scheduler.dispatch(2_000).unwrap();
        assert!(!jobs.iter().any(|job| job.identity == next.identity));
        assert!(scheduler.observations(&jobs[0].identity, 2_000).is_empty());
    }

    #[test]
    fn target_restart_preserves_other_results_and_rejects_only_the_old_target() {
        let mut scheduler = Scheduler::default();
        let identity = identity("cell");
        let mut targets = targets(3);
        scheduler
            .register(identity.clone(), targets.clone(), 0)
            .unwrap();
        let initial = scheduler.dispatch(0).unwrap();
        scheduler.complete(&initial, Some(response(&initial)), 1);
        let inflight = scheduler.dispatch(10_000).unwrap();
        targets[0].runtime_digest = "replacement-pod".into();
        scheduler
            .register(identity.clone(), targets.clone(), 10_001)
            .unwrap();
        assert_eq!(scheduler.observations(&identity, 10_001).len(), 2);
        let replacement = scheduler.dispatch(10_001).unwrap();
        assert_eq!(replacement.request.targets.len(), 1);
        scheduler.complete(&replacement, Some(response(&replacement)), 10_002);
        scheduler.complete(&inflight, Some(response(&inflight)), 10_003);
        let observations = scheduler.observations(&identity, 10_003);
        assert_eq!(observations.len(), 3);
        assert_eq!(observations["node-00000"].runtime_digest, "replacement-pod");
        assert_eq!(observations["node-00000"].observed_at_millis, 10_001);
        assert!(scheduler.dispatch(10_003).is_none());
    }

    #[test]
    fn removing_and_readding_the_same_target_does_not_accept_its_old_reply() {
        let mut scheduler = Scheduler::default();
        let identity = identity("cell");
        scheduler.register(identity.clone(), targets(1), 0).unwrap();
        let old = scheduler.dispatch(0).unwrap();
        scheduler.register(identity.clone(), Vec::new(), 1).unwrap();
        scheduler.register(identity.clone(), targets(1), 2).unwrap();
        assert!(!scheduler.complete(&old, Some(response(&old)), 3));
        assert!(scheduler.observations(&identity, 3).is_empty());
        assert_eq!(scheduler.dispatch(3).unwrap().request.targets.len(), 1);
    }
}
