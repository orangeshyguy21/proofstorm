//! Public diagnostics contain only fixed labels and numeric capacity readings.
//! Native output, credentials and resource identities remain in private logs.
use std::{fs, path::Path};

use serde_json::{Value, json};

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
        Some("funding") => "funding",
        Some("issuance") => "issuance",
        Some("swap-and-melt") => "swap-and-melt",
        Some("restart") => "restart",
        Some("payment-after-restart") => "payment-after-restart",
        _ => "gate",
    }
}

pub(crate) fn progress(work: &Path, log: &Path, elapsed_seconds: u64) -> Value {
    let stage = match log.file_name().and_then(|name| name.to_str()) {
        Some("setup.log") => setup_stage(work),
        Some("image-qualification.log") => "images",
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
        Some("not_run" | "not_required") => "images".into(),
        Some("passed") => gate_stage(work).into(),
        _ => setup_stage(work).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
