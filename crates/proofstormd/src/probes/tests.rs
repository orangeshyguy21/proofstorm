use super::*;
use proofstorm_kube::{COMPONENT_LABEL, ProofstormComponent, ROLLOUT_DIGEST_ANNOTATION};
use proofstorm_prober::{Observation, PROTOCOL_VERSION};
use serde_json::json;

const KEY: &str = "i0123456789012345678";
const IMAGE: &str = "controller:test";

fn fixture() -> (proofstorm_kube::LoadedCell, Resources) {
    let mut spec: proofstorm_core::CellSpec =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json")).unwrap();
    spec.components.retain(|component| component.id == "chain");
    spec.links.clear();
    let lock = proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).unwrap();
    let rendered = proofstorm_kube::render_cell(KEY, "revision", &spec, &lock).unwrap();
    let plan = &rendered.plans[0];
    let namespace = proofstorm_kube::instance_namespace(KEY);
    let mut workload = rendered.stateful_sets[0].clone();
    workload.metadata.generation = Some(1);
    workload.status = Some(serde_json::from_value(json!({"observedGeneration":1,"replicas":1,
        "readyReplicas":1,"updatedReplicas":1,"currentRevision":"current","updateRevision":"current"})).unwrap());
    let mut service = rendered.services[0].clone();
    service.metadata.uid = Some("service-uid".into());
    let ports: Vec<_> = plan
        .target_descriptor
        .ports
        .iter()
        .map(|(name, port)| json!({"name":name.replace('_', "-"),"port":port,"protocol":"TCP"}))
        .collect();
    let endpoint: EndpointSlice = serde_json::from_value(json!({
        "metadata":{"name":"chain-endpoints","namespace":namespace,"uid":"slice-uid",
            "labels":{"kubernetes.io/service-name":"chain"},
            "ownerReferences":[{"apiVersion":"v1","kind":"Service","name":"chain","uid":"service-uid"}]},
        "addressType":"IPv4","ports":ports,
        "endpoints":[{"addresses":["10.0.0.2"],"conditions":{"ready":true},"targetRef":{"kind":"Pod","name":"chain-0","uid":"chain-uid"}}]
    })).unwrap();
    let pod: Pod = serde_json::from_value(json!({
        "metadata":{"name":"chain-0","namespace":namespace,"uid":"chain-uid",
            "labels":{INSTANCE_LABEL:KEY,COMPONENT_LABEL:"chain"},
            "annotations":{ROLLOUT_DIGEST_ANNOTATION:plan.rollout_digest}},
        "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}
    }))
    .unwrap();
    let worker: Pod = serde_json::from_value(json!({
        "metadata":{"name":"worker","namespace":namespace,"uid":"worker-uid",
            "labels":{INSTANCE_LABEL:KEY,PROTOCOL_PROBER_LABEL:"true"}},
        "spec":{"containers":[{"name":"worker","image":IMAGE}]},
        "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],
            "containerStatuses":[{"name":"worker","image":IMAGE,"imageID":"image","restartCount":0,"ready":true}]}
    })).unwrap();
    let mut cell = proofstorm_kube::LoadedCell::from_spec(
        ProofstormCell::new(
            "cell",
            proofstorm_kube::ProofstormCellSpec {
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: KEY.into(),
                revision_digest: "revision".into(),
            },
        ),
        spec,
        lock,
    );
    cell.metadata.namespace = Some("system".into());
    cell.metadata.uid = Some("cell-uid".into());
    cell.metadata.generation = Some(1);
    cell.metadata.finalizers = Some(vec![crate::FINALIZER.into()]);
    (
        cell,
        Resources {
            stateful_sets: vec![workload],
            services: vec![service],
            endpoints: vec![endpoint],
            pods: vec![pod, worker],
            ..Default::default()
        },
    )
}

