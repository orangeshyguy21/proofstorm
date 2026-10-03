//! Compact, metadata-only recovery hints. Never change a command's output visibility.
use proofstorm_core::CellOperation;
use serde_json::{Value, json};

pub(super) fn describe(operation: &CellOperation, digest: &str) -> Option<Value> {
    let content = &operation.artifact.as_ref()?.content;
    let mode = content.get("output_mode").and_then(Value::as_str);
    let mut reads = Vec::new();
    for field in ["stdout", "stderr", "selected_output"] {
        let Some(value) = content.get(field) else {
            continue;
        };
        if value.is_null() || value.as_str() == Some("") {
            continue;
        }
        reads.push(json!({
            "tool":"operation_read",
            "arguments":{"operation_id":operation.id,"expected_digest":digest,
                "pointer":format!("/artifact/content/{field}"),"limit":1000}
        }));
    }
    let mut streams = serde_json::Map::new();
    for stream in ["stdout", "stderr"] {
        let metadata = &content["private_output"][stream];
        let mut counts = serde_json::Map::new();
        for field in ["bytes_observed", "retained_bytes"] {
            if let Some(count) = metadata[field].as_u64() {
                counts.insert(field.into(), json!(count));
            }
        }
        if !counts.is_empty() {
            streams.insert(stream.into(), Value::Object(counts));
        }
    }
    if mode.is_none() && reads.is_empty() && streams.is_empty() {
        return None;
    }
    Some(json!({
        "mode":mode,
        "streams":streams,
        "reads":reads,
        "private_streams_readable":false,
        "guidance":"Read instructions select only output already exposed in the recorded receipt. Retained byte counts describe capture at execution time, not current availability. Private streams cannot be revealed through operation_read. If needed, inspect application state with a read-only native command and an appropriate output mode; do not replay a state-changing command just to expose its output."
    }))
}
