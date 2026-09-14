//! Keep discovery of a cell small; fetch sections explicitly with JSON pointers.
use crate::{CallToolResult, ErrorData, compact_developer_view, developer_result, read_query};
use proofstorm_app::cell::CellView;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellInspectRequest {
    pub name: String,
    /// Optional RFC 6901 pointers into the detailed view, e.g. `/runtime/blockers`
    /// or `/runtime/retained_storage/chain`. Empty returns a compact summary.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Applies when explicitly selecting /activity. Prefer `activity_search` for
    /// filtered history and `operation_read` for receipt fields or text slices.
    #[serde(default)]
    pub after_sequence: u64,
}

pub(super) fn result(view: CellView, fields: &[String]) -> Result<CallToolResult, ErrorData> {
    read_query::validate_fields(fields)?;
    let view = compact_developer_view(view);
    let runtime = view.runtime.as_ref().map(|runtime| json!({
        "phase": runtime.phase,
        "generation": runtime.generation,
        "observed_generation": runtime.observed_generation,
        "observed_revision_digest": runtime.observed_revision_digest,
        "ready_components": runtime.ready_components,
        "total_components": runtime.total_components,
        "blockers": runtime.blockers.iter().map(|blocker| json!({"component_id":blocker.component_id,"reason":blocker.reason})).collect::<Vec<_>>(),
    }));
    let mut response = json!({
        "cell": view.cell,
        "instance_key": view.instance_key,
        "desired_generation": view.desired_generation,
        "runtime": runtime,
        "run_id": view.run.as_ref().map(|run| &run.id),
        "observed_at_unix": view.observed_at_unix,
        "read_tools": {"configuration":"cell_search", "components":"cell_component_status_list", "activity":"activity_search", "receipt":"operation_read", "sessions":"session_list"},
    });
    if !fields.is_empty() {
        let value = serde_json::to_value(&view)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        response["selected"] = read_query::project(&value, fields);
        if fields
            .iter()
            .any(|field| field == "/activity" || field.starts_with("/activity/"))
        {
            response["next_sequence"] = json!(view.next_sequence);
        }
    }
    developer_result(response)
}
