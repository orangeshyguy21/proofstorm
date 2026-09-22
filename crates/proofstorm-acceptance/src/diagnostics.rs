//! Public diagnostics contain fixed labels, source locations and typed statuses.
//! Native output, credentials and resource identities remain in private logs.
use std::{fs, path::Path};

use serde_json::{Value, json};

/// Extract code locations from the captured Rust backtrace, never from error
/// messages (which can contain credentials, proofs and native output).
fn failure_locations(backtrace: &str) -> Vec<String> {
    let mut locations = Vec::new();
    let mut acceptance_frame = false;
    for line in backtrace.lines() {
        let line = line.trim();
        if let Some((index, symbol)) = line.split_once(':')
            && index.parse::<u32>().is_ok()
        {
            let symbol = symbol.trim();
            acceptance_frame = symbol.starts_with("proofstorm_acceptance::")
                || symbol.starts_with("<proofstorm_acceptance::");
        }
        let location = line
            .split_once("crates/proofstorm-acceptance/src/")
            .map(|(_, path)| path)
            .or_else(|| {
                if acceptance_frame {
                    line.strip_prefix("at ./src/")
                        .or_else(|| line.strip_prefix("at src/"))
                } else {
                    None
                }
            });
        let Some(location) = location else {
            continue;
        };
        let location = location.trim();
        let Some((path, coordinates)) = location.split_once(".rs:") else {
            continue;
        };
        if path.split('/').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        }) || !coordinates
            .split(':')
            .all(|part| !part.is_empty() && part.parse::<u32>().is_ok())
        {
            continue;
        }
        let location = format!("crates/proofstorm-acceptance/src/{location}");
        if !locations.contains(&location) {
            locations.push(location);
        }
        if locations.len() == 8 {
            break;
        }
    }
    locations
}

