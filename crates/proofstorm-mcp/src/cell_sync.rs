//! Keep synchronized activity pageable within the complete MCP response budget.
use crate::{
    CallToolResult, ErrorData, MAX_AGENT_RESPONSE_BYTES, bounded_agent_response,
    compact_developer_view, serialized_size,
};

pub(super) fn result(view: proofstorm_app::cell::CellView) -> Result<CallToolResult, ErrorData> {
    let mut view = compact_developer_view(view);
    loop {
        let value = serde_json::to_value(&view)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        // Measure both structuredContent and its escaped text copy, including
        // the MCP envelope. Counting entries or just the payload is insufficient.
        let response = CallToolResult::structured(value);
        if serialized_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
            return Ok(response);
        }
        if view.activity.len() <= 1 {
            // Never hide an oversized entry behind an empty page or advance
            // past it. Keep the explicit size error if no useful page fits.
            return bounded_agent_response(response);
        }
        view.activity.pop();
        view.next_sequence = view.activity.last().map(|item| item.sequence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_app::cell::{Activity, CellView};
    use proofstorm_core::{OperationKind, OperationPhase};
    use serde_json::{Value, json};

    fn activity(sequence: u64) -> Activity {
        Activity {
            revision_digest: format!("sha256:{}", "a".repeat(64)),
            id: format!("payment-{sequence:03}-{}", "a".repeat(51)),
            sequence,
            kind: OperationKind::ComponentExecLive,
            phase: OperationPhase::Succeeded,
            accepted_at_unix: 1,
            completed_at_unix: Some(2),
            artifact_digest: Some(format!("sha256:{}", "b".repeat(64))),
            session_id: "s".repeat(63),
            run_id: "r".repeat(63),
            native_exit_code: Some(0),
            native_timed_out: Some(false),
            cleanup_verified: Some(true),
            principal_id: "agent".into(),
            components: vec!["wallet".into()],
        }
    }

    fn view(activity: Vec<Activity>, next_sequence: Option<u64>) -> CellView {
        CellView {
            cell: proofstorm_store::CellHandle {
                name: "alpha-payments".into(),
                generation: 1,
                owner: "agent".into(),
                config_digest: format!("sha256:{}", "c".repeat(64)),
                phase: proofstorm_store::CellHandlePhase::Open,
                instance_id: "cell-payments".into(),
            },
            instance_key: Some("payments".into()),
            reconciliation_error: None,
            runtime: None,
            run: None,
            sessions: proofstorm_store::SessionPage {
                sessions: (0..20)
                    .map(|n| proofstorm_core::Session {
                        id: format!("session-{n:02}-{}", "s".repeat(52)),
                        workspace_id: "alpha".into(),
                        experiment_id: "r".repeat(63),
                        instance_id: "cell-payments".into(),
                        principal_id: "agent".into(),
                        phase: proofstorm_core::SessionPhase::Active,
                        started_at_unix: 1,
                        last_activity_at_unix: 2,
                        finished_at_unix: None,
                    })
                    .collect(),
                next_cursor: Some("session-continuation".into()),
                observed_at_unix: 2,
            },
            activity,
            next_sequence,
            observed_at_unix: 2,
        }
    }

    fn visible(response: &CallToolResult) -> Value {
        let wire = serde_json::to_value(response).unwrap();
        assert!(serialized_size(response).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
        let text: Value =
            serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(Some(&text), response.structured_content.as_ref());
        text
    }

    #[test]
    fn cell_sync_pages_preserve_all_activity_and_both_mcp_content_forms() {
        let records: Vec<_> = (1..=43).map(activity).collect();
        let mut after = 0;
        let mut collected = Vec::new();
        loop {
            let entries: Vec<_> = records
                .iter()
                .filter(|item| item.sequence > after)
                .take(20)
                .cloned()
                .collect();
            let next = entries
                .last()
                .filter(|_| entries.len() == 20)
                .map(|item| item.sequence);
            let page = view(entries, next);
            let original =
                serde_json::to_value(compact_developer_view(view(page.activity.clone(), next)))
                    .unwrap();
            if after == 0 {
                assert!(
                    serialized_size(&CallToolResult::structured(original.clone())).unwrap()
                        > MAX_AGENT_RESPONSE_BYTES,
                    "fixture must reproduce the original cell_sync failure"
                );
            }
            let response = visible(&result(page).unwrap());
            assert_eq!(response["cell"], original["cell"]);
            assert_eq!(response["sessions"], original["sessions"]);
            let entries = response["activity"].as_array().unwrap();
            assert!(!entries.is_empty());
            collected.extend(entries.iter().cloned());
            let Some(next) = response["next_sequence"].as_u64() else {
                break;
            };
            assert!(next > after, "continuation must always make progress");
            assert_eq!(next, entries.last().unwrap()["sequence"]);
            after = next;
        }
        assert_eq!(
            json!(collected),
            json!(records),
            "no skipped or altered receipts"
        );
    }

    #[test]
    fn cell_sync_keeps_existing_continuation_and_empty_status() {
        for entries in [vec![], vec![activity(7)]] {
            let next = entries.last().map(|item| item.sequence);
            let expected =
                serde_json::to_value(compact_developer_view(view(entries.clone(), next))).unwrap();
            assert_eq!(visible(&result(view(entries, next)).unwrap()), expected);
        }
    }

    #[test]
    fn cell_sync_counts_json_escaping_and_rejects_an_unpageable_entry() {
        let mut entry = activity(1);
        entry.principal_id = "\"\\\n".repeat(900);
        let entries: Vec<_> = (1..=4)
            .map(|sequence| Activity {
                sequence,
                ..entry.clone()
            })
            .collect();
        let response = visible(&result(view(entries, None)).unwrap());
        let entries = response["activity"].as_array().unwrap();
        assert!(!entries.is_empty() && entries.len() < 4);
        assert_eq!(entries[0]["principal_id"], entry.principal_id);
        assert_eq!(
            response["next_sequence"],
            entries.last().unwrap()["sequence"]
        );

        entry.principal_id = "x".repeat(MAX_AGENT_RESPONSE_BYTES);
        let error = result(view(vec![entry], None)).unwrap_err();
        assert_eq!(error.data.unwrap()["code"], "agent_response_too_large");
    }
}