fn populate(state: &mut State, resources: Resources) {
    macro_rules! fill {
        ($field:ident, $items:expr) => {{
            cache_event(&mut state.$field, Event::Init);
            for resource in $items {
                cache_event(&mut state.$field, Event::InitApply(resource));
            }
            state.changed(stringify!($field), CacheChange::Reset(true));
        }};
    }
    fill!(deployments, resources.deployments);
    fill!(stateful_sets, resources.stateful_sets);
    fill!(claims, resources.claims);
    fill!(services, resources.services);
    fill!(pods, resources.pods);
    fill!(endpoints, resources.endpoints);
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
                elapsed_micros: 1,
                age_millis: 0,
                http_status: None,
            })
            .collect(),
    }
}

fn no_api() -> Client {
    Client::new(
        tower::service_fn(|_: http::Request<kube::client::Body>| async {
            panic!("watch-cache operation unexpectedly contacted the API");
            #[allow(unreachable_code)]
            Ok::<_, std::convert::Infallible>(http::Response::new(kube::client::Body::empty()))
        }),
        "system",
    )
}

#[tokio::test]
async fn watch_relist_and_worker_restart_invalidate_inflight_results() {
    let (cell, resources) = fixture();
    let plans = Arc::new(
        proofstorm_kube::compile_component_plans(
            KEY,
            "revision",
            &cell.revision.cell,
            &cell.revision.lock,
        )
        .unwrap(),
    );
    let (manager, _receive) = Manager::new(no_api(), IMAGE.into());
    manager.register(&cell, plans).unwrap();
    let mut state = manager.state.lock().unwrap();
    populate(&mut state, resources.clone());
    state.refresh(IMAGE, 0);
    let old = state.scheduler.dispatch(0).expect("ready target scheduled");
    state.changed("pods", CacheChange::Reset(false));
    state.refresh(IMAGE, 1);
    assert!(state.resources(KEY).pods.is_empty());
    assert!(!state.scheduler.complete(&old, Some(response(&old)), 2));
    populate(&mut state, resources);
    state.refresh(IMAGE, 3);
    let before_restart = state.scheduler.dispatch(3).unwrap();
    let worker = state
        .pods
        .get_mut(&proofstorm_kube::instance_namespace(KEY))
        .unwrap()
        .get_mut("worker")
        .unwrap();
    worker
        .status
        .as_mut()
        .unwrap()
        .container_statuses
        .as_mut()
        .unwrap()[0]
        .restart_count = 1;
    state.dirty.insert(KEY.into());
    state.refresh(IMAGE, 4);
    assert!(
        !state
            .scheduler
            .complete(&before_restart, Some(response(&before_restart)), 5)
    );
    let current = state.scheduler.dispatch(5).unwrap();
    assert!(
        state
            .scheduler
            .complete(&current, Some(response(&current)), 6)
    );
    assert_eq!(state.scheduler.observations(&current.identity, 6).len(), 1);
    drop(state);
    manager.remove(KEY);
    assert!(manager.snapshot(KEY).protocol.is_empty());
}

#[test]
fn cache_deletion_is_uid_bound_and_releases_namespace_entries() {
    let (_, resources) = fixture();
    let mut cache = Namespaced::new();
    let old = resources.pods[0].clone();
    let mut replacement = old.clone();
    replacement.metadata.uid = Some("new-pod".into());
    cache_event(&mut cache, Event::Apply(old.clone()));
    cache_event(&mut cache, Event::Apply(replacement.clone()));
    cache_event(&mut cache, Event::Delete(old));
    assert_eq!(cache[&proofstorm_kube::instance_namespace(KEY)].len(), 1);
    cache_event(&mut cache, Event::Delete(replacement));
    assert!(cache.is_empty());
}

type Components = Arc<Mutex<BTreeMap<String, ProofstormComponent>>>;

