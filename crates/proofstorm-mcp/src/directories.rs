//! Bounded, searchable directories for builds and optional evidence runs.
use crate::{
    CallToolResult, ErrorData, ProofstormMcp, coded_invalid_request, developer_result, read_query,
    store_error,
};
use proofstorm_core::{Capability, digest_json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DirectoryQuery {
    pub id: Option<String>,
    pub owner: Option<String>,
    pub phase: Option<String>,
    pub query: String,
    pub regex: bool,
    pub case_insensitive: bool,
    pub scan: bool,
    pub fields: Vec<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}
impl Default for DirectoryQuery {
    fn default() -> Self {
        Self {
            id: None,
            owner: None,
            phase: None,
            query: String::new(),
            regex: false,
            case_insensitive: false,
            scan: false,
            fields: vec![],
            cursor: None,
            limit: 20,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequest {
    #[serde(flatten)]
    pub cells: proofstorm_app::environment::EnvironmentReadQuery,
    /// Select the durable run directory, including empty runs. `instance_id` optionally limits it to one cell.
    /// When provided, cell detail selectors are not used. Ordinary environment reads remain unchanged.
    #[serde(default)]
    pub runs: Option<DirectoryQuery>,
}

impl ProofstormMcp {
    pub(super) fn run_directory(
        &self,
        request: &EnvironmentRequest,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize(Capability::ExperimentRead)?;
        let query = request.runs.as_ref().expect("run selector");
        let mut records = Vec::new();
        let mut after = String::new();
        loop {
            let page = self
                .store
                .run_directory(
                    &self.workspace,
                    &self.principal,
                    request.cells.instance_id.as_deref(),
                    &after,
                    200,
                )
                .map_err(store_error)?;
            let count = page.len();
            if let Some(last) = page.last() {
                after.clone_from(&last.id);
            }
            records.extend(page.into_iter().map(|run| json!(run)));
            if count < 200 {
                break;
            }
        }
        let mut result = page(
            records,
            query,
            "runs",
            &(&self.workspace, &self.principal, &request.cells.instance_id),
        )?;
        result["workspace"] = json!(
            self.store
                .workspace(&self.workspace, &self.principal)
                .map_err(store_error)?
        );
        result["capabilities"] = json!(
            self.store
                .capabilities(&self.workspace, &self.principal)
                .map_err(store_error)?
        );
        developer_result(result)
    }

    pub(super) fn candidate_directory(
        &self,
        query: &DirectoryQuery,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize(Capability::CandidateRead)?;
        let records = self
            .store
            .candidate_builds(&self.workspace, &self.principal)
            .map_err(store_error)?
            .into_iter()
            .map(|candidate| {
                let mut value = json!(crate::compact_candidate_build(&candidate, false));
                value["id"] = json!(candidate.id);
                value["owner_principal_id"] = json!(candidate.principal_id);
                value
            })
            .collect();
        developer_result(page(
            records,
            query,
            "items",
            &(&self.workspace, &self.principal),
        )?)
    }
}

fn page(
    mut records: Vec<Value>,
    query: &DirectoryQuery,
    key: &str,
    scope: &impl Serialize,
) -> Result<Value, ErrorData> {
    read_query::validate_fields(&query.fields)?;
    if !(1..=50).contains(&query.limit) || query.scan && !query.fields.is_empty() {
        return Err(coded_invalid_request(
            "directory_query_invalid",
            "limit must be 1..=50; choose scan or fields",
        ));
    }
    let pattern = read_query::pattern(&query.query, query.regex, query.case_insensitive)?;
    records.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    records.retain(|value| {
        query.id.as_ref().is_none_or(|id| value["id"] == *id)
            && query
                .owner
                .as_ref()
                .is_none_or(|owner| value["owner_principal_id"] == *owner)
            && query
                .phase
                .as_ref()
                .is_none_or(|phase| value["phase"] == *phase)
            && pattern.is_match(&value.to_string())
    });
    let mut selectors = query.clone();
    selectors.cursor = None;
    let digest = digest_json(&(scope, &selectors, &records));
    let offset = if let Some(cursor) = &query.cursor {
        let (bound, offset) = cursor
            .rsplit_once(':')
            .ok_or_else(|| coded_invalid_request("directory_cursor_invalid", "Invalid cursor"))?;
        if bound != digest {
            return Err(coded_invalid_request(
                "directory_changed",
                "Directory or selectors changed; repeat without cursor",
            ));
        }
        offset.parse::<usize>().map_err(|_| {
            coded_invalid_request("directory_cursor_invalid", "Invalid cursor offset")
        })?
    } else {
        0
    };
    if offset > records.len() {
        return Err(coded_invalid_request(
            "directory_cursor_invalid",
            "Cursor exceeds directory",
        ));
    }
    let mut result = json!({"observation_digest":digest,"matched_count":records.len(),key:[],"next_cursor":null});
    for (index, record) in records.iter().enumerate().skip(offset).take(query.limit) {
        let selected = if query.scan {
            json!({"id":record["id"],"phase":record["phase"],"instance_id":record["instance_id"],"owner_principal_id":record["owner_principal_id"]})
        } else {
            read_query::project(record, &query.fields)
        };
        result[key].as_array_mut().unwrap().push(selected);
        result["next_cursor"] =
            json!((index + 1 < records.len()).then(|| format!("{digest}:{}", index + 1)));
        if read_query::wire_size(&result)? + 2048 > crate::MAX_AGENT_RESPONSE_BYTES {
            result[key].as_array_mut().unwrap().pop();
            if index == offset {
                return Err(coded_invalid_request(
                    "directory_value_too_large",
                    "Select scan=true or smaller fields",
                ));
            }
            result["next_cursor"] = json!(format!("{digest}:{index}"));
            break;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_filters_precede_projection_and_cursors_bind_the_snapshot() {
        let records = (0..5)
            .map(|index| {
                json!({"id":format!("record-{index}"),
            "owner_principal_id":if index==0 { "other" } else { "agent" },
            "phase":"succeeded", "message":format!("needle-{index}")})
            })
            .collect::<Vec<_>>();
        let mut query = DirectoryQuery {
            owner: Some("agent".into()),
            phase: Some("succeeded".into()),
            query: "needle-[1-4]".into(),
            regex: true,
            fields: vec!["/id".into()],
            limit: 2,
            ..Default::default()
        };
        let first = page(records.clone(), &query, "items", &"actor").unwrap();
        assert_eq!(first["matched_count"], 4);
        assert_eq!(
            first["items"],
            json!([{ "/id":"record-1" },{ "/id":"record-2" }])
        );
        query.cursor = Some(first["next_cursor"].as_str().unwrap().into());
        let second = page(records.clone(), &query, "items", &"actor").unwrap();
        assert_eq!(
            second["items"],
            json!([{ "/id":"record-3" },{ "/id":"record-4" }])
        );
        assert!(second["next_cursor"].is_null());
        let mut changed = records.clone();
        changed[4]["message"] = json!("needle-4 changed");
        assert_eq!(
            page(changed, &query, "items", &"actor")
                .unwrap_err()
                .data
                .unwrap()["code"],
            "directory_changed"
        );
        assert!(page(records, &query, "items", &"different-actor").is_err());
    }

    #[test]
    fn large_directory_records_remain_searchable_and_scannable() {
        let records = vec![
            json!({"id":"large", "owner_principal_id":"agent", "phase":"failed",
            "message":format!("{}needle", "\\\"🦀".repeat(20000))}),
        ];
        let mut query = DirectoryQuery {
            query: "needle".into(),
            ..Default::default()
        };
        assert_eq!(
            page(records.clone(), &query, "items", &())
                .unwrap_err()
                .data
                .unwrap()["code"],
            "directory_value_too_large"
        );
        query.scan = true;
        let result = page(records, &query, "items", &()).unwrap();
        assert_eq!(result["items"][0]["id"], "large");
        assert!(read_query::wire_size(&result).unwrap() <= crate::MAX_AGENT_RESPONSE_BYTES);
    }
}
