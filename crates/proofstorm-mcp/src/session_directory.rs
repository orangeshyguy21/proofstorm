//! Search attribution records without inferring actor liveness.
use crate::{
    CallToolResult, ErrorData, MAX_AGENT_RESPONSE_BYTES, coded_invalid_request, developer_result,
    read_query, store_error,
};
use proofstorm_store::{SessionFilters, SessionWindow, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SessionListRequest {
    /// Canonical cell instance ID. May be omitted for exact ID or overlap lookup.
    pub instance_id: String,
    /// Exact session ID, distinct from an overlap query.
    pub id: Option<String>,
    /// Return intervals overlapping this session. The legacy `session_id` input is an alias.
    #[serde(alias = "session_id")]
    pub overlaps_with: Option<String>,
    pub principal_id: Option<String>,
    pub run_id: Option<String>,
    /// Active means unfinished tracking, not proof that an agent is running.
    pub phase: Option<proofstorm_core::SessionPhase>,
    /// Inclusive Unix time; `*_before_unix` bounds are exclusive.
    pub started_after_unix: Option<i64>,
    pub started_before_unix: Option<i64>,
    pub last_activity_after_unix: Option<i64>,
    pub last_activity_before_unix: Option<i64>,
    /// Match session JSON after exact filters, before result pagination.
    pub query: String,
    pub regex: bool,
    pub case_insensitive: bool,
    /// Only ID, actor, phase and last recorded activity. Mutually exclusive with fields.
    pub scan: bool,
    /// RFC 6901 pointers relative to a session, such as `/id` or `/experiment_id`.
    pub fields: Vec<String>,
    /// Continue with identical selectors. A directory change invalidates the cursor.
    pub cursor: String,
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
}
impl Default for SessionListRequest {
    fn default() -> Self {
        Self {
            instance_id: String::new(),
            id: None,
            overlaps_with: None,
            principal_id: None,
            run_id: None,
            phase: None,
            started_after_unix: None,
            started_before_unix: None,
            last_activity_after_unix: None,
            last_activity_before_unix: None,
            query: String::new(),
            regex: false,
            case_insensitive: false,
            scan: false,
            fields: vec![],
            cursor: String::new(),
            limit: 20,
        }
    }
}

#[derive(Debug, Serialize)]
struct DirectoryPage {
    instance_id: String,
    observation_digest: String,
    observed_at_unix: i64,
    scanned_count: usize,
    sessions: Vec<Value>,
    next_cursor: Option<String>,
}

pub(super) fn read(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &SessionListRequest,
) -> Result<CallToolResult, ErrorData> {
    read_query::validate_fields(&request.fields)?;
    if request.scan && !request.fields.is_empty()
        || !(1..=50).contains(&request.limit)
        || request.cursor.len() > 256
    {
        return Err(coded_invalid_request(
            "session_query_invalid",
            "choose scan or fields; limit must be 1..=50 and cursor at most 256 bytes",
        ));
    }
    let pattern = read_query::pattern(&request.query, request.regex, request.case_insensitive)?;
    let instance = resolve_instance(store, workspace, principal, request)?;
    let snapshot = store
        .session_observation_digest(workspace, principal, &instance)
        .map_err(store_error)?;
    let (observed_at, mut boundary, fingerprint) =
        continuation(request, workspace, principal, &instance, &snapshot)?;
    let filters = SessionFilters {
        id: request.id.clone(),
        principal_id: request.principal_id.clone(),
        run_id: request.run_id.clone(),
        phase: request.phase,
        started_after_unix: request.started_after_unix,
        started_before_unix: request.started_before_unix,
        last_activity_after_unix: request.last_activity_after_unix,
        last_activity_before_unix: request.last_activity_before_unix,
        overlaps_with: request.overlaps_with.clone(),
    };
    let candidates = store
        .session_candidates(
            workspace,
            principal,
            &instance,
            &filters,
            SessionWindow {
                after_id: &boundary,
                limit: 201,
                observed_at,
            },
        )
        .map_err(store_error)?;
    let count = candidates.len();
    let cursor = |id: &str| {
        format!(
            "{}:{observed_at}:{id}",
            proofstorm_core::digest_json(&(&fingerprint, id))
        )
    };
    let mut page = DirectoryPage {
        instance_id: instance.clone(),
        observation_digest: snapshot.clone(),
        observed_at_unix: observed_at,
        scanned_count: 0,
        sessions: vec![],
        next_cursor: None,
    };
    for (index, session) in candidates.into_iter().take(200).enumerate() {
        let document = json!(session);
        if pattern.is_match(&document.to_string()) {
            page.sessions.push(if request.scan {
                let omitted = session.principal_id.len() > 512;
                let mut summary = json!({"id":session.id,"principal_id":if omitted {None} else {Some(&session.principal_id)},"phase":session.phase,"last_activity_at_unix":session.last_activity_at_unix});
                if omitted { summary["principal_id_omitted"] = json!(true); }
                summary
            } else { read_query::project(&document, &request.fields) });
            page.next_cursor = Some(cursor(&session.id));
            if read_query::wire_size(&page)? + 64 > MAX_AGENT_RESPONSE_BYTES {
                page.sessions.pop();
                if page.sessions.is_empty() {
                    return Err(coded_invalid_request(
                        "session_response_too_large",
                        "one session exceeds the response budget; use scan or select smaller fields",
                    ));
                }
                page.next_cursor = Some(cursor(&boundary));
                break;
            }
        }
        boundary = session.id;
        page.scanned_count += 1;
        page.next_cursor = (index + 1 < count).then(|| cursor(&boundary));
        if page.sessions.len() >= request.limit as usize {
            break;
        }
    }
    if store
        .session_observation_digest(workspace, principal, &instance)
        .map_err(store_error)?
        != snapshot
    {
        return Err(stale());
    }
    developer_result(page)
}

fn continuation(
    request: &SessionListRequest,
    workspace: &str,
    principal: &str,
    instance: &str,
    snapshot: &str,
) -> Result<(i64, String, String), ErrorData> {
    let (observed_at, boundary, supplied_digest) = if request.cursor.is_empty() {
        (Store::session_observed_at(), String::new(), None)
    } else {
        let parts: Vec<_> = request.cursor.rsplitn(3, ':').collect();
        if parts.len() != 3 || parts[0].is_empty() {
            return Err(stale());
        }
        (
            parts[1].parse::<i64>().map_err(|_| stale())?,
            parts[0].to_owned(),
            Some(parts[2]),
        )
    };
    let mut selectors = request.clone();
    selectors.cursor.clear();
    selectors.limit = 0;
    let fingerprint = proofstorm_core::digest_json(&(
        workspace,
        principal,
        &instance,
        &snapshot,
        observed_at,
        selectors,
    ));
    if supplied_digest
        .is_some_and(|digest| digest != proofstorm_core::digest_json(&(&fingerprint, &boundary)))
    {
        return Err(stale());
    }
    Ok((observed_at, boundary, fingerprint))
}

fn resolve_instance(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &SessionListRequest,
) -> Result<String, ErrorData> {
    let mut instance = request.instance_id.clone();
    for id in [request.id.as_ref(), request.overlaps_with.as_ref()]
        .into_iter()
        .flatten()
    {
        let session = store
            .session(workspace, principal, id)
            .map_err(store_error)?;
        if !instance.is_empty() && instance != session.instance_id {
            return Err(coded_invalid_request(
                "session_scope_invalid",
                "session and instance selectors must belong to the same cell",
            ));
        }
        instance = session.instance_id;
    }
    if instance.is_empty() {
        return Err(coded_invalid_request(
            "session_scope_required",
            "provide instance_id, id, or overlaps_with",
        ));
    }
    Ok(instance)
}

fn stale() -> ErrorData {
    coded_invalid_request(
        "session_cursor_invalid",
        "The directory or selectors changed, or the cursor is invalid. Repeat without cursor",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::Capability;

    fn fixture() -> Store {
        let store = crate::tests::seeded_store();
        for cap in [Capability::CellOperate, Capability::ExperimentRead] {
            store.grant("alpha", "designer", cap).unwrap();
        }
        let spec = serde_json::from_value(json!({"api_version":"proofstorm/v1alpha1","name":"directory","links":[],"components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]})).unwrap();
        store
            .create_draft("alpha", "designer", "directory", &spec, "draft")
            .unwrap();
        let revision = store
            .publish("alpha", "designer", "directory", 1, "publish")
            .unwrap();
        store
            .materialize(
                "alpha",
                "designer",
                "directory",
                &revision.digest,
                "materialize",
            )
            .unwrap();
        let run = store
            .ensure_default_run("alpha", "designer", "directory", Capability::CellOperate)
            .unwrap();
        for index in 0..450 {
            store
                .track_session("alpha", "designer", &run.id, &format!("session-{index:03}"))
                .unwrap();
        }
        store
    }

    fn page(store: &Store, request: &SessionListRequest) -> Value {
        let response = read(store, "alpha", "designer", request).unwrap();
        assert!(crate::serialized_size(&response).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
        let wire = serde_json::to_value(&response).unwrap();
        let value = response.structured_content.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(wire["content"][0]["text"].as_str().unwrap()).unwrap(),
            value
        );
        value
    }

    #[test]
    fn sparse_search_advances_empty_pages_and_filters_before_projection() {
        let store = fixture();
        let before = store.observation_token("alpha", "designer").unwrap();
        let mut request:SessionListRequest = serde_json::from_value(json!({"instance_id":"directory","query":"SESSION-44[89]","regex":true,"case_insensitive":true,"fields":["/id","/phase"],"limit":1})).unwrap();
        let first = page(&store, &request);
        assert!(first["sessions"].as_array().unwrap().is_empty());
        assert_eq!(first["scanned_count"], 200);
        request.cursor = first["next_cursor"].as_str().unwrap().into();
        let second = page(&store, &request);
        assert!(second["sessions"].as_array().unwrap().is_empty());
        assert_ne!(second["next_cursor"], first["next_cursor"]);
        request.cursor = second["next_cursor"].as_str().unwrap().into();
        let third = page(&store, &request);
        assert_eq!(
            third["sessions"][0],
            json!({"/id":"session-448","/phase":"active"})
        );
        request.cursor = third["next_cursor"].as_str().unwrap().into();
        assert_eq!(page(&store, &request)["sessions"][0]["/id"], "session-449");
        assert_eq!(
            store.observation_token("alpha", "designer").unwrap(),
            before
        );
    }

    #[test]
    fn full_directory_scans_have_no_lost_ids_and_cursors_reject_changes() {
        let store = fixture();
        let mut request: SessionListRequest =
            serde_json::from_value(json!({"instance_id":"directory","scan":true,"limit":50}))
                .unwrap();
        let mut ids = std::collections::BTreeSet::new();
        loop {
            let result = page(&store, &request);
            for session in result["sessions"].as_array().unwrap() {
                assert!(ids.insert(session["id"].as_str().unwrap().to_owned()));
            }
            let Some(cursor) = result["next_cursor"].as_str() else {
                break;
            };
            request.cursor = cursor.into();
        }
        assert_eq!(ids.len(), 450);
        request.cursor.clear();
        request.cursor = page(&store, &request)["next_cursor"]
            .as_str()
            .unwrap()
            .into();
        let mut changed = request.clone();
        changed.principal_id = Some("reader".into());
        assert_eq!(
            read(&store, "alpha", "designer", &changed)
                .unwrap_err()
                .data
                .unwrap()["code"],
            "session_cursor_invalid"
        );
        store
            .finish_session("alpha", "designer", "session-000", "finish")
            .unwrap();
        assert_eq!(
            read(&store, "alpha", "designer", &request)
                .unwrap_err()
                .data
                .unwrap()["code"],
            "session_cursor_invalid"
        );
    }

    #[test]
    fn exact_lookup_and_legacy_overlap_alias_are_distinct_and_scoped() {
        let store = fixture();
        let exact: SessionListRequest =
            serde_json::from_value(json!({"id":"session-010","scan":true})).unwrap();
        assert_eq!(
            page(&store, &exact)["sessions"].as_array().unwrap().len(),
            1
        );
        let overlap: SessionListRequest =
            serde_json::from_value(json!({"session_id":"session-010","scan":true})).unwrap();
        assert_eq!(overlap.overlaps_with.as_deref(), Some("session-010"));
        assert!(
            page(&store, &overlap)["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["id"] != "session-010")
        );
        assert!(
            serde_json::from_value::<SessionListRequest>(
                json!({"session_id":"one","overlaps_with":"two"})
            )
            .is_err()
        );
        let wrong = SessionListRequest {
            instance_id: "another-cell".into(),
            ..exact
        };
        assert_eq!(
            read(&store, "alpha", "designer", &wrong)
                .unwrap_err()
                .data
                .unwrap()["code"],
            "session_scope_invalid"
        );
    }
}
