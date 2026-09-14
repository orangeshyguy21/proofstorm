//! Search the stored topology before sending data into an agent's context.
use proofstorm_core::digest_json;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CellReadResponse, MAX_AGENT_RESPONSE_BYTES, coded_invalid_request, serialized_size};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellSearchSection {
    #[default]
    Components,
    Links,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellSearchRequest {
    #[serde(flatten)]
    pub target: crate::cell_read::CellTarget,
    /// Match each component/link's compact JSON. Empty matches everything.
    /// For example, literal `"txindex":false` finds explicit disabled settings.
    #[serde(default)]
    pub query: String,
    /// Interpret query as a regular expression instead of literal text.
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_insensitive: bool,
    #[serde(default)]
    pub section: CellSearchSection,
    /// Optional JSON pointers relative to each matched object, e.g. /id and
    /// /config/txindex. Empty returns complete matching objects. Missing fields
    /// are returned as null. Select fields if a matching object is too large.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Return IDs, paths and sizes without values; use fields to retrieve a subtree.
    #[serde(default)]
    pub scan: bool,
    /// Exact component or link ID, applied before text search.
    #[serde(default)]
    pub id: Option<String>,
    /// Page size; the server may return fewer items to fit the response.
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CellSearchMatch {
    pub id: String,
    pub path: String,
    /// Selected data; null in scan mode or when `value_omitted` is true.
    /// For an omitted value, request smaller fields; the stored value is unchanged.
    pub value: Option<Value>,
    pub value_bytes: usize,
    pub value_omitted: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CellSearchResult {
    pub id: String,
    pub version: u64,
    pub cell_digest: String,
    pub component_count: usize,
    pub link_count: usize,
    pub matched_count: usize,
    pub items: Vec<CellSearchMatch>,
    pub next_cursor: Option<String>,
}

pub fn search(
    document: CellReadResponse,
    request: &CellSearchRequest,
) -> Result<CellSearchResult, ErrorData> {
    crate::read_query::validate_fields(&request.fields)?;
    if request.scan && !request.fields.is_empty() {
        return Err(coded_invalid_request(
            "search_fields_invalid",
            "choose scan or fields, not both",
        ));
    }
    let pattern =
        crate::read_query::pattern(&request.query, request.regex, request.case_insensitive)?;
    let cell_digest = digest_json(&document.cell);
    let fingerprint = digest_json(&(
        &document.id,
        document.version,
        &cell_digest,
        &request.query,
        request.regex,
        request.case_insensitive,
        request.section,
        &request.fields,
        request.scan,
        &request.id,
    ));
    let offset = search_start(request.cursor.as_deref(), &fingerprint)?;
    let mut result = CellSearchResult {
        id: document.id,
        version: document.version,
        cell_digest,
        component_count: document.cell.components.len(),
        link_count: document.cell.links.len(),
        matched_count: 0,
        items: Vec::new(),
        next_cursor: None,
    };
    let sections = match request.section {
        CellSearchSection::Components => vec![(
            "components",
            serde_json::to_value(&document.cell.components),
        )],
        CellSearchSection::Links => vec![("links", serde_json::to_value(&document.cell.links))],
        CellSearchSection::All => vec![
            (
                "components",
                serde_json::to_value(&document.cell.components),
            ),
            ("links", serde_json::to_value(&document.cell.links)),
        ],
    };
    let mut page_full = false;
    for (section, entries) in sections {
        let entries = entries.map_err(|error| {
            coded_invalid_request("cell_search_serialization", error.to_string())
        })?;
        for (index, entry) in entries
            .as_array()
            .expect("typed component/link array")
            .iter()
            .enumerate()
        {
            if request
                .id
                .as_ref()
                .is_some_and(|id| entry["id"].as_str() != Some(id))
                || !pattern.is_match(&entry.to_string())
            {
                continue;
            }
            let position = result.matched_count;
            result.matched_count += 1;
            if position < offset || page_full || result.items.len() >= request.limit.clamp(1, 200) {
                continue;
            }
            result.items.push(project_match(
                entry,
                section,
                index,
                &request.fields,
                request.scan,
            )?);
            // Reserve room for the count and continuation even on the last item.
            if crate::read_query::wire_size(&result)? + 512 > MAX_AGENT_RESPONSE_BYTES {
                result.items.pop();
                page_full = true;
            }
        }
    }
    if offset > result.matched_count {
        return Err(invalid_cursor());
    }
    let next = offset + result.items.len();
    if next < result.matched_count {
        result.next_cursor = Some(format!("{fingerprint}:{next}"));
    }
    if result.items.is_empty() && result.next_cursor.is_some() {
        return Err(coded_invalid_request(
            "cell_search_response_too_large",
            "no matching item fits; use scan=true or select smaller fields",
        ));
    }
    crate::developer_result(&result)?;
    Ok(result)
}

fn search_start(cursor: Option<&str>, fingerprint: &str) -> Result<usize, ErrorData> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let (digest, offset) = cursor.rsplit_once(':').ok_or_else(invalid_cursor)?;
    if digest != fingerprint {
        return Err(invalid_cursor());
    }
    offset.parse().map_err(|_| invalid_cursor())
}

fn project_match(
    entry: &Value,
    section: &str,
    index: usize,
    fields: &[String],
    scan: bool,
) -> Result<CellSearchMatch, ErrorData> {
    let value = crate::read_query::project(entry, fields);
    let value_bytes = serialized_size(&value)?;
    let value_omitted =
        !scan && crate::read_query::wire_size(&value)? > MAX_AGENT_RESPONSE_BYTES / 2;
    Ok(CellSearchMatch {
        id: entry["id"].as_str().unwrap_or_default().to_owned(),
        path: format!("/{section}/{index}"),
        value: (!scan && !value_omitted).then_some(value),
        value_bytes,
        value_omitted,
    })
}

fn invalid_cursor() -> ErrorData {
    coded_invalid_request(
        "cell_search_cursor_invalid",
        "The document or query changed, or the cursor is invalid. Repeat the search without cursor",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> CellReadResponse {
        serde_json::from_value(serde_json::json!({"id":"fleet","workspace_id":"alpha","version":1,
            "cell":{"api_version":"proofstorm/v1alpha1","name":"fleet","links":[],
                "components":(0..25).map(|i| serde_json::json!({"id":format!("node-{i:02}"),"kind":"bitcoin",
                    "implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1",
                    "control":"cell","config":{"txindex":i%2==0}})).collect::<Vec<_>>()}})).unwrap()
    }

    #[test]
    fn search_projects_exact_matches_and_binds_continuation_to_query_and_document() {
        let mut request: CellSearchRequest = serde_json::from_value(serde_json::json!({
            "plan_id":"fleet","query":"\"txindex\":false","fields":["/id","/config/txindex"],"limit":5
        })).unwrap();
        let first = search(document(), &request).unwrap();
        assert_eq!(first.matched_count, 12);
        assert_eq!(first.items.len(), 5);
        assert_eq!(first.items[0].id, "node-01");
        assert_eq!(
            first.items[0].value.as_ref().unwrap()["/config/txindex"],
            false
        );
        request.cursor = first.next_cursor;
        let next = search(document(), &request).unwrap();
        assert_eq!(next.items[0].id, "node-11");
        let mut changed = document();
        changed.version += 1;
        assert!(search(changed, &request).is_err());
        request.query = "node-".into();
        assert!(search(document(), &request).is_err());
    }

    #[test]
    fn regex_and_large_values_have_recoverable_projection() {
        let request: CellSearchRequest =
            serde_json::from_value(serde_json::json!({"query":"node-(01|03)","regex":true}))
                .unwrap();
        let mut doc = document();
        doc.cell.components[1]
            .config
            .insert("large".into(), "x".repeat(50_000).into());
        let result = search(doc.clone(), &request).unwrap();
        assert_eq!(result.matched_count, 2);
        assert!(result.items[0].value_omitted);
        let projected = search(
            doc,
            &CellSearchRequest {
                fields: vec!["/id".into()],
                ..request
            },
        )
        .unwrap();
        assert!(!projected.items[0].value_omitted);
        assert_eq!(projected.items[0].value.as_ref().unwrap()["/id"], "node-01");
    }

    #[test]
    fn wire_bounded_pages_and_scans_always_advance() {
        let mut doc = document();
        for component in &mut doc.cell.components {
            component
                .config
                .insert("detail".into(), "\"\\\n".repeat(500).into());
        }
        let mut request: CellSearchRequest =
            serde_json::from_value(serde_json::json!({"limit":20})).unwrap();
        let mut ids = Vec::new();
        loop {
            let page = search(doc.clone(), &request).unwrap();
            assert!(crate::read_query::wire_size(&page).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
            assert!(!page.items.is_empty());
            ids.extend(page.items.iter().map(|item| item.id.clone()));
            request.cursor = page.next_cursor;
            if request.cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids.len(), 25);
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            25
        );
        request.scan = true;
        request.id = Some("node-03".into());
        let scan = search(doc.clone(), &request).unwrap();
        assert_eq!(scan.matched_count, 1);
        assert!(scan.items[0].value.is_none());
        assert_eq!(scan.items[0].path, "/components/3");
        request.scan = false;
        request.fields = vec!["/config/txindex".into()];
        assert_eq!(
            search(doc, &request).unwrap().items[0]
                .value
                .as_ref()
                .unwrap()["/config/txindex"],
            false
        );
        request.fields = vec!["/broken~escape".into()];
        assert!(search(document(), &request).is_err());
    }
}
