//! Watch-backed fleet scheduling. Worker lifetime is independent of scheduling turns.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures::StreamExt;
use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    core::v1::{PersistentVolumeClaim, Pod, Service},
    discovery::v1::EndpointSlice,
};
use kube::{
    Api, Client, ResourceExt,
    runtime::{
        reflector::ObjectRef,
        watcher::{self, Event},
    },
};
use proofstorm_core::{ComponentPlanContract, InventoryEntry, ProtocolObservation};
use proofstorm_kube::{
    ComponentObservationResources, INSTANCE_LABEL, PROTOCOL_PROBER_LABEL, ProofstormCell,
    probes::{ProbeObservation, container_incarnation, eligible_targets},
};
use proofstorm_prober::{
    Outcome, PORT, Response,
    scheduler::{Identity, Job, OBSERVATION_TTL_MILLIS, Scheduler},
    transport,
};
use tokio::{
    sync::{Notify, mpsc},
    task::JoinSet,
};

type Namespaced<K> = BTreeMap<String, BTreeMap<String, K>>;

#[derive(Clone)]
pub(super) struct Applied {
    pub signature: String,
    pub at: Instant,
    pub plans: Arc<Vec<ComponentPlanContract>>,
    pub inventory: Vec<InventoryEntry>,
    pub retained_storage: BTreeMap<String, String>,
    pub pruned: bool,
}

#[derive(Default, Clone)]
pub(super) struct Resources {
    pub deployments: Vec<Deployment>,
    pub stateful_sets: Vec<StatefulSet>,
    pub claims: Vec<PersistentVolumeClaim>,
    pub services: Vec<Service>,
    pub pods: Vec<Pod>,
    pub endpoints: Vec<EndpointSlice>,
    pub protocol: BTreeMap<String, ProbeObservation>,
}

impl Resources {
    pub fn observed(&self) -> ComponentObservationResources<'_> {
        ComponentObservationResources {
            deployments: &self.deployments,
            stateful_sets: &self.stateful_sets,
            persistent_volume_claims: &self.claims,
            services: &self.services,
            pods: &self.pods,
            endpoint_slices: &self.endpoints,
            protocol: &self.protocol,
        }
    }
}

struct Cell {
    object: ObjectRef<ProofstormCell>,
    uid: String,
    revision: String,
    plans: Arc<Vec<ComponentPlanContract>>,
    worker: Option<(Identity, String)>,
    applied: Option<Applied>,
    last_notify: u64,
    outcomes: BTreeMap<String, Outcome>,
}

#[derive(Default)]
struct State {
    cells: BTreeMap<String, Cell>,
    deployments: Namespaced<Deployment>,
    stateful_sets: Namespaced<StatefulSet>,
    claims: Namespaced<PersistentVolumeClaim>,
    services: Namespaced<Service>,
    pods: Namespaced<Pod>,
    endpoints: Namespaced<EndpointSlice>,
    ready: BTreeSet<&'static str>,
    dirty: BTreeSet<String>,
    notify: BTreeSet<String>,
    force_notify: BTreeSet<String>,
    scheduler: Scheduler,
}

enum CacheChange {
    Reset(bool),
    Namespace(String),
    None,
}

fn cache_event<K: ResourceExt>(cache: &mut Namespaced<K>, event: Event<K>) -> CacheChange {
    match event {
        Event::Init => {
            cache.clear();
            CacheChange::Reset(false)
        }
        Event::InitDone => CacheChange::Reset(true),
        Event::Apply(resource) | Event::InitApply(resource) => {
            let Some(namespace) = resource
                .namespace()
                .filter(|namespace| namespace.starts_with("proofstorm-"))
            else {
                return CacheChange::None;
            };
            cache
                .entry(namespace.clone())
                .or_default()
                .insert(resource.name_any(), resource);
            CacheChange::Namespace(namespace)
        }
        Event::Delete(resource) => {
            let Some(namespace) = resource.namespace() else {
                return CacheChange::None;
            };
            if let Some(items) = cache.get_mut(&namespace) {
                // A delayed delete for a reused name must not evict the new object.
                if items
                    .get(&resource.name_any())
                    .is_some_and(|current| current.uid() == resource.uid())
                {
                    items.remove(&resource.name_any());
                }
                if items.is_empty() {
                    cache.remove(&namespace);
                }
            }
            CacheChange::Namespace(namespace)
        }
    }
}

