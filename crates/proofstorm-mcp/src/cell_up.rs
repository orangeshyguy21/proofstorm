//! Admission is independent of growing status and activity payloads.
use crate::{CallToolResult, ErrorData, developer_result};
use proofstorm_app::cell::UpResult;
use serde_json::json;

pub(super) fn result(up: UpResult) -> Result<CallToolResult, ErrorData> {
    let applied = up.applied;
    developer_result(json!({
        "accepted": true,
        "cell": {
            "name": up.cell.name,
            "instance_id": up.cell.instance_id,
            "incarnation_generation": up.cell.generation,
        },
        "instance_key": applied.instance.instance_key,
        "accepted_generation": applied.generation,
        "desired_generation": applied.instance.generation,
        "revision_digest": applied.revision_digest,
        "current_revision_digest": applied.instance.revision_digest,
        "runtime_reconciliation_failed": applied.reconciliation_error.is_some(),
        "activity_ready": up.activity_ready,
        "next_tool": "cell_inspect",
    }))
}
