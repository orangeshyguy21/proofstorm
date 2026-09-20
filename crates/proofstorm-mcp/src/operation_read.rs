//! Selected recorded receipt data, without polling the runtime or changing history.
use crate::read_query::{validate_pointer, wire};
use crate::{
    CallToolResult, ErrorData, MAX_AGENT_RESPONSE_BYTES, coded_invalid_request, serialized_size,
    store_error,
};
use proofstorm_core::digest_json;
use proofstorm_store::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationReadRequest {
    pub operation_id: String,
    /// RFC 6901 path, e.g. `/artifact/content/exit_code` or `/artifact/content/stdout`.
    /// Empty selects the entire recorded operation if it fits.
    #[serde(default)]
    pub pointer: String,
    /// Copy `operation_digest` from `activity_search` or a previous read to reject changed data.
    #[serde(default)]
    pub expected_digest: Option<String>,
    /// Unicode character offset for strings; element offset for arrays. Zero otherwise.
    #[serde(default)]
    pub offset: usize,
    /// Maximum characters or array elements. The response may contain fewer to fit.
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 4000))]
    pub limit: usize,
}

fn default_limit() -> usize {
    1000
}

#[derive(Serialize)]
struct ReadResult {
    operation_id: String,
    operation_digest: String,
    source: &'static str,
    pointer: String,
    value: Value,
    offset: usize,
    next_offset: Option<usize>,
    total_length: Option<usize>,
    unit: Option<&'static str>,
}

pub(super) fn read(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &OperationReadRequest,
) -> Result<CallToolResult, ErrorData> {
    validate_pointer(&request.pointer)?;
    if !(1..=4000).contains(&request.limit) {
        return Err(coded_invalid_request(
            "operation_read_limit",
            "limit must be between 1 and 4000",
        ));
    }
    let operation = store
        .operation(workspace, principal, &request.operation_id)
        .map_err(store_error)?;
    let document = serde_json::to_value(&operation)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    let digest = digest_json(&document);
    if request
        .expected_digest
        .as_ref()
        .is_some_and(|expected| *expected != digest)
    {
        return Err(coded_invalid_request(
            "operation_read_changed",
            "The recorded operation changed. Repeat activity_search or read without expected_digest to observe its current state",
        ));
    }
    let value = document.pointer(&request.pointer).ok_or_else(|| coded_invalid_request(
        "operation_read_pointer_missing", "The selected JSON pointer does not exist. Use a matching pointer from activity_search or read its parent object"
    ))?;
    let (total, unit) = match value {
        Value::String(text) => (Some(text.chars().count()), Some("characters")),
        Value::Array(items) => (Some(items.len()), Some("items")),
        _ => (None, None),
    };
    if total.map_or(request.offset != 0, |len| request.offset > len) {
        return Err(coded_invalid_request(
            "operation_read_offset",
            "offset must be within the selected string or array; use zero for other JSON values",
        ));
    }
    let mut length = total.map_or(0, |len| request.limit.min(len - request.offset));
    loop {
        let selected = match value {
            Value::String(text) => json!(
                text.chars()
                    .skip(request.offset)
                    .take(length)
                    .collect::<String>()
            ),
            Value::Array(items) => json!(&items[request.offset..request.offset + length]),
            _ => value.clone(),
        };
        let response = wire(&ReadResult {
            operation_id: operation.id.clone(),
            operation_digest: digest.clone(),
            source: "recorded",
            pointer: request.pointer.clone(),
            value: selected,
            offset: request.offset,
            next_offset: total
                .filter(|len| request.offset + length < *len)
                .map(|_| request.offset + length),
            total_length: total,
            unit,
        })?;
        if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
            return Ok(response);
        }
        if length <= 1 {
            return Err(coded_invalid_request(
                "operation_read_value_too_large",
                "The selected object or array item exceeds 32 KiB. Select a deeper JSON pointer; string values support offset/limit slices",
            ));
        }
        length /= 2;
    }
}
