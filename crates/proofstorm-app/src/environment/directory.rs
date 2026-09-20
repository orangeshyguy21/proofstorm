//! Selective directory reads shared by MCP and HTTP; the GUI keeps its full view.
use super::{
    Activity, Capability, Cells, EnvironmentEntry, Error, ObservationState, Page, ProofstormCell,
    StoreError, bound_page_bytes, component_views, link_views, now, observe_resource, prober,
    resources, section_page,
};
use proofstorm_core::{CellInstance, digest_json};
use serde::Serialize;
use serde_json::{Value, json};

mod query;
pub use query::EnvironmentReadQuery;

struct Candidate {
    entry: EnvironmentEntry,
    instance: CellInstance,
    resource: Option<ProofstormCell>,
    header: Value,
}

impl Cells {
    /// Shared transport read. Default queries preserve the complete GUI shape;
    /// scans and selectors load only the sections needed for their projection.
    pub async fn environment_read(
        &self,
        query: &EnvironmentReadQuery,
        maximum_bytes: usize,
    ) -> Result<Value, Error> {
        query.validate()?;
        if !query.selective() {
            let mut view = self.environment(&query.page()).await?;
            bound_page_bytes(&mut view, maximum_bytes)?;
            return serialize(&view);
        }
        for cap in [
            Capability::CellRead,
            Capability::CellStatus,
            Capability::ExperimentRead,
        ] {
            self.store
                .authorize(&self.workspace, &self.principal, cap)?;
        }
        let started = now();
        let (candidates, unavailable) = self.directory_candidates(query).await?;
        self.directory_page(query, maximum_bytes, started, candidates, unavailable)
            .await
    }

