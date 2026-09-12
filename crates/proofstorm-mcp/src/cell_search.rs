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
    #[serde(default)]
    pub draft_id: String,
    #[serde(default)]
    pub instance_id: Option<String>,
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
    /// Selected data, or null when it would dominate the response. In that
    /// case request the required fields; the stored value is unchanged.
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
    if request
        .fields
        .iter()
        .any(|field| !field.is_empty() && !field.starts_with('/'))
    {
        return Err(coded_invalid_request(
            "cell_search_field_invalid",
            "fields must be JSON pointers, such as /id or /config/txindex",
        ));
    }
    let pattern = if request.regex {
        request.query.clone()
    } else {
        regex::escape(&request.query)
    };
    let pattern = regex::RegexBuilder::new(&pattern)
        .case_insensitive(request.case_insensitive)
        .build()
        .map_err(|error| coded_invalid_request("cell_search_regex_invalid", error.to_string()))?;
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
    ));
    let offset = match &request.cursor {
        None => 0,
        Some(cursor) => {
            let (digest, offset) = cursor.rsplit_once(':').ok_or_else(invalid_cursor)?;
            if digest != fingerprint {
                return Err(invalid_cursor());
            }
            offset.parse::<usize>().map_err(|_| invalid_cursor())?
        }
    };
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
            if !pattern.is_match(&entry.to_string()) {
                continue;
            }
            let position = result.matched_count;
            result.matched_count += 1;
            if position < offset || page_full || result.items.len() >= request.limit.clamp(1, 200) {
                continue;
            }
            result
                .items
                .push(project_match(entry, section, index, &request.fields)?);
            // Reserve room for the count and continuation even on the last item.
            if serialized_size(&result)? + 256 > MAX_AGENT_RESPONSE_BYTES {
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
    Ok(result)
}

fn project_match(
    entry: &Value,
    section: &str,
    index: usize,
    fields: &[String],
) -> Result<CellSearchMatch, ErrorData> {
    let value = if fields.is_empty() {
        entry.clone()
    } else {
        Value::Object(
            fields
                .iter()
                .map(|field| {
                    (
                        field.clone(),
                        entry.pointer(field).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect(),
        )
    };
    let value_bytes = serialized_size(&value)?;
    let value_omitted = value_bytes > MAX_AGENT_RESPONSE_BYTES / 2;
    Ok(CellSearchMatch {
        id: entry["id"].as_str().unwrap_or_default().to_owned(),
        path: format!("/{section}/{index}"),
        value: (!value_omitted).then_some(value),
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
            "draft_id":"fleet","query":"\"txindex\":false","fields":["/id","/config/txindex"],"limit":5
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
}