fn state_ready(manager: &Manager, kind: &'static str) -> bool {
    manager
        .state
        .lock()
        .expect("prober state lock")
        .ready
        .contains(kind)
}

impl State {
    fn changed(&mut self, kind: &'static str, change: CacheChange) {
        match change {
            CacheChange::Reset(ready) => {
                if ready {
                    self.ready.insert(kind);
                } else {
                    self.ready.remove(kind);
                }
                self.dirty.extend(self.cells.keys().cloned());
            }
            CacheChange::Namespace(namespace) => {
                if let Some(key) = namespace.strip_prefix("proofstorm-") {
                    if self.cells.contains_key(key) {
                        self.dirty.insert(key.into());
                    }
                }
            }
            CacheChange::None => {}
        }
    }

    fn notifications(&mut self, now: u64) -> Vec<(String, ObjectRef<ProofstormCell>)> {
        let refresh = self
            .cells
            .iter()
            .filter(|(_, cell)| now.saturating_sub(cell.last_notify) >= 10_000)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        self.notify.extend(refresh);
        let mut notifications = Vec::new();
        for key in std::mem::take(&mut self.notify) {
            let Some(cell) = self.cells.get(&key) else {
                continue;
            };
            if now.saturating_sub(cell.last_notify) < 500 {
                self.notify.insert(key);
                continue;
            }
            let outcomes = cell
                .worker
                .as_ref()
                .map(|(identity, _)| {
                    self.scheduler
                        .observations(identity, now)
                        .into_iter()
                        .map(|(key, value)| (key, value.observation.outcome))
                        .collect()
                })
                .unwrap_or_default();
            let forced = self.force_notify.remove(&key);
            let cell = self.cells.get_mut(&key).expect("registered cell");
            if forced || outcomes != cell.outcomes || now.saturating_sub(cell.last_notify) >= 10_000
            {
                cell.last_notify = now;
                cell.outcomes = outcomes;
                notifications.push((key, cell.object.clone()));
            }
        }
        notifications
    }

    fn resources(&self, key: &str) -> Resources {
        if self.ready.len() != 6 {
            return Resources::default();
        }
        let namespace = proofstorm_kube::instance_namespace(key);
        Resources {
            deployments: values(&self.deployments, &namespace),
            stateful_sets: values(&self.stateful_sets, &namespace),
            claims: values(&self.claims, &namespace),
            services: values(&self.services, &namespace),
            pods: values(&self.pods, &namespace),
            endpoints: values(&self.endpoints, &namespace),
            protocol: BTreeMap::new(),
        }
    }

    fn refresh(&mut self, image: &str, now: u64) {
        for key in std::mem::take(&mut self.dirty) {
            let resources = self.resources(&key);
            let Some(cell) = self.cells.get_mut(&key) else {
                continue;
            };
            cell.worker = resources
                .pods
                .iter()
                .find(|pod| {
                    pod.labels()
                        .get(PROTOCOL_PROBER_LABEL)
                        .is_some_and(|label| label == "true")
                        && pod.labels().get(INSTANCE_LABEL) == Some(&key)
                        && pod.metadata.deletion_timestamp.is_none()
                        && pod
                            .status
                            .as_ref()
                            .and_then(|status| status.phase.as_deref())
                            == Some("Running")
                        && pod
                            .status
                            .as_ref()
                            .and_then(|status| status.conditions.as_ref())
                            .is_some_and(|conditions| {
                                conditions.iter().any(|condition| {
                                    condition.type_ == "Ready" && condition.status == "True"
                                })
                            })
                        && pod.spec.as_ref().is_some_and(|spec| {
                            spec.containers.iter().any(|container| {
                                container.name == "worker"
                                    && container.image.as_deref() == Some(image)
                            })
                        })
                })
                .and_then(|pod| {
                    Some((
                        Identity {
                            instance_key: key.clone(),
                            incarnation: format!("{}:{}", cell.uid, container_incarnation(pod)),
                            revision_digest: cell.revision.clone(),
                            worker_uid: pod.uid()?,
                        },
                        pod.name_any(),
                    ))
                });
            if let Some((identity, _)) = &cell.worker {
                let targets = eligible_targets(&cell.plans, &resources.observed());
                if let Err(code) = self.scheduler.register(identity.clone(), targets, now) {
                    eprintln!("protocol target registration failed: {code}");
                    self.scheduler.remove(&key);
                }
            } else {
                self.scheduler.remove(&key);
            }
            self.force_notify.insert(key.clone());
            self.notify.insert(key);
        }
    }
}