/// A fake API that keeps the cell's status, component objects and their
/// statuses, and records each write.
fn recording_client(
    observed: &Arc<Mutex<ProofstormCell>>,
    objects: &Components,
    requests: &Arc<Mutex<Vec<String>>>,
) -> Client {
    Client::new(
        tower::service_fn({
            let observed = observed.clone();
            let objects = objects.clone();
            let requests = requests.clone();
            move |request: http::Request<kube::client::Body>| {
                let observed = observed.clone();
                let objects = objects.clone();
                let requests = requests.clone();
                async move {
                    assert_eq!(request.method(), http::Method::PATCH);
                    let uri = request.uri().clone();
                    let path = uri.path().to_owned();
                    requests.lock().unwrap().push(uri.to_string());
                    let body = request.into_body().collect_bytes().await.unwrap();
                    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let response = if path.ends_with("/proofstormcells/cell/status") {
                        let mut observed = observed.lock().unwrap();
                        if uri
                            .query()
                            .unwrap_or_default()
                            .contains("proofstormd-heartbeat")
                        {
                            observed.status.as_mut().unwrap().protocol_freshness =
                                serde_json::from_value(
                                    value["status"]["protocolFreshness"].clone(),
                                )
                                .unwrap();
                        } else {
                            observed.status =
                                Some(serde_json::from_value(value["status"].clone()).unwrap());
                        }
                        serde_json::to_vec(&*observed).unwrap()
                    } else {
                        let rest = path
                            .split("/proofstormcomponents/")
                            .nth(1)
                            .expect("only cell and component status writes");
                        let mut objects = objects.lock().unwrap();
                        let object = if let Some(name) = rest.strip_suffix("/status") {
                            let object = objects.get_mut(name).expect("status of a created object");
                            object.status =
                                Some(serde_json::from_value(value["status"].clone()).unwrap());
                            object.clone()
                        } else {
                            let object: ProofstormComponent =
                                serde_json::from_value(value).unwrap();
                            objects.insert(rest.to_owned(), object.clone());
                            object
                        };
                        serde_json::to_vec(&object).unwrap()
                    };
                    Ok::<_, std::convert::Infallible>(
                        http::Response::builder()
                            .header("content-type", "application/json")
                            .body(kube::client::Body::from(response))
                            .unwrap(),
                    )
                }
            }
        }),
        "system",
    )
}

