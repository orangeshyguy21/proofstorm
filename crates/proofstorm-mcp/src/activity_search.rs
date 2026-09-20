//! Passive, bounded search over recorded operations across a cell's actors and runs.
use crate::{
    CallToolResult, ErrorData, MAX_AGENT_RESPONSE_BYTES, coded_invalid_request, developer_result,
    read_query, serialized_size, store_error,
};
use proofstorm_core::{CellOperation, OperationKind, OperationPhase, digest_json};
use proofstorm_store::Store;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCAN_LIMIT: usize = 200;
const MATCH_LIMIT: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivitySearchRequest {
    /// Cell name or canonical instance ID. Searches all actors and runs in it.
    pub name: String,
    /// Literal text in recorded JSON scalar values, including request and receipt output.
    /// Empty matches every operation that satisfies the filters.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_insensitive: bool,
    #[serde(default)]
    pub component: Option<String>,
    /// Recorded operation phase; a succeeded execution can have a nonzero native exit code.
    #[serde(default)]
    pub phase: Option<OperationPhase>,
    /// Exact native exit code, when recorded. Missing codes never match.
    #[serde(default)]
    pub native_exit_code: Option<i64>,
    #[serde(default)]
    pub kind: Option<OperationKind>,
    #[serde(default)]
    pub principal_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Inclusive operation acceptance time, in Unix seconds.
    #[serde(default)]
    pub accepted_after_unix: Option<i64>,
    /// Exclusive operation acceptance time, in Unix seconds.
    #[serde(default)]
    pub accepted_before_unix: Option<i64>,
    /// Optional RFC 6901 fields, e.g. `/artifact/content/exit_code`. Large values
    /// are explicitly omitted; use `operation_read` with the returned digest.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: usize,
    /// Continue with identical filters. Changed cell history invalidates the cursor.
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Serialize)]
struct SearchResult {
    instance_id: String,
    observation_digest: String,
    source: &'static str,
    scanned_count: usize,
    items: Vec<SearchHit>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct SearchHit {
    operation_id: String,
    operation_digest: String,
    activity: proofstorm_view::Activity,
    matches: Vec<TextMatch>,
    matches_truncated: bool,
    fields: Vec<SelectedField>,
}

#[derive(Debug, Serialize)]
struct TextMatch {
    pointer: String,
    excerpt: String,
    /// Unicode character offset of the excerpt within a string value.
    offset: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SelectedField {
    pointer: String,
    exists: bool,
    value: Option<Value>,
    value_bytes: usize,
    value_omitted: bool,
}

fn pattern(request: &ActivitySearchRequest) -> Result<Regex, ErrorData> {
    if request.query.len() > proofstorm_app::query::MAX_QUERY_BYTES
        || request.fields.len() > 16
        || !(1..=50).contains(&request.limit)
    {
        return Err(coded_invalid_request(
            "activity_search_limits",
            "Use a query of at most 4096 bytes, at most 16 fields, and limit between 1 and 50",
        ));
    }
    if let (Some(start), Some(end)) = (request.accepted_after_unix, request.accepted_before_unix)
        && start >= end
    {
        return Err(coded_invalid_request(
            "activity_search_time_range",
            "accepted_after_unix must be less than accepted_before_unix",
        ));
    }
    for field in &request.fields {
        read_query::validate_pointer(field)?;
    }
    proofstorm_app::query::pattern(&request.query, request.regex, request.case_insensitive)
        .map_err(|error| coded_invalid_request("activity_search_regex_invalid", error.message))
}

fn matches_filters(
    request: &ActivitySearchRequest,
    op: &CellOperation,
    activity: &proofstorm_view::Activity,
) -> bool {
    request
        .component
        .as_ref()
        .is_none_or(|v| activity.components.contains(v))
        && request.phase.is_none_or(|v| op.phase == v)
        && request
            .native_exit_code
            .is_none_or(|v| activity.native_exit_code == Some(v))
        && request.kind.is_none_or(|v| op.kind == v)
        && request
            .principal_id
            .as_ref()
            .is_none_or(|v| op.principal_id == *v)
        && request
            .run_id
            .as_ref()
            .is_none_or(|v| op.experiment_id == *v)
        && request
            .session_id
            .as_ref()
            .is_none_or(|v| op.session_id == *v)
        && request
            .accepted_after_unix
            .is_none_or(|v| op.accepted_at_unix >= v)
        && request
            .accepted_before_unix
            .is_none_or(|v| op.accepted_at_unix < v)
}

fn text_matches(value: &Value, pointer: &str, regex: &Regex, found: &mut Vec<TextMatch>) {
    if found.len() > MATCH_LIMIT {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let key = key.replace('~', "~0").replace('/', "~1");
                text_matches(child, &format!("{pointer}/{key}"), regex, found);
                if found.len() > MATCH_LIMIT {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                text_matches(child, &format!("{pointer}/{index}"), regex, found);
                if found.len() > MATCH_LIMIT {
                    break;
                }
            }
        }
        _ => {
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            if let Some(matched) = regex.find(&text) {
                let offset = text[..matched.start()].chars().count().saturating_sub(60);
                found.push(TextMatch {
                    pointer: pointer.into(),
                    excerpt: text.chars().skip(offset).take(180).collect(),
                    offset: value.is_string().then_some(offset),
                });
            }
        }
    }
}

fn hit(
    request: &ActivitySearchRequest,
    regex: &Regex,
    op: CellOperation,
) -> Result<Option<SearchHit>, ErrorData> {
    let activity = proofstorm_view::Activity::from(op.clone());
    if !matches_filters(request, &op, &activity) {
        return Ok(None);
    }
    let document = serde_json::to_value(&op)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    let mut matches = Vec::new();
    if !request.query.is_empty() {
        text_matches(&document, "", regex, &mut matches);
        if matches.is_empty() {
            return Ok(None);
        }
    }
    let matches_truncated = matches.len() > MATCH_LIMIT;
    matches.truncate(MATCH_LIMIT);
    let mut fields = Vec::new();
    for pointer in &request.fields {
        let value = document.pointer(pointer);
        let value_bytes = serialized_size(&value)?;
        let omitted = value_bytes > 2048;
        fields.push(SelectedField {
            pointer: pointer.clone(),
            exists: value.is_some(),
            value: if omitted { None } else { value.cloned() },
            value_bytes,
            value_omitted: omitted,
        });
    }
    let mut hit = SearchHit {
        operation_id: op.id,
        operation_digest: digest_json(&document),
        activity,
        matches,
        matches_truncated,
        fields,
    };
    // An individual match must leave room for pagination and the dual MCP envelope.
    while read_query::wire_size(&hit)? > MAX_AGENT_RESPONSE_BYTES / 2 {
        if let Some(field) = hit.fields.iter_mut().rev().find(|f| f.value.is_some()) {
            field.value = None;
            field.value_omitted = true;
        } else if hit.matches.pop().is_some() {
            hit.matches_truncated = true;
        } else {
            return Err(coded_invalid_request(
                "activity_search_item_too_large",
                "Match metadata exceeds the response budget; request fewer fields",
            ));
        }
    }
    Ok(Some(hit))
}

fn stale_cursor() -> ErrorData {
    coded_invalid_request(
        "activity_search_cursor_invalid",
        "The query, cell history, or cursor changed. Repeat activity_search without cursor; no recorded data was changed",
    )
}

pub(super) fn search(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &ActivitySearchRequest,
) -> Result<CallToolResult, ErrorData> {
    let regex = pattern(request)?;
    let cell = store
        .resolve_cell(workspace, principal, &request.name)
        .map_err(store_error)?;
    let snapshot = store
        .activity_observation_digest(workspace, principal, &cell.instance_id)
        .map_err(store_error)?;
    let mut query = request.clone();
    query.cursor = None;
    let fingerprint = digest_json(&(workspace, principal, &cell.instance_id, &snapshot, query));
    let mut boundary = match &request.cursor {
        None => String::new(),
        Some(cursor) => {
            let (digest, boundary) = cursor.rsplit_once(':').ok_or_else(stale_cursor)?;
            if digest != fingerprint || boundary.is_empty() {
                return Err(stale_cursor());
            }
            boundary.into()
        }
    };
    let mut result = SearchResult {
        instance_id: cell.instance_id.clone(),
        observation_digest: snapshot.clone(),
        source: "recorded",
        scanned_count: 0,
        items: Vec::new(),
        next_cursor: None,
    };
    'scan: loop {
        let (operations, next) = store
            .instance_activity(workspace, principal, &cell.instance_id, &boundary, 50)
            .map_err(store_error)?;
        let count = operations.len();
        for (index, operation) in operations.into_iter().enumerate() {
            let id = operation.id.clone();
            if let Some(hit) = hit(request, &regex, operation)? {
                // Measure with the candidate's actual cursor and reserve room
                // for the final scanned_count increment.
                if !proofstorm_app::query::push_bounded(
                    &mut result,
                    hit,
                    Some(format!("{fingerprint}:{id}")),
                    |page| (&mut page.items, &mut page.next_cursor),
                    |page| {
                        read_query::wire_size(page)
                            .map(|size| size + 64 <= MAX_AGENT_RESPONSE_BYTES)
                    },
                )? {
                    if result.items.is_empty() {
                        return Err(coded_invalid_request(
                            "activity_search_item_too_large",
                            "Match metadata exceeds the response budget; request fewer fields",
                        ));
                    }
                    break 'scan;
                }
            }
            boundary = id;
            result.scanned_count += 1;
            let more = index + 1 < count || next.is_some();
            result.next_cursor = more.then(|| format!("{fingerprint}:{boundary}"));
            if result.items.len() >= request.limit || result.scanned_count >= SCAN_LIMIT {
                break 'scan;
            }
        }
        if next.is_none() {
            break;
        }
    }
    if store
        .activity_observation_digest(workspace, principal, &cell.instance_id)
        .map_err(store_error)?
        != snapshot
    {
        return Err(stale_cursor());
    }
    developer_result(result)
}

#[cfg(test)]
mod tests;
