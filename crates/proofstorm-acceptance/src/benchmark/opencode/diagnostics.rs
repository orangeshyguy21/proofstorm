//! Observational diagnostics: never use a connectivity probe to invent a cause.
use super::{Context, read, save};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn controls(profile: &Value) -> Result<Vec<String>> {
    let Some(config) = profile.get("diagnostics") else {
        return Ok(Vec::new());
    };
    ensure!(
        config
            .as_object()
            .is_some_and(|v| v.keys().all(|k| k == "control_origins")),
        "diagnostics accepts only control_origins"
    );
    let entries = config["control_origins"]
        .as_array()
        .context("control_origins must be an array")?;
    ensure!(
        entries.len() <= 2,
        "at most two independent control origins"
    );
    entries
        .iter()
        .map(|entry| {
            let url = url::Url::parse(entry.as_str().context("control origin must be a URL")?)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path() == "/",
                "controls must be credential-free HTTPS origins without paths"
            );
            Ok(url.origin().ascii_serialization())
        })
        .collect()
}

pub(super) fn install(context: &Context, profile: &Value) -> Result<PathBuf> {
    let root = context.work.join("opencode-home/transport-capture");
    fs::create_dir(&root)?;
    let provider = context
        .model
        .split_once('/')
        .context("provider/model identity")?
        .0;
    let settings = json!({"work":context.work,"provider_id":provider,"control_origins":controls(profile)?,
        "body_limit_bytes":1024*1024,"log_limit_bytes":64*1024*1024});
    save(&root.join("settings.json"), &settings)?;
    save(
        &root.join("package.json"),
        &json!({"name":"proofstorm-transport-capture","type":"module","exports":"./index.ts"}),
    )?;
    fs::write(root.join("index.ts"), include_str!("capture-plugin.ts"))?;
    fs::write(root.join("capture.mjs"), include_str!("capture.mjs"))?;
    save(
        &context.work.join("transport-capture-config.json"),
        &json!({
            "format_version":1,"http_hooks":true,"control_origins":settings["control_origins"],
            "body_limit_bytes":settings["body_limit_bytes"],"log_limit_bytes":settings["log_limit_bytes"],
            "probe_interval_seconds":60,"probe_minimum_interval_seconds":30,
            "request_headers":"allowlisted only","body_capture":"bounded private evidence; JSON credential keys and known HTTP credentials redacted; SSE model content remains private",
            "network_attribution":"separate DNS/TCP/TLS observations, never proof of inference-path root cause",
        }),
    )?;
    Ok(root.canonicalize()?)
}

