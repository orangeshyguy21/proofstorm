use crate::{Error, query};
use proofstorm_core::{CandidateBuild, digest_json};
use proofstorm_store::Store;
use proofstorm_view::DirectoryQuery;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Serialize)]
struct Page {
    observation_digest: String,
    matched_count: Option<usize>,
    scanned_count: usize,
    items: Vec<Value>,
    next_cursor: Option<String>,
}

/// Scan at most 500 stored records per call. A continuation may contain no matches.
pub fn directory(
    store: &Store,
    workspace: &str,
    principal: &str,
    query: &DirectoryQuery,
    maximum_bytes: usize,
) -> Result<Value, Error> {
    validate(query)?;
    let pattern = query::pattern(&query.query, query.regex, query.case_insensitive)
        .map_err(|error| Error::problem("directory_query_invalid", error.message))?;
    let generation = store.candidate_directory_generation(workspace, principal)?;
    let mut selectors = query.clone();
    selectors.cursor = None;
    let digest = digest_json(&(workspace, principal, generation, selectors));
    let mut after = if let Some(cursor) = &query.cursor {
        let (bound, after) = cursor.rsplit_once(':').ok_or_else(changed)?;
        if bound != digest {
            return Err(changed());
        }
        after.to_owned()
    } else {
        String::new()
    };
    let mut result = Page {
        observation_digest: digest.clone(),
        matched_count: None,
        scanned_count: 0,
        items: vec![],
        next_cursor: None,
    };
    'scan: for _ in 0..10 {
        let batch = store.candidate_build_page(workspace, principal, &after, false, 50)?;
        let count = batch.len();
        for (index, candidate) in batch.into_iter().enumerate() {
            let record = record(&candidate);
            let hit = query.id.as_ref().is_none_or(|id| id == &candidate.id)
                && query
                    .owner
                    .as_ref()
                    .is_none_or(|owner| owner == &candidate.principal_id)
                && query
                    .phase
                    .as_ref()
                    .is_none_or(|phase| record["phase"] == *phase)
                && pattern.is_match(&record.to_string());
            if hit {
                let selected = if query.scan {
                    json!({"id":candidate.id,"phase":candidate.phase,"owner_principal_id":candidate.principal_id,"implementation":candidate.implementation})
                } else if query.fields.is_empty() {
                    record
                } else {
                    query::project(&record, &query.fields)
                };
                if !query::push_bounded(
                    &mut result,
                    selected,
                    Some(format!("{digest}:{}", candidate.id)),
                    |page| (&mut page.items, &mut page.next_cursor),
                    |page| query::wire_size(page).map(|size| size + 64 <= maximum_bytes),
                )
                .map_err(|error| Error::failure(error.to_string(), None))?
                {
                    if result.items.is_empty() {
                        return Err(Error::problem(
                            "directory_value_too_large",
                            "Select scan or smaller fields",
                        ));
                    }
                    break 'scan;
                }
            }
            after = candidate.id;
            result.scanned_count += 1;
            result.next_cursor =
                (index + 1 < count || count == 50).then(|| format!("{digest}:{after}"));
            if result.items.len() == query.limit {
                break 'scan;
            }
        }
        if count < 50 {
            result.next_cursor = None;
            break;
        }
    }
    if store.candidate_directory_generation(workspace, principal)? != generation {
        return Err(changed());
    }
    serde_json::to_value(result).map_err(|e| Error::failure(e.to_string(), None))
}

fn record(candidate: &CandidateBuild) -> Value {
    let mut record = json!(super::receipt(candidate, false));
    record["id"] = json!(candidate.id);
    record["owner_principal_id"] = json!(candidate.principal_id);
    record["implementation"] = json!(candidate.implementation);
    record["repository"] = json!(candidate.repository);
    record["requested_source"] = json!(candidate.provenance.as_ref().map(|p| &p.requested_source));
    record["platform"] = json!(candidate.provenance.as_ref().map(|p| &p.platform));
    record["build_fingerprint"] = json!(candidate.request_digest);
    record["profile_digest"] = json!(candidate.provenance.as_ref().map(|p| &p.profile_digest));
    record["diagnostics_available"] = json!(candidate.diagnostics.is_some());
    record
}

fn validate(query: &DirectoryQuery) -> Result<(), Error> {
    if !(1..=50).contains(&query.limit)
        || query.query.len() > query::MAX_QUERY_BYTES
        || query::validate_fields(&query.fields).is_err()
        || query.scan && !query.fields.is_empty()
        || query.cursor.as_ref().is_some_and(|c| c.len() > 256)
    {
        return Err(Error::problem(
            "directory_query_invalid",
            "Use bounded query, limit 1..50, and either scan or valid JSON-pointer fields",
        ));
    }
    Ok(())
}
fn changed() -> Error {
    Error::problem(
        "directory_changed",
        "Build directory or selectors changed; restart without cursor",
    )
}