    async fn directory_candidates(
        &self,
        query: &EnvironmentReadQuery,
    ) -> Result<(Vec<Candidate>, Vec<String>), Error> {
        let pattern = query.pattern()?;
        let resources = self.runtime.current_instances(&self.workspace).await?;
        let mut candidates = Vec::new();
        let mut unavailable = Vec::new();
        for (id, resource) in resources {
            if query
                .instance_id
                .as_ref()
                .is_some_and(|wanted| wanted != &id)
            {
                continue;
            }
            let entry = match self
                .store
                .environment_entry(&self.workspace, &self.principal, &id)
            {
                Ok(entry) => entry,
                Err(StoreError::NotFound { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
            if query
                .name
                .as_ref()
                .is_some_and(|name| entry.handle.as_ref().is_none_or(|h| &h.name != name))
                || query
                    .owner
                    .as_ref()
                    .is_some_and(|owner| entry.handle.as_ref().is_none_or(|h| &h.owner != owner))
            {
                continue;
            }
            let instance = match self.store.instance(&self.workspace, &self.principal, &id) {
                Ok(instance) => instance,
                Err(StoreError::NotFound { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
            let (runtime, resource) = observe_resource(&instance, resource);
            let ready = resource
                .as_ref()
                .filter(|_| matches!(runtime.state, ObservationState::Available))
                .map(|resource| {
                    crate::runtime::status_from_current_resource(instance.clone(), resource)
                        .components
                        .iter()
                        .filter(|component| component.ready)
                        .count()
                });
            let header = json!({"id":id,"instance_key":instance.instance_key,"name":entry.handle.as_ref().map(|h|&h.name),"owner":entry.handle.as_ref().map(|h|&h.owner),"desired_generation":instance.generation,"revision_digest":instance.revision_digest,
                "runtime":runtime,"ready_components":ready,"total_components":resource.as_ref().map(|r|r.spec.cell.components.len())});
            if query.phase.is_some() && runtime.phase.is_none() {
                unavailable.push(id);
                continue;
            }
            if query
                .phase
                .is_some_and(|phase| runtime.phase != Some(phase))
            {
                continue;
            }
            if !pattern.is_match(&header.to_string()) {
                continue;
            }
            if query.component_kind.is_some() || query.implementation.is_some() {
                let revision = match self.store.revision(
                    &self.workspace,
                    &self.principal,
                    &instance.revision_digest,
                ) {
                    Ok(revision) => revision,
                    Err(StoreError::Serialization(_)) => {
                        unavailable.push(id);
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                if !revision.cell.components.iter().any(|c| {
                    query.component_kind.is_none_or(|kind| kind == c.kind)
                        && query
                            .implementation
                            .as_ref()
                            .is_none_or(|implementation| implementation == &c.implementation)
                }) {
                    continue;
                }
            }
            candidates.push(Candidate {
                entry,
                instance,
                resource,
                header,
            });
        }
        Ok((candidates, unavailable))
    }

    async fn directory_page(
        &self,
        query: &EnvironmentReadQuery,
        maximum_bytes: usize,
        started: i64,
        candidates: Vec<Candidate>,
        unavailable: Vec<String>,
    ) -> Result<Value, Error> {
        if query.instance_id.is_some() && candidates.is_empty() && query.only_instance_selector() {
            return Err(Error::missing(
                "cell is not present and tracked in the selected cluster",
                Some(json!({"code":"cell_not_in_cluster"})),
            ));
        }
        // Bind membership and durable generations; changing live observations
        // remain visible without restarting every page on a controller heartbeat.
        let snapshot = digest_json(&(
            candidates
                .iter()
                .map(|c| {
                    (
                        &c.instance.id,
                        &c.instance.instance_key,
                        c.instance.generation,
                        &c.instance.revision_digest,
                        &c.entry.handle,
                    )
                })
                .collect::<Vec<_>>(),
            &unavailable,
        ));
        let context = query.fingerprint(&self.workspace, &self.principal, &snapshot);
        let boundary = query.boundary("cells", &query.cursor, &context)?;
        let start = candidates.partition_point(|c| c.entry.id.as_str() <= boundary.as_str());
        let mut response = json!({"api_version":"proofstorm/environment/v1alpha1","workspace_id":self.workspace,"scope":"cells present in the selected cluster and tracked in this workspace","observation_started_at_unix":started,"observation_finished_at_unix":now(),"observation_digest":snapshot,
            "matched_count":candidates.len(),"unavailable_count":unavailable.len(),"selection":{"scan":query.scan,"sections":query.sections,"fields":query.fields},
            "cells":{"items":[],"next_cursor":null},
            "coverage":{"runtime":"live observation; phase can be stale, check runtime.state","activity":"recorded only; no synchronization or commands","omitted_sections":"not requested, not empty or unavailable"}});
        let end = (start + query.limit as usize).min(candidates.len());
        for (index, candidate) in candidates.iter().enumerate().take(end).skip(start) {
            let cell = match self.directory_cell(candidate, query, &context).await {
                Ok(cell) => cell,
                Err(error)
                    if error
                        .details
                        .as_ref()
                        .is_some_and(|details| details["code"] == "stored_record_incompatible") =>
                {
                    let mut cell = candidate.header.clone();
                    for section in query.load_sections() {
                        cell[section] = Value::Null;
                    }
                    cell["read_error"] = json!("stored_record_incompatible");
                    cell
                }
                Err(error) => return Err(error),
            };
            response["cells"]["items"]
                .as_array_mut()
                .unwrap()
                .push(cell);
            response["cells"]["next_cursor"] =
                json!((index + 1 < candidates.len()).then(|| query.cursor_for(
                    "cells",
                    &context,
                    &candidate.entry.id
                )));
            if serde_json::to_vec(&project_response(&response, &query.fields))
                .map_err(|error| Error::failure(error.to_string(), None))?
                .len()
                > maximum_bytes
            {
                // Stop hydrating when the first non-fitting cell is found.
                // The bounder preserves that cell as the next unread record.
                break;
            }
        }
        response["observation_finished_at_unix"] = json!(now());
        bound_directory(&mut response, maximum_bytes, query, &context)?;
        Ok(project_response(&response, &query.fields))
    }

    async fn directory_cell(
        &self,
        candidate: &Candidate,
        query: &EnvironmentReadQuery,
        context: &str,
    ) -> Result<Value, Error> {
        let mut value = candidate.header.clone();
        let sections = query.load_sections();
        if sections.is_empty() {
            return Ok(value);
        }
        let instance = &candidate.instance;
        let wants = |name| sections.iter().any(|section| section == name);
        let revision = if wants("components") || wants("links") || wants("resources") {
            match self
                .store
                .revision(&self.workspace, &self.principal, &instance.revision_digest)
            {
                Ok(revision) => Some(revision),
                Err(StoreError::Serialization(_)) => {
                    for section in &sections {
                        value[section] = Value::Null;
                    }
                    value["read_error"] = json!("stored_record_incompatible");
                    return Ok(value);
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        let mut endpoints = Vec::new();
        if wants("resources") || query.needs_endpoints() {
            if let Ok((demand, projected_endpoints)) = resources::project(
                instance,
                revision.as_ref().expect("resource section loads revision"),
            ) {
                if wants("resources") {
                    let prober =
                        prober::observe(self.runtime.client.clone(), &instance.instance_key).await;
                    let mut demand = Some(demand);
                    resources::include_runtime(&mut demand, candidate.resource.as_ref(), prober);
                    value["resources"] = serialize(&demand)?;
                }
                endpoints = projected_endpoints;
            } else {
                value["resources"] = Value::Null;
                value["resource_error"] = json!("render_unavailable");
            }
        }
        if wants("components") {
            let components = component_views(
                revision.as_ref(),
                candidate.resource.as_ref(),
                candidate.header["runtime"]["state"] == "available",
                &endpoints,
            );
            let boundary = query.boundary("components", &query.component_cursor, context)?;
            let page = section_page(components, &boundary, query.limit, |c| &c.id);
            value["components"] = encoded_page(page, query, context, "components")?;
        }
        if wants("links") {
            let boundary = query.boundary("links", &query.link_cursor, context)?;
            let page = section_page(link_views(revision.as_ref()), &boundary, query.limit, |l| {
                &l.id
            });
            value["links"] = encoded_page(page, query, context, "links")?;
        }
        if wants("sessions") {
            let boundary = query.boundary("sessions", &query.session_cursor, context)?;
            value["sessions"] = encoded_page(
                self.environment_sessions(&instance.id, &boundary, query.limit)?,
                query,
                context,
                "sessions",
            )?;
        }
        if wants("activity") {
            let boundary = query.boundary("activity", &query.activity_cursor, context)?;
            let (operations, next_cursor) = self.store.instance_activity(
                &self.workspace,
                &self.principal,
                &instance.id,
                &boundary,
                query.limit,
            )?;
            let page = Page {
                items: operations.into_iter().map(Activity::from).collect(),
                next_cursor,
            };
            value["activity"] = encoded_page(page, query, context, "activity")?;
        }
        value["included_sections"] = json!(sections);
        Ok(value)
    }
}

fn serialize(value: &impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::failure(error.to_string(), None))
}

fn encoded_page<T: Serialize>(
    mut page: Page<T>,
    query: &EnvironmentReadQuery,
    context: &str,
    section: &str,
) -> Result<Value, Error> {
    page.next_cursor = page
        .next_cursor
        .map(|id| query.cursor_for(section, context, &id));
    serialize(&page)
}

fn project_cell(mut value: Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        if let Some(runtime) = value.get_mut("runtime").and_then(Value::as_object_mut) {
            if runtime
                .remove("message")
                .is_some_and(|message| !message.is_null())
            {
                runtime.insert("message_omitted".into(), json!(true));
            }
        }
        if value["owner"]
            .as_str()
            .is_some_and(|owner| owner.len() > 512)
        {
            value["owner"] = Value::Null;
            value["owner_omitted"] = json!(true);
        }
        return value;
    }
    let selected = crate::query::project(&value, fields);
    let mut continuations = serde_json::Map::new();
    for section in ["components", "links", "sessions", "activity"] {
        if let Some(cursor) = value.get(section).and_then(|page| page.get("next_cursor")) {
            continuations.insert(section.into(), cursor.clone());
        }
    }
    json!({"id":value["id"],"instance_key":value["instance_key"],"selected":selected,"continuations":continuations,"read_error":value.get("read_error"),"resource_error":value.get("resource_error"),"included_sections":value.get("included_sections")})
}

fn project_response(value: &Value, fields: &[String]) -> Value {
    let mut response = value.clone();
    for cell in response["cells"]["items"]
        .as_array_mut()
        .expect("directory items")
    {
        *cell = project_cell(cell.clone(), fields);
    }
    response
}

fn bound_directory(
    value: &mut Value,
    maximum: usize,
    query: &EnvironmentReadQuery,
    context: &str,
) -> Result<(), Error> {
    while serde_json::to_vec(&project_response(value, &query.fields))
        .map_err(|error| Error::failure(error.to_string(), None))?
        .len()
        > maximum
    {
        let items = value["cells"]["items"]
            .as_array_mut()
            .expect("directory items");
        if items.len() > 1 {
            items.pop();
            let id = items.last().unwrap()["id"].as_str().unwrap().to_owned();
            value["cells"]["next_cursor"] = json!(query.cursor_for("cells", context, &id));
            continue;
        }
        let Some(cell) = items.first_mut() else {
            return Err(Error::problem(
                "environment_response_too_large",
                "directory metadata exceeds the response limit; request fewer fields",
            ));
        };
        let mut shortened = false;
        for section in ["components", "links", "sessions", "activity"] {
            if let Some(entries) = cell
                .get_mut(section)
                .and_then(|page| page.get_mut("items"))
                .and_then(Value::as_array_mut)
            {
                if entries.len() > query.minimum_items(section) {
                    entries.pop();
                    let last = entries.last().unwrap();
                    let id = if section == "sessions" {
                        &last["session"]["id"]
                    } else {
                        &last["id"]
                    };
                    let next = query.cursor_for(section, context, id.as_str().unwrap());
                    cell[section]["next_cursor"] = json!(next);
                    shortened = true;
                }
            }
        }
        if !shortened {
            return Err(Error::problem(
                "environment_item_too_large",
                "selected environment item exceeds the response limit; use scan, fewer fields, or a smaller section limit",
            ));
        }
    }
    Ok(())
}