fn native_failure(content: &Value) -> Value {
    let rpc = content["stdout"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let message = rpc.as_ref().and_then(|value| value["message"].as_str());
    let reason = match message {
        Some(message) if message.contains("Channel request rejected") => "channel-request-rejected",
        Some(message) if message.contains("Insufficient funds") => "insufficient-funds",
        _ => "native-command-failed",
    };
    json!({
        "reason":reason,
        "exit_code":content["exit_code"].as_i64(),
        "rpc_code":rpc.as_ref().and_then(|value| value["code"].as_i64()),
        "timed_out":content["timed_out"].as_bool(),
        "cancelled":content["cancelled"].as_bool(),
        "cleanup_verified":content["cleanup_verified"].as_bool(),
        "streams_complete":content["streams_complete"].as_bool(),
        "output_truncated":content["output_truncated"].as_bool()
    })
}

/// Terminal failure codes the daemon assigns from a closed set. Anything else
/// is a code this build does not know and is dropped rather than republished.
const OPERATION_CODES: [&str; 8] = [
    "action_cancelled",
    "action_deadline_exceeded",
    "action_failed",
    "action_job_lost",
    "action_runtime_not_found",
    "container_failed",
    "terminal_artifact_missing",
    "terminal_artifact_serialization_failed",
];

/// Termination reasons the kubelet assigns. `OOMKilled` is the one that
/// distinguishes an undersized container limit from a component that failed on
/// its own terms, so it has to survive into the public diagnostic.
const TERMINATION_REASONS: [&str; 5] = [
    "Completed",
    "ContainerStatusUnknown",
    "DeadlineExceeded",
    "Error",
    "OOMKilled",
];

/// A container name is a DNS label drawn from this project's own rendering, not
/// from any observed output.
fn container_name(value: &Value) -> Option<&str> {
    value.as_str().filter(|name| {
        !name.is_empty()
            && name.len() <= 63
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

/// Public evidence for an operation that reached a terminal failure. The
/// daemon's failure code, the terminated container and its Kubernetes exit
/// facts are closed-set values; the native error tail and wallet diagnostic
/// reason that may accompany them are not, and stay in the private log.
fn operation_failure(content: &Value) -> Value {
    let failure = &content["operation"]["artifact"]["content"];
    let code = failure["code"].as_str().unwrap_or_default();
    let reason = match code {
        "container_failed" => "operation-container-failed",
        "action_deadline_exceeded" => "operation-deadline-exceeded",
        "action_job_lost" | "action_runtime_not_found" => "operation-runtime-lost",
        _ => "operation-failed",
    };
    json!({
        "reason":reason,
        "code":OPERATION_CODES.contains(&code).then_some(code),
        "phase":content["phase"].as_str().filter(|phase| ["failed","cancelled"].contains(phase)),
        "container":container_name(&failure["container"]),
        "exit_code":failure["exit_code"].as_i64(),
        "termination_reason":failure["reason"].as_str()
            .filter(|reason| TERMINATION_REASONS.contains(reason))
    })
}

/// Public failure evidence identifies the failing assertion and native exit
/// status without publishing any part of an arbitrary error message.
pub(crate) fn gate_failure(error: &anyhow::Error) -> Value {
    let native = error.chain().find_map(|cause| {
        let message = cause.to_string();
        let content = message.strip_prefix("native command failed or has incomplete evidence: ")?;
        let content: Value = serde_json::from_str(content).ok()?;
        Some(native_failure(&content))
    });
    let operation = error.chain().find_map(|cause| {
        let message = cause.to_string();
        let content = message.strip_prefix("operation reached a terminal failure: ")?;
        let content: Value = serde_json::from_str(content).ok()?;
        Some(operation_failure(&content))
    });
    let reason = native
        .as_ref()
        .or(operation.as_ref())
        .and_then(|value| value["reason"].as_str())
        .unwrap_or_else(|| {
            for cause in error.chain() {
                let message = cause.to_string();
                if let Some(content) =
                    message.strip_prefix("cell readiness blocked or superseded: ")
                {
                    let status = serde_json::from_str::<Value>(content).unwrap_or_default();
                    if let Some(reason) = status["blockers"].as_array().and_then(|blockers| {
                        blockers.iter().find_map(|blocker| {
                            Some(match blocker["reason"].as_str()? {
                                "image_pull_failed" => "image-pull-failed",
                                "image_pull_backoff" => "image-pull-backoff",
                                "invalid_image_name" => "invalid-image-name",
                                "container_config_error" => "container-config-error",
                                "container_crash_loop" => "container-crash-loop",
                                "container_start_error" => "container-start-error",
                                "container_exited" => "container-exited",
                                "pod_unschedulable" => "pod-unschedulable",
                                _ => return None,
                            })
                        })
                    }) {
                        return reason;
                    }
                    return "cell-readiness-blocked";
                }
                if message.starts_with("native observation ") && message.contains(" timed out:") {
                    return "native-observation-timeout";
                }
            }
            "gate-failed"
        });
    json!({"reason":reason,"locations":failure_locations(&error.backtrace().to_string()),"native":native,"operation":operation})
}

fn text(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return None;
    }
    fs::read_to_string(path).ok()
}

pub(crate) fn resources(path: &Path) -> Value {
    let memory = fs::read_to_string("/proc/meminfo").ok().and_then(|info| {
        info.lines().find_map(|line| {
            let value = line
                .strip_prefix("MemAvailable:")?
                .split_whitespace()
                .next()?;
            value.parse::<u64>().ok()?.checked_mul(1024)
        })
    });
    let disk = nix::sys::statvfs::statvfs(path).ok().and_then(|info| {
        let bytes = u128::from(info.blocks_available()) * u128::from(info.fragment_size());
        u64::try_from(bytes).ok()
    });
    json!({"available_memory_bytes":memory,"available_disk_bytes":disk,
        "pressure":pressure(
            &fs::read_to_string("/proc/loadavg").unwrap_or_default(),
            &fs::read_to_string("/proc/vmstat").unwrap_or_default())})
}

fn pressure(load: &str, vmstat: &str) -> Value {
    let mut fields = load.split_whitespace();
    let load = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0);
    let tasks = fields.nth(2).and_then(|value| value.split_once('/'));
    let runnable = tasks.and_then(|(value, _)| value.parse::<u64>().ok());
    let total = tasks.and_then(|(_, value)| value.parse::<u64>().ok());
    let oom = vmstat
        .lines()
        .find_map(|line| line.strip_prefix("oom_kill "))
        .and_then(|value| value.trim().parse::<u64>().ok());
    json!({"load_1m":load,"runnable_tasks":runnable,"total_tasks":total,"oom_kills":oom})
}

fn setup_stage(work: &Path) -> &'static str {
    let value = text(&work.join("state/setup-progress.json"))
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    match value.as_ref().and_then(|value| value["stage"].as_str()) {
        Some("tools") => "setup-tools",
        Some("cluster") => "setup-cluster",
        Some("controller") => "setup-controller",
        Some("images") => "setup-images",
        Some("deployment") => "setup-deployment",
        Some("health") => "setup-health",
        Some("permissions") => "setup-permissions",
        _ => "setup-preflight",
    }
}

fn gate_stage(work: &Path) -> &'static str {
    let value = text(&work.join("qualification-stage.json"))
        .and_then(|text| serde_json::from_str::<String>(&text).ok());
    match value.as_deref() {
        Some("materialize") => "materialize",
        Some("cdk-materialize") => "cdk-materialize",
        Some("nutshell-materialize") => "nutshell-materialize",
        Some("cdk-replay") => "cdk-replay",
        Some("nutshell-replay") => "nutshell-replay",
        Some("configuration") => "configuration",
        Some("version") => "version",
        Some("peer-connect") => "peer-connect",
        Some("funding") => "funding",
        Some("bolt12-quote") => "bolt12-quote",
        Some("bolt12-payment") => "bolt12-payment",
        Some("issuance") => "issuance",
        Some("swap-and-melt") => "swap-and-melt",
        Some("restart") => "restart",
        Some("payment-after-restart") => "payment-after-restart",
        Some("teardown") => "teardown",
        _ => "gate",
    }
}