#[tokio::test]
async fn readiness_reconciliation_uses_cached_resources_and_writes_only_changed_status() {
    let (cell, resources) = fixture();
    let observed = Arc::new(Mutex::new(cell.resource.clone()));
    let objects = Arc::new(Mutex::new(BTreeMap::<String, ProofstormComponent>::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = recording_client(&observed, &objects, &requests);
    let (manager, _receive) = Manager::new(client.clone(), IMAGE.into());
    let plans = Arc::new(
        proofstorm_kube::compile_component_plans(
            KEY,
            "revision",
            &cell.revision.cell,
            &cell.revision.lock,
        )
        .unwrap(),
    );
    manager.register(&cell, plans.clone()).unwrap();
    populate(&mut manager.state.lock().unwrap(), resources);
    manager.remember_applied(
        KEY,
        Applied {
            signature: signature(&cell),
            at: Instant::now(),
            plans,
            inventory: vec![],
            retained_storage: BTreeMap::new(),
            pruned: true,
            documents: Arc::default(),
            held: Arc::default(),
        },
    );
    let (component_store, mut component_writer) = kube::runtime::reflector::store();
    let context = Arc::new(crate::Context {
        components: component_store,
        ..crate::Context::for_tests(client, manager.clone())
    });
    // This fake API serves no revision objects; the controller has read this one.
    context.revisions.insert((*cell.revision).clone());
    component_writer.apply_watcher_event(&kube::runtime::watcher::Event::InitDone);
    crate::reconcile(Arc::new(cell.resource.clone()), context.clone())
        .await
        .unwrap();
    // One cell status write, then each component object and its status.
    let components = cell.revision.cell.components.len();
    assert_eq!(requests.lock().unwrap().len(), 1 + 2 * components);
    assert!(
        observed
            .lock()
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .components
            .is_empty(),
        "the cell status never stores component statuses"
    );
    for object in objects.lock().unwrap().values() {
        component_writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(object.clone()));
    }
    let current = observed.lock().unwrap().clone();
    crate::reconcile(Arc::new(current), context).await.unwrap();
    assert_eq!(
        requests.lock().unwrap().len(),
        1 + 2 * components,
        "unchanged state writes nothing"
    );
    let mut changed = cell;
    changed.spec.revision_digest = "changed".into();
    assert!(manager.applied(&changed).is_none());
    changed.spec.revision_digest = "revision".into();
    changed.metadata.uid = Some("recreated-cell".into());
    assert!(manager.applied(&changed).is_none());
}

#[tokio::test]
async fn stable_results_coalesce_but_expiry_and_resource_changes_notify() {
    let (cell, resources) = fixture();
    let plans = Arc::new(
        proofstorm_kube::compile_component_plans(
            KEY,
            "revision",
            &cell.revision.cell,
            &cell.revision.lock,
        )
        .unwrap(),
    );
    let (manager, _receive) = Manager::new(no_api(), IMAGE.into());
    manager.register(&cell, plans).unwrap();
    let mut state = manager.state.lock().unwrap();
    populate(&mut state, resources);
    state.refresh(IMAGE, 0);
    let job = state.scheduler.dispatch(0).unwrap();
    state.scheduler.complete(&job, Some(response(&job)), 1);
    assert_eq!(state.notifications(500).len(), 1);
    state.notify.insert(KEY.into());
    assert!(
        state.notifications(1000).is_empty(),
        "unchanged outcome waits for heartbeat"
    );
    assert_eq!(state.notifications(10_500).len(), 1);
    state.changed(
        "services",
        CacheChange::Namespace(proofstorm_kube::instance_namespace(KEY)),
    );
    state.refresh(IMAGE, 10_600);
    assert!(
        state.notifications(10_600).is_empty(),
        "resource bursts are coalesced"
    );
    assert_eq!(state.notifications(11_000).len(), 1);
    assert_eq!(state.notifications(30_000).len(), 1);
    assert!(
        state.cells[KEY].outcomes.is_empty(),
        "expiry removes success"
    );
}

#[tokio::test]
async fn target_changes_wait_for_the_retarget_interval_but_worker_changes_do_not() {
    let (cell, resources) = fixture();
    let plans = Arc::new(
        proofstorm_kube::compile_component_plans(
            KEY,
            "revision",
            &cell.revision.cell,
            &cell.revision.lock,
        )
        .unwrap(),
    );
    let (manager, _receive) = Manager::new(no_api(), IMAGE.into());
    manager.register(&cell, plans).unwrap();
    let mut state = manager.state.lock().unwrap();
    populate(&mut state, resources);
    state.refresh(IMAGE, 0);
    let first = state.scheduler.dispatch(0).expect("ready target scheduled");
    assert!(state.scheduler.complete(&first, Some(response(&first)), 1));
    let namespace = proofstorm_kube::instance_namespace(KEY);
    let set_endpoint_ready = |state: &mut State, ready: bool| {
        state
            .endpoints
            .get_mut(&namespace)
            .unwrap()
            .get_mut("chain-endpoints")
            .unwrap()
            .endpoints[0]
            .conditions
            .as_mut()
            .unwrap()
            .ready = Some(ready);
        state.changed("endpoints", CacheChange::Namespace(namespace.clone()));
    };

    set_endpoint_ready(&mut state, false);
    state.refresh(IMAGE, 500);
    assert!(state.retarget.contains(KEY), "the recompute waits");
    assert_eq!(state.scheduler.observations(&first.identity, 500).len(), 1);
    state.refresh(IMAGE, RETARGET_MILLIS);
    assert!(state.retarget.is_empty());
    assert!(
        state
            .scheduler
            .observations(&first.identity, RETARGET_MILLIS)
            .is_empty(),
        "the unready target is dropped"
    );

    set_endpoint_ready(&mut state, true);
    state.refresh(IMAGE, RETARGET_MILLIS + 100);
    assert!(state.retarget.contains(KEY));
    state
        .pods
        .get_mut(&namespace)
        .unwrap()
        .get_mut("worker")
        .unwrap()
        .status
        .as_mut()
        .unwrap()
        .container_statuses
        .as_mut()
        .unwrap()[0]
        .restart_count = 1;
    state.changed("pods", CacheChange::Namespace(namespace.clone()));
    state.refresh(IMAGE, RETARGET_MILLIS + 200);
    assert!(
        state.retarget.is_empty(),
        "a restarted worker applies at once"
    );
    let current = state
        .scheduler
        .dispatch(RETARGET_MILLIS + 200)
        .expect("target registered with the restarted worker");
    assert_ne!(current.identity, first.identity);
}

#[test]
fn observation_wall_time_follows_the_current_wall_clock() {
    let ttl = i64::try_from(OBSERVATION_TTL_MILLIS / 1000).unwrap();
    // Checked 1.2s ago in monotonic time: stamped at least that old, never in the future.
    assert_eq!(observed_at_unix(1_000_000, 50_000, 48_800), 999_998);
    assert_eq!(observed_at_unix(1_000_000, 50_000, 50_000), 1_000_000);
    // After a host sleep the wall clock jumps an hour while the monotonic clock does not.
    // A fresh check must still read as fresh against the wall clock readiness uses.
    let (now_unix, now_millis) = (1_000_000 + 3_600, 50_000 + 1_000);
    let observed = observed_at_unix(now_unix, now_millis, now_millis - 500);
    assert!(
        ProtocolObservation {
            observed_at_unix: observed,
            expires_at_unix: observed + ttl,
            elapsed_micros: 0,
        }
        .is_fresh(now_unix)
    );
}

#[tokio::test]
async fn admission_reads_its_components_live_so_a_newer_heartbeat_cannot_vouch_for_a_stale_cache() {
    use proofstorm_core::{ComponentKind, ComponentStatus, ProtocolObservation};
    let (mut cell, _) = fixture();
    let status = |ready: bool, observed: Option<i64>| ComponentStatus {
        id: "chain".into(),
        kind: ComponentKind::Bitcoin,
        observed_revision_digest: "revision".into(),
        observed_rollout_digest: "rollout".into(),
        conditions: Vec::new(),
        ready,
        protocol_observation: observed.map(|at| ProtocolObservation {
            observed_at_unix: at,
            expires_at_unix: at + 30,
            elapsed_micros: 5,
        }),
        service: "chain.svc".into(),
        ports: BTreeMap::new(),
    };
    // The cached object predates a failed check; the API already has its result,
    // and the cached cell's newer heartbeat covers only components still fresh.
    let cached = proofstorm_kube::component_object(KEY, "system", &status(true, Some(100)), None);
    let live = proofstorm_kube::component_object(KEY, "system", &status(false, None), None);
    cell.resource.status = Some(proofstorm_kube::ProofstormCellStatus {
        protocol_freshness: Some(proofstorm_kube::ProtocolFreshness {
            observed_at_unix: 1_000,
            expires_at_unix: 1_030,
        }),
        ..Default::default()
    });
    let client = Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let live = live.clone();
            async move {
                assert_eq!(request.method(), http::Method::GET);
                assert!(
                    request
                        .uri()
                        .path()
                        .ends_with(&format!("/proofstormcomponents/{KEY}-chain"))
                );
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .header("content-type", "application/json")
                        .body(kube::client::Body::from(serde_json::to_vec(&live).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "system",
    );
    let (manager, _receive) = Manager::new(client.clone(), IMAGE.into());
    let (store, mut writer) = kube::runtime::reflector::store();
    writer.apply_watcher_event(&kube::runtime::watcher::Event::InitDone);
    writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(cached));
    let context = crate::Context {
        components: store,
        ..crate::Context::for_tests(client, manager)
    };
    let chain =
        |cell: &proofstorm_kube::LoadedCell| cell.status.as_ref().unwrap().components[0].clone();
    // From the cache alone the heartbeat would vouch for the withdrawn evidence.
    let cached = crate::with_component_statuses(&context, cell.clone(), &BTreeSet::new())
        .await
        .unwrap();
    assert!(chain(&cached).ready);
    assert_eq!(
        chain(&cached)
            .protocol_observation
            .map(|o| o.expires_at_unix),
        Some(1_030)
    );
    let live = crate::with_component_statuses(&context, cell, &BTreeSet::from(["chain".into()]))
        .await
        .unwrap();
    assert!(!chain(&live).ready);
    assert!(chain(&live).protocol_observation.is_none());
}
