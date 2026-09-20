//! Bounded search, JSON-pointer selection, and complete MCP response measurement.
//! Directories retain their own filters, cursor identities, and traversal rules.
use crate::Error;
use serde::Serialize;
use serde_json::Value;

mod page;
pub use page::push_bounded;

pub const MAX_QUERY_BYTES: usize = 4096;
pub const MAX_FIELDS: usize = 32;
pub const MAX_FIELD_BYTES: usize = 512;

pub fn pattern(query: &str, regex: bool, insensitive: bool) -> Result<regex::Regex, Error> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(Error::problem(
            "search_query_invalid",
            "query must be at most 4096 bytes",
        ));
    }
    regex::RegexBuilder::new(&if regex {
        query.to_owned()
    } else {
        regex::escape(query)
    })
    .case_insensitive(insensitive)
    .size_limit(1 << 20)
    .build()
    .map_err(|error| Error::problem("search_regex_invalid", error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerError {
    RootOrLength,
    Escape,
}

/// Validate syntax without resolving a field. Individual reads may allow longer
/// pointers than directory field selections, so the byte limit is explicit.
pub fn validate_pointer(pointer: &str, maximum_bytes: usize) -> Result<(), PointerError> {
    if pointer.len() > maximum_bytes || (!pointer.is_empty() && !pointer.starts_with('/')) {
        return Err(PointerError::RootOrLength);
    }
    let mut chars = pointer.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err(PointerError::Escape);
        }
    }
    Ok(())
}

pub fn validate_fields(fields: &[String]) -> Result<(), Error> {
    if fields.len() > MAX_FIELDS
        || fields
            .iter()
            .any(|field| validate_pointer(field, MAX_FIELD_BYTES).is_err())
    {
        return Err(Error::problem(
            "search_fields_invalid",
            "select at most 32 RFC 6901 JSON pointers, each at most 512 bytes",
        ));
    }
    Ok(())
}

/// Preserve requested pointer keys, including missing fields as null. An empty
/// selector list returns the document; the empty pointer selects its root.
#[must_use]
pub fn project(entry: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return entry.clone();
    }
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
}

pub fn wire(value: &impl Serialize) -> Result<rmcp::model::CallToolResult, serde_json::Error> {
    serde_json::to_value(value).map(rmcp::model::CallToolResult::structured)
}

/// Measure both structured content and its text representation, including JSON
/// escaping. Callers must include continuations and other page metadata first.
pub fn wire_size(value: &impl Serialize) -> Result<usize, serde_json::Error> {
    serde_json::to_vec(&wire(value)?).map(|encoded| encoded.len())
}

#[cfg(test)]
mod tests;