fn image_stage(work: &Path) -> &'static str {
    let result = text(&work.join("lightning/result.json"))
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let stage = result
        .as_ref()
        .and_then(|result| result["cases"].as_array())
        .and_then(|cases| cases.iter().find(|case| case["passed"] == false))
        .and_then(|case| case["last_stage"].as_str());
    match stage {
        Some("setup") => "lightning-setup",
        Some("bitcoin-startup") => "lightning-bitcoin-startup",
        Some("lightning-startup") => "lightning-startup",
        Some("funding") => "lightning-funding",
        Some("channel-ready") => "lightning-channel-ready",
        Some("payment") => "lightning-payment",
        Some("restart") => "lightning-restart",
        Some("mint-wallet-compatibility") => "lightning-mint-wallet",
        Some("complete") => "lightning-cleanup",
        _ => {
            let phase = text(&work.join("qualification-image-stage.json"))
                .and_then(|text| serde_json::from_str::<String>(&text).ok());
            match phase.as_deref() {
                Some("registry-manifest") => "registry-manifest",
                Some("registry-platform-manifest") => "registry-platform-manifest",
                Some("image-pull") => "image-pull",
                Some("image-inspection") => "image-inspection",
                Some("image-probe") => "image-probe",
                Some("lightning-compatibility") => "lightning-compatibility",
                _ => "images",
            }
        }
    }
}

pub(crate) fn progress(work: &Path, log: &Path, elapsed_seconds: u64) -> Value {
    let stage = match log.file_name().and_then(|name| name.to_str()) {
        Some("setup.log") => setup_stage(work),
        Some("image-qualification.log") => image_stage(work),
        _ => gate_stage(work),
    };
    json!({"stage":stage,"elapsed_seconds":elapsed_seconds,"resources":resources(work)})
}

pub(crate) fn setup_failure(work: &Path) -> Value {
    let log = text(&work.join("setup.log")).unwrap_or_default();
    let reason = if log.contains("supports macOS Apple Silicon and Linux")
        || log.contains("no bootstrap tools for this host")
    {
        "unsupported-host"
    } else if log.contains("Docker must run") {
        "docker-platform-mismatch"
    } else if log.contains("checksum mismatch") {
        "tool-checksum-mismatch"
    } else if log.contains("curl failed (") {
        "tool-download-failed"
    } else if log.contains("controller/client compatibility or source mismatch") {
        "controller-artifact-mismatch"
    } else if log.contains("docker failed (") {
        "docker-command-failed"
    } else if log.contains("no space left on device") || log.contains("No space left on device") {
        "disk-full"
    } else {
        "setup-failed"
    };
    json!({"stage":setup_stage(work),"reason":reason,"resources":resources(work)})
}