pub(super) fn summarize(work: &Path) -> Result<()> {
    let text = fs::read_to_string(work.join("transport.private.jsonl")).unwrap_or_default();
    let mut kinds = BTreeMap::<String, usize>::new();
    let mut observations = Vec::new();
    let mut malformed = 0;
    for line in text.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            malformed += 1;
            continue;
        };
        let kind = row["type"].as_str().unwrap_or("unknown");
        *kinds.entry(kind.into()).or_default() += 1;
        let attribution = match kind {
            "provider_error_payload" => Some("provider_reported_error"),
            "response_headers" if row["status"].as_u64().is_some_and(|s| s >= 400) => {
                Some("http_error_response_received")
            }
            "stream_error" => Some("transport_failure_origin_undetermined"),
            "request_signal_abort" | "consumer_cancel" => Some("client_cancellation_observed"),
            "correlation_gap" | "capture_error" => Some("local_capture_gap"),
            _ => None,
        };
        if let Some(attribution) = attribution {
            observations.push(json!({"observation":attribution,"request_id":row["request_id"],"at_unix_ms":row["at_unix_ms"],
                "status":row["status"],"response_id":row["response_id"],"source_event":kind}));
        }
    }
    let attempt = read(&work.join("benchmark-attempt.json")).unwrap_or(Value::Null);
    let status = read(&work.join("transport-capture-status.json")).unwrap_or(Value::Null);
    let capture_lifecycle_complete = !text.is_empty()
        && malformed == 0
        && status["closed"] == true
        && status["drain_complete"] == true
        && status["write_errors"] == 0
        && status["dropped_records"] == 0
        && status["pending_requests"]
            .as_array()
            .is_some_and(Vec::is_empty);
    let (rows, transcript_complete) =
        super::transcript(work, attempt["session_export_required"] == true);
    let task = read(&work.join("benchmark-task.json"))
        .ok()
        .and_then(|v| serde_json::from_value(v).ok());
    let captured = super::super::events(work).and_then(|rows| super::super::calls(&rows));
    let mcp_read_error = captured.as_ref().err().map(ToString::to_string);
    let audit = super::telemetry::audit(captured.unwrap_or_default(), &rows, task.as_ref());
    save(&work.join("telemetry-reconciliation.private.json"), &audit)?;
    let reconciliation_complete = mcp_read_error.is_none()
        && transcript_complete
        && audit["unmatched_proxy_call_ids"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && audit["unmatched_harness_calls"]
            .as_array()
            .is_some_and(Vec::is_empty);
    save(
        &work.join("failure-diagnostics.json"),
        &json!({
            "format_version":1,"local_termination":attempt["outcome"],"observations":observations,"event_counts":kinds,
            "malformed_capture_records":malformed,"capture_status":status,
            "capture_missing":text.is_empty(),
            "capture_lifecycle_complete_within_declared_limits":capture_lifecycle_complete,
            "client_error_event_count":rows.iter().filter(|v| v["type"] == "error").count(),
            "telemetry":{"exact_reconciliation_complete":reconciliation_complete,"transcript_complete":transcript_complete,
                "mcp_read_error":mcp_read_error,"unmatched_proxy_call_ids":audit["unmatched_proxy_call_ids"],
                "unmatched_harness_call_count":audit["unmatched_harness_calls"].as_array().map(Vec::len),
                "detail":"telemetry-reconciliation.private.json","affects_scoring_rules":false},
            "session_export_present":work.join("opencode-session.private.json").is_file(),
            "stop":read(&work.join("opencode-stop.json")).unwrap_or(Value::Null),
            "root_cause":"not_inferred_from_transport_errors_or_probes",
            "interpretation":["HTTP/SSE error responses prove an error was received, not which provider/backend caused it.",
                "A failed DNS/TCP/TLS probe is supporting evidence only; controls and inference may use different routes.",
                "Socket resets and missing responses remain undetermined without correlated upstream traces.",
                "Local termination and client cancellation are recorded separately from provider errors.",
                "Missing/unfinished capture is not evidence that the network or provider was healthy."],
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probes_do_not_turn_an_ambiguous_reset_into_an_upstream_or_local_verdict() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::write(
            dir.path().join("transport.private.jsonl"),
            concat!(
                "{\"type\":\"stream_error\",\"request_id\":\"r1\"}\n",
                "{\"type\":\"network_sample\",\"observations\":[{\"dns\":{\"ok\":false}}]}\n",
                "{\"type\":\"provider_error_payload\",\"request_id\":\"r2\",\"response_id\":\"upstream-id\"}\n"
            ),
        )?;
        summarize(dir.path())?;
        let result = read(&dir.path().join("failure-diagnostics.json"))?;
        assert_eq!(
            result["observations"][0]["observation"],
            "transport_failure_origin_undetermined"
        );
        assert_eq!(
            result["observations"][1]["observation"],
            "provider_reported_error"
        );
        assert_eq!(result["observations"].as_array().unwrap().len(), 2);
        assert!(result["capture_status"].is_null());
        Ok(())
    }
    #[test]
    fn control_origins_are_explicit_bounded_and_credential_free() -> Result<()> {
        assert!(controls(&json!({}))?.is_empty());
        assert_eq!(
            controls(&json!({"diagnostics":{"control_origins":["https://example.com"]}}))?,
            ["https://example.com"]
        );
        for input in [
            "https://user:secret@example.com",
            "https://example.com/key",
            "https://example.com/?key=secret",
            "http://example.com",
        ] {
            assert!(controls(&json!({"diagnostics":{"control_origins":[input]}})).is_err());
        }
        Ok(())
    }
}
