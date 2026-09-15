use crate::Error;
use proofstorm_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CandidateReadQuery {
    pub id: String,
    pub path: String,
    pub offset: usize,
    pub limit: usize,
    pub expected_digest: Option<String>,
}
impl Default for CandidateReadQuery {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            offset: 0,
            limit: 4096,
            expected_digest: None,
        }
    }
}

pub fn read(
    store: &Store,
    workspace: &str,
    principal: &str,
    query: &CandidateReadQuery,
) -> Result<Value, Error> {
    if query.path.len() > 512 || !(1..=4096).contains(&query.limit) {
        return Err(Error::problem(
            "candidate_read_invalid",
            "Use a bounded path and limit 1..4096",
        ));
    }
    let candidate = store.candidate_build(workspace, principal, &query.id)?;
    let record =
        serde_json::to_value(&candidate).map_err(|e| Error::failure(e.to_string(), None))?;
    let value = record.pointer(&query.path).ok_or_else(|| {
        Error::problem(
            "candidate_path_missing",
            "This path is unavailable; legacy builds may lack recorded evidence",
        )
    })?;
    let text = value
        .as_str()
        .map_or_else(|| serde_json::to_string_pretty(value), |s| Ok(s.to_owned()))
        .map_err(|e| Error::failure(e.to_string(), None))?;
    let digest =
        proofstorm_core::digest_json(&(workspace, principal, &query.id, &query.path, &text));
    if query
        .expected_digest
        .as_ref()
        .is_some_and(|expected| expected != &digest)
    {
        return Err(Error::problem(
            "candidate_read_changed",
            "Record changed; restart from offset zero",
        ));
    }
    let count = text.chars().count();
    if query.offset > count {
        return Err(Error::problem(
            "candidate_read_invalid",
            "Offset exceeds the selected value",
        ));
    }
    let selected = text
        .chars()
        .skip(query.offset)
        .take(query.limit.min(2048))
        .collect::<String>();
    let next = query.offset + selected.chars().count();
    Ok(
        json!({"id":query.id,"path":query.path,"digest":digest,"text":selected,"offset":query.offset,"next_offset":(next < count).then_some(next)}),
    )
}
