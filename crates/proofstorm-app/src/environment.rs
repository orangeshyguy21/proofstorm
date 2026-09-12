//! A credential-free read model shared by CLI, MCP and HTTP.
mod prober;
mod resources;
use crate::{Error, cell::Cells};
use futures::{StreamExt, stream};
use kube::{Api, ResourceExt};
use proofstorm_core::Capability;
use proofstorm_kube::ProofstormCell;
use proofstorm_store::{EnvironmentEntry, StoreError};
pub use resources::{Endpoint, ResourceDemand};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use proofstorm_view::*;

impl Cells {
    /// Only reads local history and existing runtime status. Never creates a session or job.
    pub async fn environment(&self, query: &EnvironmentQuery) -> Result<EnvironmentView, Error> {
        query
            .validate()
            .map_err(|message| Error::problem("invalid_page", message))?;
        for cap in [
            Capability::CellRead,
            Capability::CellStatus,
            Capability::ExperimentRead,
        ] {
            self.store
                .authorize(&self.workspace, &self.principal, cap)?;
        }
        let started = now();
        let live = self.runtime.current_instance_ids(&self.workspace).await?;
        let (entries, next_cursor) = if let Some(id) = &query.instance_id {
            if !live.contains(id) {
                return Err(Error::missing(
                    "cell is not present in the current cluster",
                    Some(serde_json::json!({"code":"cell_not_in_cluster"})),
                ));
            }
            (
                vec![
                    self.store
                        .environment_entry(&self.workspace, &self.principal, id)?,
                ],
                None,
            )
        } else {
            let mut entries = Vec::new();
            for id in live.iter().filter(|id| *id > &query.cursor) {
                match self
                    .store
                    .environment_entry(&self.workspace, &self.principal, id)
                {
                    Ok(entry) => entries.push(entry),
                    Err(StoreError::NotFound { .. }) => continue,
                    Err(error) => return Err(error.into()),
                }
                if entries.len() > query.limit as usize {
                    break;
                }
            }
            let next = (entries.len() > query.limit as usize)
                .then(|| entries[query.limit as usize - 1].id.clone());
            entries.truncate(query.limit as usize);
            (entries, next)
        };
        // Decode only records belonging to cells still present in the selected cluster.
        let cells = stream::iter(entries)
            .map(|entry| async {
                let id = entry.id.clone();
                let handle = entry.handle.clone();
                match self.environment_cell(entry, query).await {
                    Err(error)
                        if error.details.as_ref().is_some_and(|details| {
                            details["code"] == "stored_record_incompatible"
                        }) =>
                    {
                        Ok(unreadable_cell(id, handle))
                    }
                    result => result,
                }
            })
            .buffered(8)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let mut view = EnvironmentView {
            api_version:"proofstorm/environment/v1alpha1".into(),workspace_id:self.workspace.clone(),
            scope:"cells present in the selected cluster and tracked in this database and workspace; deleted and unmaterialized cells are excluded".into(),
            observation_started_at_unix:started,observation_finished_at_unix:now(),cells:Page {items:cells,next_cursor},
            coverage:Coverage {
                topology:"declared links, not measured reachability or payment flows".into(),
                activity:"recorded managed operations; pending/running outcomes may require explicit sync; no receipts are collected by this read".into(),
                resource_demand:"rendered desired requests/limits with namespace defaults; excludes transient action jobs. replicas is desired scale, not a running pod count. The protocol prober is a controller-scheduled Deployment: its scale and observation come from a bounded live read, or are null when unavailable. Compare observation generation with observed_generation before treating status as current.".into(),
                resource_usage:"not collected".into(),protocol_traffic:"not collected".into(),attached_clients:"not tracked; advertised endpoints do not imply active tunnels or clients".into(),
            },
        };
        bound_page_bytes(&mut view, 24 * 1024)?;
        Ok(view)
    }

