//! A credential-free read model shared by CLI, MCP and HTTP.
mod resources;
use crate::{
    Error,
    lab::{Activity, Labs},
};
use futures::{StreamExt, stream};
use kube::{Api, ResourceExt};
use proofstorm_core::{
    Capability, ComponentConditionReason, ComponentConditionState, ComponentConditionType,
    ComponentKind, InstancePhase, LinkKind, Session,
};
use proofstorm_kube::ProofstormLab;
use proofstorm_store::{EnvironmentEntry, LabHandle, StoreError};
pub use resources::{Endpoint, ResourceDemand};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentQuery {
    pub instance_id: Option<String>,
    pub cursor: String,
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    pub session_cursor: String,
    pub activity_cursor: String,
    pub component_cursor: String,
    pub link_cursor: String,
}
impl Default for EnvironmentQuery {
    fn default() -> Self {
        Self {
            instance_id: None,
            cursor: String::new(),
            limit: 20,
            session_cursor: String::new(),
            activity_cursor: String::new(),
            component_cursor: String::new(),
            link_cursor: String::new(),
        }
    }
}
impl EnvironmentQuery {
    pub fn validate(&self) -> Result<(), Error> {
        if !(1..=50).contains(&self.limit)
            || [
                &self.cursor,
                &self.session_cursor,
                &self.activity_cursor,
                &self.component_cursor,
                &self.link_cursor,
            ]
            .iter()
            .any(|s| s.len() > 128)
            || self
                .instance_id
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 128)
        {
            return Err(Error::problem(
                "invalid_page",
                "limit must be 1..=50; IDs and cursors must be at most 128 bytes",
            ));
        }
        if (self.instance_id.is_none()
            && (!self.session_cursor.is_empty()
                || !self.activity_cursor.is_empty()
                || !self.component_cursor.is_empty()
                || !self.link_cursor.is_empty()))
            || (self.instance_id.is_some() && !self.cursor.is_empty())
        {
            return Err(Error::problem(
                "invalid_page",
                "section cursors require instance_id; the lab cursor requires an environment page",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct EnvironmentView {
    pub api_version: String,
    pub workspace_id: String,
    pub scope: String,
    pub observation_started_at_unix: i64,
    pub observation_finished_at_unix: i64,
    pub labs: Page<EnvironmentLab>,
    pub coverage: Coverage,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct Coverage {
    pub topology: String,
    pub activity: String,
    pub resource_demand: String,
    pub resource_usage: String,
    pub protocol_traffic: String,
    pub attached_clients: String,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct EnvironmentLab {
    pub id: String,
    pub handle: Option<LabHandle>,
    pub revision_digest: Option<String>,
    pub journal_read_at_unix: i64,
    pub last_recorded_activity_at_unix: Option<i64>,
    pub runtime: RuntimeObservation,
    pub components: Page<ComponentView>,
    pub links: Page<LinkView>,
    pub resources: Option<ResourceDemand>,
    pub resource_error: Option<String>,
    pub sessions: Page<SessionView>,
    pub activity: Page<Activity>,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    Available,
    Stale,
    Missing,
    Unavailable,
    NotMaterialized,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct RuntimeObservation {
    pub state: ObservationState,
    pub fetched_at_unix: i64,
    pub source_updated_at_unix: Option<i64>,
    pub resource_version: Option<String>,
    pub generation: Option<i64>,
    pub observed_generation: Option<i64>,
    pub phase: Option<InstancePhase>,
    pub error: Option<String>,
}
impl RuntimeObservation {
    fn empty(state: ObservationState, error: Option<&str>) -> Self {
        Self {
            state,
            fetched_at_unix: now(),
            source_updated_at_unix: None,
            resource_version: None,
            generation: None,
            observed_generation: None,
            phase: None,
            error: error.map(str::to_owned),
        }
    }
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ComponentView {
    pub id: String,
    pub kind: ComponentKind,
    pub implementation: String,
    pub version: Option<String>,
    pub ready: Option<bool>,
    pub conditions: Vec<ConditionView>,
    pub endpoints: Vec<Endpoint>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConditionView {
    pub condition_type: ComponentConditionType,
    pub state: ComponentConditionState,
    pub reason: ComponentConditionReason,
    pub last_transition_unix: i64,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct LinkView {
    pub id: String,
    pub kind: LinkKind,
    pub from: String,
    pub to: String,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct SessionView {
    pub session: Session,
    pub overlapping_session_count: i64,
}

impl Labs {
    /// Only reads local history and existing runtime status. Never creates a session or job.
    pub async fn environment(&self, query: &EnvironmentQuery) -> Result<EnvironmentView, Error> {
        query.validate()?;
        for cap in [
            Capability::LabRead,
            Capability::LabStatus,
            Capability::ExperimentRead,
        ] {
            self.store
                .authorize(&self.workspace, &self.principal, cap)?;
        }
        let started = now();
        let (entries, next_cursor) = if let Some(id) = &query.instance_id {
            (
                vec![
                    self.store
                        .environment_entry(&self.workspace, &self.principal, id)?,
                ],
                None,
            )
        } else {
            self.store.environment_entries(
                &self.workspace,
                &self.principal,
                &query.cursor,
                query.limit,
            )?
        };
        // Bounded concurrency and per-lab timeouts keep an unavailable cluster from hiding history.
        let labs = stream::iter(entries)
            .map(|entry| self.environment_lab(entry, query))
            .buffered(8)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let mut view = EnvironmentView {
            api_version:"proofstorm/environment/v1alpha1".into(),workspace_id:self.workspace.clone(),
            scope:"instances retained in this database and workspace, including historical generations; runtime-only labs in other databases are outside this inventory".into(),
            observation_started_at_unix:started,observation_finished_at_unix:now(),labs:Page {items:labs,next_cursor},
            coverage:Coverage {
                topology:"declared links, not measured reachability or payment flows".into(),
                activity:"recorded managed operations; pending/running outcomes may require explicit sync; no receipts are collected by this read".into(),
                resource_demand:"rendered desired requests/limits with namespace defaults for this component page and shared workloads; excludes transient action jobs".into(),
                resource_usage:"not collected".into(),protocol_traffic:"not collected".into(),attached_clients:"not tracked; advertised endpoints do not imply active tunnels or clients".into(),
            },
        };
        bound_page(&mut view)?;
        Ok(view)
    }

    async fn environment_lab(
        &self,
        entry: EnvironmentEntry,
        query: &EnvironmentQuery,
    ) -> Result<EnvironmentLab, Error> {
        let instance = match self
            .store
            .instance(&self.workspace, &self.principal, &entry.id)
        {
            Ok(i) => Some(i),
            Err(StoreError::NotFound { .. }) => None,
            Err(e) => return Err(e.into()),
        };
        let revision = instance
            .as_ref()
            .map(|i| {
                self.store
                    .revision(&self.workspace, &self.principal, &i.revision_digest)
            })
            .transpose()?;
        let (runtime, resource) = if let Some(instance) = &instance {
            self.observe_environment_runtime(instance).await
        } else {
            (
                RuntimeObservation::empty(ObservationState::NotMaterialized, None),
                None,
            )
        };
        let (resources, resource_error, endpoints) =
            if let (Some(instance), Some(revision)) = (&instance, &revision) {
                match resources::project(instance, revision) {
                    Ok((r, e)) => (Some(r), None, e),
                    Err(_) => (None, Some("render_unavailable".into()), Vec::new()),
                }
            } else {
                (None, None, Vec::new())
            };
        let (components, links) = topology(
            revision.as_ref(),
            resource.as_ref(),
            matches!(runtime.state, ObservationState::Available),
            &endpoints,
        );
        let page_limit = if query.instance_id.is_some() {
            query.limit
        } else {
            20
        };
        let components = section_page(components, &query.component_cursor, page_limit, |c| &c.id);
        let links = section_page(links, &query.link_cursor, page_limit, |l| &l.id);
        let mut resources = resources;
        filter_resources(&mut resources, &components);
        let sessions = self.environment_sessions(&entry.id, &query.session_cursor, page_limit)?;
        let (ops, next_cursor) = self.store.instance_activity(
            &self.workspace,
            &self.principal,
            &entry.id,
            &query.activity_cursor,
            page_limit,
        )?;
        let last_activity =
            self.store
                .last_instance_activity(&self.workspace, &self.principal, &entry.id)?;
        let activity = ops
            .into_iter()
            .map(|op| {
                let mut activity = Activity::from(op);
                activity.components.retain(|id| {
                    revision
                        .as_ref()
                        .is_some_and(|r| r.lab.components.iter().any(|c| &c.id == id))
                });
                activity
            })
            .collect();
        Ok(EnvironmentLab {
            id: entry.id,
            handle: entry.handle,
            revision_digest: revision.map(|r| r.digest),
            journal_read_at_unix: now(),
            last_recorded_activity_at_unix: last_activity,
            runtime,
            components,
            links,
            resources,
            resource_error,
            sessions,
            activity: Page {
                items: activity,
                next_cursor,
            },
        })
    }

    fn environment_sessions(
        &self,
        instance: &str,
        cursor: &str,
        limit: u32,
    ) -> Result<Page<SessionView>, Error> {
        let sessions =
            self.store
                .sessions(&self.workspace, &self.principal, instance, cursor, limit)?;
        let session_items = sessions
            .sessions
            .into_iter()
            .map(|session| {
                Ok(SessionView {
                    overlapping_session_count: self.store.session_overlap_count(
                        &self.workspace,
                        &self.principal,
                        &session.id,
                        sessions.observed_at_unix,
                    )?,
                    session,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(Page {
            items: session_items,
            next_cursor: sessions.next_cursor,
        })
    }

    async fn observe_environment_runtime(
        &self,
        instance: &proofstorm_core::LabInstance,
    ) -> (RuntimeObservation, Option<ProofstormLab>) {
        let labs = Api::<ProofstormLab>::namespaced(
            self.runtime.client.clone(),
            &self.runtime.control_namespace,
        );
        let resource = match tokio::time::timeout(
            Duration::from_secs(3),
            labs.get_opt(&instance.resource_name),
        )
        .await
        {
            Ok(Ok(Some(r))) => r,
            Ok(Ok(None)) => {
                return (
                    RuntimeObservation::empty(ObservationState::Missing, None),
                    None,
                );
            }
            Ok(Err(_)) => {
                return (
                    RuntimeObservation::empty(
                        ObservationState::Unavailable,
                        Some("runtime_read_failed"),
                    ),
                    None,
                );
            }
            Err(_) => {
                return (
                    RuntimeObservation::empty(
                        ObservationState::Unavailable,
                        Some("runtime_read_timeout"),
                    ),
                    None,
                );
            }
        };
        if resource.spec.workspace_id != instance.workspace_id
            || resource.spec.instance_id != instance.id
            || resource.spec.instance_key != instance.instance_key
            || resource.spec.revision_digest != instance.revision_digest
            || resource.spec.lock.digest != instance.lock_digest
        {
            return (
                RuntimeObservation::empty(
                    ObservationState::Unavailable,
                    Some("runtime_identity_mismatch"),
                ),
                None,
            );
        }
        let status = resource.status.as_ref();
        let current = status.is_some_and(|s| {
            s.observed_revision_digest == instance.revision_digest
                && resource.metadata.generation.is_some()
                && s.observed_generation == resource.metadata.generation
        });
        let phase =
            status.map(|_| crate::runtime::status_from_resource(instance.clone(), &resource).phase);
        let observation = RuntimeObservation {
            state: if current {
                ObservationState::Available
            } else {
                ObservationState::Stale
            },
            fetched_at_unix: now(),
            source_updated_at_unix: resource
                .metadata
                .managed_fields
                .as_ref()
                .into_iter()
                .flatten()
                .filter(|f| f.subresource.as_deref() == Some("status"))
                .filter_map(|f| f.time.as_ref().map(|t| t.0.as_second()))
                .max(),
            resource_version: resource.resource_version(),
            generation: resource.metadata.generation,
            observed_generation: status.and_then(|s| s.observed_generation),
            phase,
            error: None,
        };
        (observation, Some(resource))
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn section_page<T>(
    mut items: Vec<T>,
    cursor: &str,
    limit: u32,
    id: impl Fn(&T) -> &str,
) -> Page<T> {
    items.sort_by(|a, b| id(a).cmp(id(b)));
    items.retain(|item| id(item) > cursor);
    let next_cursor =
        (items.len() > limit as usize).then(|| id(&items[limit as usize - 1]).to_owned());
    items.truncate(limit as usize);
    Page { items, next_cursor }
}
fn filter_resources(resources: &mut Option<ResourceDemand>, components: &Page<ComponentView>) {
    if let Some(resources) = resources {
        let keep = |id: &Option<String>| {
            id.as_ref()
                .is_none_or(|id| components.items.iter().any(|c| &c.id == id))
        };
        resources.workloads.retain(|w| keep(&w.component));
        resources.storage.retain(|s| keep(&s.component));
    }
}
fn shorten<T>(page: &mut Page<T>, id: impl Fn(&T) -> &str) -> bool {
    if page.items.len() < 2 {
        return false;
    }
    page.items.pop();
    page.next_cursor = page.items.last().map(|item| id(item).to_owned());
    true
}
/// Bound the common payload below MCP's envelope budget, with explicit continuation.
fn bound_page(view: &mut EnvironmentView) -> Result<(), Error> {
    while serde_json::to_vec(&view)
        .map_err(|_| Error::failure("environment serialization failed", None))?
        .len()
        > 24 * 1024
    {
        if shorten(&mut view.labs, |lab| &lab.id) {
            continue;
        }
        let Some(lab) = view.labs.items.first_mut() else {
            break;
        };
        let changed = shorten(&mut lab.activity, |op| &op.id)
            | shorten(&mut lab.sessions, |s| &s.session.id)
            | shorten(&mut lab.links, |l| &l.id)
            | shorten(&mut lab.components, |c| &c.id);
        filter_resources(&mut lab.resources, &lab.components);
        if !changed {
            return Err(Error::problem(
                "environment_item_too_large",
                "a single environment item exceeds the response limit",
            ));
        }
    }
    Ok(())
}

fn topology(
    revision: Option<&proofstorm_core::PublishedRevision>,
    resource: Option<&ProofstormLab>,
    current: bool,
    endpoints: &[Endpoint],
) -> (Vec<ComponentView>, Vec<LinkView>) {
    let components: Vec<ComponentView> = revision
        .map(|r| {
            r.lab
                .components
                .iter()
                .map(|c| {
                    let status = resource
                        .as_ref()
                        .and_then(|r| r.status.as_ref())
                        .and_then(|s| {
                            s.components
                                .iter()
                                .find(|s| s.id == c.id && s.observed_revision_digest == r.digest)
                        });
                    ComponentView {
                        id: c.id.clone(),
                        kind: c.kind,
                        implementation: c.implementation.clone(),
                        version: c.version.clone(),
                        ready: status.filter(|_| current).map(|s| s.ready),
                        conditions: status
                            .map(|s| {
                                s.conditions
                                    .iter()
                                    .map(|c| ConditionView {
                                        condition_type: c.condition_type,
                                        state: c.state,
                                        reason: c.reason,
                                        last_transition_unix: c.last_transition_unix,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        endpoints: endpoints
                            .iter()
                            .filter(|e| e.component == c.id)
                            .cloned()
                            .collect(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let links: Vec<LinkView> = revision
        .map(|r| {
            r.lab
                .links
                .iter()
                .map(|l| LinkView {
                    id: l.id.clone(),
                    kind: l.kind,
                    from: l.from.clone(),
                    to: l.to.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    (components, links)
}