fn values<K: Clone>(map: &Namespaced<K>, namespace: &str) -> Vec<K> {
    map.get(namespace)
        .map(|items| items.values().cloned().collect())
        .unwrap_or_default()
}

pub(super) struct Manager {
    pub image: String,
    client: Client,
    state: Mutex<State>,
    wake: Notify,
    triggers: mpsc::Sender<ObjectRef<ProofstormCell>>,
    origin: Instant,
}

impl Manager {
    pub fn new(
        client: Client,
        image: String,
    ) -> (Arc<Self>, mpsc::Receiver<ObjectRef<ProofstormCell>>) {
        let (triggers, receive) = mpsc::channel(128);
        (
            Arc::new(Self {
                image,
                client,
                state: Mutex::new(State::default()),
                wake: Notify::new(),
                triggers,
                origin: Instant::now(),
            }),
            receive,
        )
    }

    fn now(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn register(
        &self,
        cell: &ProofstormCell,
        plans: Arc<Vec<ComponentPlanContract>>,
    ) -> Result<(), super::Error> {
        let uid = cell
            .uid()
            .ok_or(super::Error::ControllerInvariant("cell has no UID"))?;
        let key = &cell.spec.instance_key;
        let object = ObjectRef::from_obj(cell);
        let mut state = self.state.lock().expect("prober state lock");
        if let Some(existing) = state.cells.get(key) {
            if existing.object != object {
                return Err(super::Error::ControllerInvariant(
                    "duplicate cell instance key",
                ));
            }
            if existing.uid == uid && existing.revision == cell.spec.revision_digest {
                return Ok(());
            }
        }
        state.cells.insert(
            key.clone(),
            Cell {
                object,
                uid,
                revision: cell.spec.revision_digest.clone(),
                plans,
                worker: None,
                applied: None,
                last_notify: 0,
                outcomes: BTreeMap::new(),
            },
        );
        state.dirty.insert(key.clone());
        self.wake.notify_one();
        Ok(())
    }

    pub fn remove(&self, key: &str) {
        let mut state = self.state.lock().expect("prober state lock");
        state.cells.remove(key);
        state.scheduler.remove(key);
        state.dirty.remove(key);
        state.notify.remove(key);
        state.force_notify.remove(key);
        self.wake.notify_one();
    }

    pub fn remember_applied(&self, key: &str, applied: Applied) {
        if let Some(cell) = self
            .state
            .lock()
            .expect("prober state lock")
            .cells
            .get_mut(key)
        {
            cell.applied = Some(applied);
        }
    }

    pub fn applied(&self, cell: &ProofstormCell) -> Option<Applied> {
        self.state
            .lock()
            .expect("prober state lock")
            .cells
            .get(&cell.spec.instance_key)
            .filter(|binding| Some(&binding.uid) == cell.metadata.uid.as_ref())?
            .applied
            .as_ref()
            .filter(|applied| {
                applied.signature == signature(cell)
                    && applied.at.elapsed()
                        < Duration::from_secs(if applied.pruned { 30 } else { 3 })
            })
            .cloned()
    }

    pub fn snapshot(&self, key: &str) -> Resources {
        let mut state = self.state.lock().expect("prober state lock");
        let now = self.now();
        let now_unix = super::now_unix();
        state.refresh(&self.image, now);
        let mut resources = state.resources(key);
        if let Some((identity, _)) = state.cells.get(key).and_then(|cell| cell.worker.as_ref()) {
            resources.protocol = state
                .scheduler
                .observations(identity, now)
                .into_iter()
                .map(|(key, observation)| {
                    let observed_at =
                        observed_at_unix(now_unix, now, observation.observed_at_millis);
                    (
                        key,
                        ProbeObservation {
                            rollout_digest: observation.observation.rollout_digest,
                            runtime_digest: observation.runtime_digest,
                            worker_uid: identity.worker_uid.clone(),
                            outcome: observation.observation.outcome,
                            timing: ProtocolObservation {
                                observed_at_unix: observed_at,
                                expires_at_unix: observed_at.saturating_add(
                                    i64::try_from(OBSERVATION_TTL_MILLIS / 1000).unwrap_or(30),
                                ),
                                elapsed_micros: observation.observation.elapsed_micros,
                            },
                        },
                    )
                })
                .collect();
        }
        resources
    }

    fn start_watches(self: Arc<Self>) -> JoinSet<()> {
        let mut watches = JoinSet::new();
        macro_rules! watch {
            ($kind:ty, $field:ident, $config:expr) => {{
                let manager = self.clone();
                watches.spawn(async move {
                    let mut delay = 1;
                    loop {
                        let mut stream =
                            watcher::watcher(Api::<$kind>::all(manager.client.clone()), $config)
                                .boxed();
                        while let Some(event) = stream.next().await {
                            let Ok(event) = event else {
                                break;
                            };
                            let mut state = manager.state.lock().expect("prober state lock");
                            let change = cache_event(&mut state.$field, event);
                            state.changed(stringify!($field), change);
                            drop(state);
                            if state_ready(&manager, stringify!($field)) {
                                delay = 1;
                            }
                            manager.wake.notify_one();
                        }
                        manager
                            .state
                            .lock()
                            .expect("prober state lock")
                            .changed(stringify!($field), CacheChange::Reset(false));
                        manager.wake.notify_one();
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        delay = (delay * 2).min(30);
                    }
                });
            }};
        }
        watch!(
            Deployment,
            deployments,
            watcher::Config::default().labels(INSTANCE_LABEL)
        );
        watch!(
            StatefulSet,
            stateful_sets,
            watcher::Config::default().labels(INSTANCE_LABEL)
        );
        watch!(
            PersistentVolumeClaim,
            claims,
            watcher::Config::default().labels(INSTANCE_LABEL)
        );
        watch!(
            Service,
            services,
            watcher::Config::default().labels(INSTANCE_LABEL)
        );
        watch!(Pod, pods, watcher::Config::default().labels(INSTANCE_LABEL));
        // Kubernetes propagates the parent Service labels onto its EndpointSlices.
        watch!(
            EndpointSlice,
            endpoints,
            watcher::Config::default().labels(INSTANCE_LABEL)
        );

        watches
    }

    pub async fn run(self: Arc<Self>) -> Result<(), &'static str> {
        let mut watches = self.clone().start_watches();
        let mut jobs = JoinSet::<(Job, Option<Response>)>::new();
        let mut active: BTreeMap<String, (Job, tokio::task::AbortHandle)> = BTreeMap::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let terminate = async {
            #[cfg(unix)]
            {
                let mut signal =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("termination signal");
                signal.recv().await;
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };
        tokio::pin!(terminate);
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                () = &mut terminate => break,
                Some(_) = watches.join_next() => {
                    // An unexpectedly lost watch must not keep renewing stale runtime proof.
                    return Err("protocol resource watcher stopped unexpectedly");
                }
                () = self.wake.notified() => {},
                _ = tick.tick() => {},
                Some(result) = jobs.join_next() => {
                    match result {
                        Ok((job, response)) => {
                            active.remove(&job.request.batch_id);
                            let mut state = self.state.lock().expect("prober state lock");
                            state.refresh(&self.image, self.now());
                            if state.scheduler.complete(&job, response, self.now()) { state.notify.insert(job.identity.instance_key); }
                        }
                        Err(error) => {
                            let id = active.iter().find(|(_, (_, handle))| handle.id() == error.id()).map(|(id, _)| id.clone());
                            if let Some((job, _)) = id.and_then(|id| active.remove(&id)) {
                                let mut state = self.state.lock().expect("prober state lock");
                                if state.scheduler.complete(&job, None, self.now()) {
                                    state.notify.insert(job.identity.instance_key);
                                }
                            }
                        }
                    }
                }
            }
            let notifications = {
                let mut state = self.state.lock().expect("prober state lock");
                let now = self.now();
                state.refresh(&self.image, now);
                let obsolete = active
                    .iter()
                    .filter(|(_, (job, _))| !state.scheduler.job_is_current(job))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in obsolete {
                    if let Some((_, handle)) = active.get(&id) {
                        handle.abort();
                    }
                }
                while let Some(job) = state.scheduler.dispatch(now) {
                    let Some((_, pod)) = state
                        .cells
                        .get(&job.identity.instance_key)
                        .and_then(|cell| cell.worker.as_ref())
                    else {
                        state.scheduler.complete(&job, None, now);
                        continue;
                    };
                    let client = self.client.clone();
                    let pod = pod.clone();
                    let task_job = job.clone();
                    let handle = jobs.spawn(async move {
                        let response = tokio::time::timeout(
                            Duration::from_secs(5),
                            probe_batch(client, &pod, &task_job),
                        )
                        .await
                        .ok()
                        .flatten();
                        (task_job, response)
                    });
                    active.insert(job.request.batch_id.clone(), (job, handle));
                }
                state.notifications(now)
            };
            for (key, object) in notifications {
                if self.triggers.try_send(object).is_err() {
                    // A full controller queue retains the request for the next pass.
                    let mut state = self.state.lock().expect("prober state lock");
                    state.force_notify.insert(key.clone());
                    state.notify.insert(key);
                }
            }
        }
        jobs.shutdown().await;
        watches.shutdown().await;
        Ok(())
    }
}