    async fn environment_cell(
        &self,
        entry: EnvironmentEntry,
        query: &EnvironmentQuery,
    ) -> Result<EnvironmentCell, Error> {
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
        let ((runtime, resource), prober) = if let Some(instance) = &instance {
            tokio::join!(
                self.observe_environment_runtime(instance),
                prober::observe(self.runtime.client.clone(), &instance.instance_key)
            )
        } else {
            (
                (empty_runtime(ObservationState::NotMaterialized, None), None),
                None,
            )
        };
        let (mut resources, resource_error, endpoints) =
            if let (Some(instance), Some(revision)) = (&instance, &revision) {
                match resources::project(instance, revision) {
                    Ok((r, e)) => (Some(r), None, e),
                    Err(_) => (None, Some("render_unavailable".into()), Vec::new()),
                }
            } else {
                (None, None, Vec::new())
            };
        resources::include_runtime(&mut resources, resource.as_ref(), prober);
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
                        .is_some_and(|r| r.cell.components.iter().any(|c| &c.id == id))
                });
                activity
            })
            .collect();
        Ok(EnvironmentCell {
            layout_id: instance.as_ref().map(layout_identity),
            desired_generation: instance.as_ref().map(|i| i.generation),
            last_converged_revision: resource
                .as_ref()
                .and_then(|r| r.status.as_ref())
                .and_then(|s| s.last_converged_revision.clone()),
            id: entry.id,
            handle: entry.handle,
            read_error: None,
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
        instance: &proofstorm_core::CellInstance,
    ) -> (RuntimeObservation, Option<ProofstormCell>) {
        let cells = Api::<ProofstormCell>::namespaced(
            self.runtime.client.clone(),
            &self.runtime.control_namespace,
        );
        let resource = match tokio::time::timeout(
            Duration::from_secs(3),
            cells.get_opt(&instance.resource_name),
        )
        .await
        {
            Ok(Ok(Some(r))) => r,
            Ok(Ok(None)) => {
                return (empty_runtime(ObservationState::Missing, None), None);
            }
            Ok(Err(_)) => {
                return (
                    empty_runtime(ObservationState::Unavailable, Some("runtime_read_failed")),
                    None,
                );
            }
            Err(_) => {
                return (
                    empty_runtime(ObservationState::Unavailable, Some("runtime_read_timeout")),
                    None,
                );
            }
        };
        if resource.spec.workspace_id != instance.workspace_id
            || resource.spec.instance_id != instance.id
            || resource.spec.instance_key != instance.instance_key
        {
            return (
                empty_runtime(
                    ObservationState::Unavailable,
                    Some("runtime_identity_mismatch"),
                ),
                None,
            );
        }
        let status = resource.status.as_ref();
        let current = status.is_some_and(|s| {
            s.observed_desired_generation == instance.generation
                && s.observed_revision_digest == instance.revision_digest
                && resource.metadata.generation.is_some()
                && s.observed_generation == resource.metadata.generation
        });
        let phase =
            status.map(|_| crate::runtime::status_from_resource(instance.clone(), &resource).phase);
        let observation = RuntimeObservation {
            message: status.and_then(|s| s.message.clone()),
            observed_desired_generation: status.map(|s| s.observed_desired_generation),
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
fn unreadable_cell(id: String, handle: Option<proofstorm_store::CellHandle>) -> EnvironmentCell {
    EnvironmentCell {
        layout_id: None,
        desired_generation: None,
        last_converged_revision: None,
        id,
        handle,
        read_error: Some("stored_record_incompatible".into()),
        revision_digest: None,
        journal_read_at_unix: now(),
        last_recorded_activity_at_unix: None,
        runtime: empty_runtime(ObservationState::Unavailable, None),
        components: Page {
            items: vec![],
            next_cursor: None,
        },
        links: Page {
            items: vec![],
            next_cursor: None,
        },
        resources: None,
        resource_error: None,
        sessions: Page {
            items: vec![],
            next_cursor: None,
        },
        activity: Page {
            items: vec![],
            next_cursor: None,
        },
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
        let probes = components
            .items
            .iter()
            .map(|c| proofstorm_kube::protocol_probe_container_name(&c.id))
            .collect::<std::collections::BTreeSet<_>>();
        for workload in &mut resources.workloads {
            if workload.name == proofstorm_kube::PROTOCOL_PROBER_NAME {
                let previous_count = workload.containers.len();
                workload
                    .containers
                    .retain(|container| probes.contains(&container.name));
                workload.omitted_container_count += previous_count - workload.containers.len();
            }
        }
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
/// Bound a page with explicit continuation, preserving the shared read model.
/// Transports that serialize the page twice can reserve a smaller payload budget.
///
/// # Errors
/// Returns an error if even a single item cannot fit; never substitutes an empty page.
pub fn bound_page_bytes(view: &mut EnvironmentView, maximum_bytes: usize) -> Result<(), Error> {
    while serde_json::to_vec(&view)
        .map_err(|_| Error::failure("environment serialization failed", None))?
        .len()
        > maximum_bytes
    {
        if shorten(&mut view.cells, |cell| &cell.id) {
            continue;
        }
        let Some(cell) = view.cells.items.first_mut() else {
            break;
        };
        let changed = shorten(&mut cell.activity, |op| &op.id)
            | shorten(&mut cell.sessions, |s| &s.session.id)
            | shorten(&mut cell.links, |l| &l.id)
            | shorten(&mut cell.components, |c| &c.id);
        filter_resources(&mut cell.resources, &cell.components);
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
    resource: Option<&ProofstormCell>,
    current: bool,
    endpoints: &[Endpoint],
) -> (Vec<ComponentView>, Vec<LinkView>) {
    let components: Vec<ComponentView> =
        revision
            .map(|r| {
                r.cell
                    .components
                    .iter()
                    .map(|c| {
                        let status =
                            resource
                                .as_ref()
                                .and_then(|r| r.status.as_ref())
                                .and_then(|s| {
                                    s.components.iter().find(|s| {
                                        s.id == c.id
                                            && r.lock.entries.iter().any(|e| {
                                                e.component_id == c.id
                                                    && e.rollout_digest == s.observed_rollout_digest
                                            })
                                    })
                                });
                        ComponentView {
                            details: r.lock.entries.iter().find(|e| e.component_id == c.id).map(
                                |entry| component_details(entry, status.is_some_and(|s| s.ready)),
                            ),
                            id: c.id.clone(),
                            kind: c.kind,
                            implementation: c.implementation.clone(),
                            version: c.version.clone(),
                            ready: status
                                .filter(|s| {
                                    current
                                        || r.lock.entries.iter().any(|e| {
                                            e.component_id == c.id
                                                && e.rollout_digest == s.observed_rollout_digest
                                        })
                                })
                                .map(|s| s.ready),
                            conditions: status
                                .map(|s| {
                                    s.conditions
                                        .iter()
                                        .map(|c| ConditionView {
                                            message: c.message.clone(),
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
            r.cell
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

fn empty_runtime(state: ObservationState, error: Option<&str>) -> RuntimeObservation {
    RuntimeObservation {
        message: None,
        observed_desired_generation: None,
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

fn component_details(entry: &proofstorm_core::LockEntry, observed: bool) -> ComponentDetails {
    let embedded = proofstorm_core::default_catalog()
        .entries
        .iter()
        .find(|catalog| catalog.id == entry.catalog_id)
        .map(|catalog| {
            catalog
                .runtime_endpoints
                .iter()
                .filter(|endpoint| endpoint.id != "component")
                .map(|endpoint| {
                    let (name, kind) = match endpoint.id.as_str() {
                        "ldk-node" => (
                            "LDK Node".to_owned(),
                            proofstorm_core::ComponentKind::Lightning,
                        ),
                        "bdk" => (
                            "BDK wallet".to_owned(),
                            proofstorm_core::ComponentKind::Wallet,
                        ),
                        _ => (endpoint.id.clone(), proofstorm_core::ComponentKind::Proxy),
                    };
                    EmbeddedResourceView {
                        id: endpoint.id.clone(),
                        name,
                        kind,
                        version: None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ComponentDetails {
        resolved_version: entry.version.clone(),
        observed_version: observed.then(|| entry.version.clone()),
        image: entry.image.clone(),
        adapter_version: entry.adapter_version.clone(),
        source_commit: entry
            .source
            .as_ref()
            .map(|s| s.commit_sha.clone())
            .or_else(|| {
                entry
                    .build_provenance
                    .as_ref()
                    .map(|p| p.commit_sha.clone())
            }),
        embedded,
    }
}

fn layout_identity(instance: &proofstorm_core::CellInstance) -> String {
    format!("{}:{}", instance.workspace_id, instance.instance_key)
}

#[cfg(test)]
mod canvas_tests {
    use super::*;
    use proofstorm_core::{ComponentKind, LockEntry};

    fn lock(catalog_id: &str) -> LockEntry {
        serde_json::from_value(serde_json::json!({
            "component_id":"mint", "catalog_id":catalog_id,
            "adapter_version":"adapter-1", "version":"0.18.0",
            "config_version":"test/v1", "config_schema_digest":"schema",
            "features":[], "compatible_dependencies":[],
            "effective_config_digest":"config", "rollout_digest":"rollout",
            "image":"mint@sha256:123", "source_digest":"source"
        }))
        .unwrap()
    }

    #[test]
    fn embedded_versions_never_inherit_parent_version() {
        for (catalog, id, kind) in [
            ("cdk-ldk", "ldk-node", ComponentKind::Lightning),
            ("cdk-bdk", "bdk", ComponentKind::Wallet),
        ] {
            let details = component_details(&lock(catalog), true);
            assert_eq!(details.resolved_version, "0.18.0");
            assert_eq!(details.observed_version.as_deref(), Some("0.18.0"));
            assert_eq!(details.embedded.len(), 1);
            assert_eq!(details.embedded[0].id, id);
            assert_eq!(details.embedded[0].kind, kind);
            assert_eq!(details.embedded[0].version, None);
        }
        assert_eq!(
            component_details(&lock("cdk-bdk"), false).observed_version,
            None
        );
        assert!(component_details(&lock("cdk"), true).embedded.is_empty());
    }

    #[test]
    fn identity_providers_are_projected_without_runtime_observations() {
        let revision = serde_json::from_value(serde_json::json!({
            "workspace_id":"test", "digest":"revision",
            "cell":{"api_version":"proofstorm/v1alpha1", "name":"test", "links":[],
                "components":[{"id":"identity", "kind":"identity_provider", "implementation":"keycloak", "config_version":"test/v1", "control":"cell", "config":{}}]},
            "lock":{"api_version":"proofstorm/lock/v2alpha1", "digest":"lock", "entries":[]}
        })).unwrap();
        let (components, _) = topology(Some(&revision), None, false, &[]);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].kind, ComponentKind::IdentityProvider);
        assert_eq!(components[0].ready, None);
    }
}