pub(crate) fn qualification_stage(
    work: &Path,
    report: &Value,
    passed: bool,
    cleanup: bool,
    preservation: bool,
) -> String {
    if passed {
        return if !cleanup {
            "cleanup"
        } else if !preservation {
            "preservation"
        } else {
            "complete"
        }
        .into();
    }
    match report["setup"].as_str() {
        Some("not_run" | "not_required") => image_stage(work).into(),
        Some("passed") => gate_stage(work).into(),
        _ => setup_stage(work).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_failure_publishes_status_without_output_or_resource_identity() {
        let content = json!({
            "exit_code":1,"timed_out":false,"cleanup_verified":true,
            "stdout":json!({"code":-1,"message":"They sent ERROR channel private-id: Channel request rejected","data":{"credential":"private-secret"}}).to_string(),
            "stderr":"private-key","pod":"private-pod"
        });
        let error = anyhow::anyhow!("native command failed or has incomplete evidence: {content}");
        let summary = gate_failure(&error);
        assert_eq!(summary["native"]["reason"], "channel-request-rejected");
        assert_eq!(summary["native"]["exit_code"], 1);
        assert_eq!(summary["native"]["rpc_code"], -1);
        assert_eq!(summary["native"]["cleanup_verified"], true);
        assert!(!summary.to_string().contains("private-"));
        if error.backtrace().status() == std::backtrace::BacktraceStatus::Captured {
            assert!(
                summary["locations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| {
                        value.as_str().is_some_and(|location| {
                            location.starts_with("crates/proofstorm-acceptance/src/diagnostics.rs:")
                        })
                    }),
                "captured backtrace: {}",
                error.backtrace()
            );
        }
        let arbitrary = gate_failure(&anyhow::anyhow!("private-secret"));
        assert_eq!(arbitrary["native"], Value::Null);
        assert!(!arbitrary.to_string().contains("private-secret"));
        assert_eq!(
            native_failure(&json!({"exit_code":"private-secret"}))["exit_code"],
            Value::Null
        );
    }

    #[test]
    fn failure_backtrace_retains_only_relative_rust_source_locations() {
        let frames = "  at /home/private-user/work/repo/crates/proofstorm-acceptance/src/native.rs:107:5\n  at crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:382:5\n  at /private/user/runtime.rs:1:1\n  at crates/proofstorm-acceptance/src/../../private.rs:1:1\n  at crates/proofstorm-acceptance/src/native.rs:private-secret";
        assert_eq!(
            failure_locations(frames),
            vec![
                "crates/proofstorm-acceptance/src/native.rs:107:5",
                "crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:382:5"
            ]
        );
        assert!(failure_locations("disabled backtrace").is_empty());
        assert_eq!(
            failure_locations(
                " 0: other_crate::run\n at ./src/secret.rs:1:2\n 1: proofstorm_acceptance::gates::run\n at ./src/gates/cdk_ldk.rs:40:5"
            ),
            vec!["crates/proofstorm-acceptance/src/gates/cdk_ldk.rs:40:5"]
        );
    }

    #[test]
    fn blocked_readiness_reports_a_fixed_reason_without_component_details() {
        let status = json!({"blockers":[{"reason":"container_crash_loop", "message":"private-secret", "component_id":"private-component"}]});
        let summary = gate_failure(&anyhow::anyhow!(
            "cell readiness blocked or superseded: {status}"
        ));
        assert_eq!(summary["reason"], "container-crash-loop");
        assert!(!summary.to_string().contains("private-"));
        for (reason, expected) in [
            ("image_pull_failed", "image-pull-failed"),
            ("image_pull_backoff", "image-pull-backoff"),
            ("invalid_image_name", "invalid-image-name"),
            ("container_config_error", "container-config-error"),
            ("container_start_error", "container-start-error"),
            ("container_exited", "container-exited"),
            ("pod_unschedulable", "pod-unschedulable"),
            ("private-secret", "cell-readiness-blocked"),
        ] {
            let status = json!({"blockers":[{"reason":reason,"component_id":"private-component","message":"private-secret"}]});
            let summary = gate_failure(&anyhow::anyhow!(
                "cell readiness blocked or superseded: {status}"
            ));
            assert_eq!(summary["reason"], expected);
            assert!(!summary.to_string().contains("private-"));
        }
        let summary = gate_failure(&anyhow::anyhow!(
            "native observation private-id timed out: private-output"
        ));
        assert_eq!(summary["reason"], "native-observation-timeout");
        assert!(!summary.to_string().contains("private-"));
    }

    #[test]
    fn terminal_operation_failures_publish_only_closed_set_runtime_facts() {
        let failure = |content: Value| {
            let message = json!({
                "operation_id":"private-operation",
                "phase":"failed",
                "operation":{"artifact":{"content":content},"run_id":"private-run"}
            });
            gate_failure(&anyhow::anyhow!(
                "operation reached a terminal failure: {message}"
            ))
        };
        let summary = failure(json!({
            "code":"container_failed", "container":"component", "exit_code":137,
            "reason":"OOMKilled", "native_error_tail":"private-secret",
            "diagnostic_reason":"private-secret"
        }));
        assert_eq!(summary["reason"], "operation-container-failed");
        assert_eq!(summary["operation"]["code"], "container_failed");
        assert_eq!(summary["operation"]["container"], "component");
        assert_eq!(summary["operation"]["exit_code"], 137);
        assert_eq!(summary["operation"]["termination_reason"], "OOMKilled");
        assert_eq!(summary["operation"]["phase"], "failed");
        assert!(!summary.to_string().contains("private-"));
        for (code, expected) in [
            ("action_deadline_exceeded", "operation-deadline-exceeded"),
            ("action_job_lost", "operation-runtime-lost"),
            ("action_runtime_not_found", "operation-runtime-lost"),
            ("action_failed", "operation-failed"),
            ("private-secret", "operation-failed"),
        ] {
            let summary = failure(json!({"code":code}));
            assert_eq!(summary["reason"], expected);
            assert!(!summary.to_string().contains("private-"));
        }
        // Anything outside the closed sets is dropped rather than republished.
        let summary = failure(json!({
            "code":"container_failed", "container":"PRIVATE Secret!", "exit_code":"private-secret",
            "reason":"private-secret"
        }));
        assert_eq!(summary["operation"]["container"], Value::Null);
        assert_eq!(summary["operation"]["exit_code"], Value::Null);
        assert_eq!(summary["operation"]["termination_reason"], Value::Null);
        assert!(!summary.to_string().contains("private-"));
        // A native receipt still classifies as a native failure.
        let summary = gate_failure(&anyhow::anyhow!(
            "native command failed or has incomplete evidence: {}",
            json!({"exit_code":3,"stdout":"private-secret"})
        ));
        assert_eq!(summary["reason"], "native-command-failed");
        assert_eq!(summary["operation"], Value::Null);
    }

    #[test]
    fn host_pressure_retains_only_numeric_counts_and_handles_unavailable_linux_files() {
        assert_eq!(
            pressure("2.50 3.0 4.0 12/345 99999", "other 10\noom_kill 7\n"),
            json!({"load_1m":2.5,"runnable_tasks":12,"total_tasks":345,"oom_kills":7})
        );
        for input in [
            "",
            "private credential",
            "NaN 0 0 private/credential",
            "-1 0 0 -1/-2",
        ] {
            assert_eq!(
                pressure(input, "oom_kill private credential"),
                json!({"load_1m":null,"runnable_tasks":null,"total_tasks":null,"oom_kills":null})
            );
        }
    }

    #[test]
    fn heartbeat_reports_the_current_stage_without_copying_private_output() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("state")).unwrap();
        fs::write(
            root.path().join("state/setup-progress.json"),
            r#"{"stage":"tools"}"#,
        )
        .unwrap();
        let setup = root.path().join("setup.log");
        assert_eq!(progress(root.path(), &setup, 30)["stage"], "setup-tools");
        let log = root.path().join("gate-0-qualification.log");
        fs::write(root.path().join("qualification-stage.json"), r#""funding""#).unwrap();
        let value = progress(root.path(), &log, 120);
        assert_eq!(value["stage"], "funding");
        assert_eq!(value["elapsed_seconds"], 120);
        fs::write(
            root.path().join("qualification-stage.json"),
            r#""private-credential""#,
        )
        .unwrap();
        assert_eq!(progress(root.path(), &log, 121)["stage"], "gate");
    }

    #[test]
    fn embedded_ldk_steps_remain_visible_in_failure_receipts() {
        let root = tempfile::tempdir().unwrap();
        for stage in [
            "materialize",
            "configuration",
            "version",
            "peer-connect",
            "funding",
            "bolt12-quote",
            "bolt12-payment",
            "issuance",
            "swap-and-melt",
            "restart",
            "payment-after-restart",
            "teardown",
        ] {
            fs::write(
                root.path().join("qualification-stage.json"),
                json!(stage).to_string(),
            )
            .unwrap();
            assert_eq!(
                qualification_stage(root.path(), &json!({"setup":"passed"}), false, true, true),
                stage
            );
        }
    }

    #[test]
    fn standalone_lightning_failures_report_only_allowlisted_behavioral_stages() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("lightning")).unwrap();
        for (stage, expected) in [
            ("payment", "lightning-payment"),
            ("restart", "lightning-restart"),
            ("private-secret", "images"),
        ] {
            fs::write(root.path().join("lightning/result.json"), json!({
                "cases":[{"passed":true,"last_stage":"complete"},{"passed":false,"last_stage":stage,"stderr":"private-secret"}]
            }).to_string()).unwrap();
            assert_eq!(
                qualification_stage(
                    root.path(),
                    &json!({"setup":"not_required"}),
                    false,
                    true,
                    true
                ),
                expected
            );
        }
    }

    #[test]
    fn image_failures_distinguish_registry_transfer_and_probe_without_private_text() {
        let root = tempfile::tempdir().unwrap();
        for stage in [
            "registry-manifest",
            "registry-platform-manifest",
            "image-pull",
            "image-inspection",
            "image-probe",
            "lightning-compatibility",
            "private-secret",
        ] {
            fs::write(
                root.path().join("qualification-image-stage.json"),
                json!(stage).to_string(),
            )
            .unwrap();
            let expected = if stage == "private-secret" {
                "images"
            } else {
                stage
            };
            assert_eq!(
                qualification_stage(root.path(), &json!({"setup":"not_run"}), false, false, true),
                expected
            );
        }
    }

    #[test]
    fn completed_scenario_distinguishes_cleanup_and_preservation_failure() {
        let root = tempfile::tempdir().unwrap();
        let report = json!({"setup":"not_required"});
        assert_eq!(
            qualification_stage(root.path(), &report, true, true, false),
            "preservation"
        );
        assert_eq!(
            qualification_stage(root.path(), &report, true, false, true),
            "cleanup"
        );
        assert_eq!(
            qualification_stage(root.path(), &report, true, true, true),
            "complete"
        );
        assert_eq!(
            qualification_stage(root.path(), &report, false, false, true),
            "images"
        );
    }

    #[test]
    fn setup_diagnostics_never_copy_private_output_or_unrecognized_stage_labels() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("state")).unwrap();
        fs::write(
            root.path().join("setup.log"),
            "Error: this alpha supports macOS Apple Silicon and Linux x86-64\nprivate credential",
        )
        .unwrap();
        fs::write(
            root.path().join("state/setup-progress.json"),
            r#"{"stage":"private credential"}"#,
        )
        .unwrap();
        let summary = setup_failure(root.path());
        assert_eq!(summary["reason"], "unsupported-host");
        assert_eq!(summary["stage"], "setup-preflight");
        assert!(!summary.to_string().contains("private credential"));
        fs::write(
            root.path().join("state/setup-progress.json"),
            r#"{"stage":"tools"}"#,
        )
        .unwrap();
        assert_eq!(
            qualification_stage(root.path(), &json!({"setup":"failed"}), false, false, true),
            "setup-tools"
        );
    }
}