/// Anchors a monotonic observation time to the wall clock as it is now, never to the
/// wall clock at startup. A paused VM (host sleep) stops the monotonic clock while the
/// wall clock resyncs on wake, so a startup anchor falls permanently behind and every
/// observation would be stamped already expired. The age rounds up to stay conservative.
fn observed_at_unix(now_unix: i64, now_millis: u64, observed_at_millis: u64) -> i64 {
    let age = now_millis.saturating_sub(observed_at_millis).div_ceil(1000);
    now_unix.saturating_sub(i64::try_from(age).unwrap_or(i64::MAX))
}

pub(super) fn signature(cell: &ProofstormCell) -> String {
    proofstorm_core::digest_json(&(&cell.spec, cell.annotations()))
}

struct Forward(kube::api::Portforwarder);
impl Drop for Forward {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn probe_batch(client: Client, pod_name: &str, job: &Job) -> Option<Response> {
    let pods = Api::<Pod>::namespaced(
        client,
        &proofstorm_kube::instance_namespace(&job.identity.instance_key),
    );
    let pod = pods.get(pod_name).await.ok()?;
    if pod.uid().as_deref() != Some(&job.identity.worker_uid)
        || pod.metadata.deletion_timestamp.is_some()
    {
        return None;
    }
    let mut forward = Forward(pods.portforward(pod_name, &[PORT]).await.ok()?);
    let mut stream = forward.0.take_stream(PORT)?;
    transport::write(&mut stream, &job.request).await.ok()?;
    transport::read(&mut stream).await.ok()
}

#[cfg(test)]
mod tests;
