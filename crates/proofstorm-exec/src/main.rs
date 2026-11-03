//! Linux supervisor installed by the controller, independent of target utilities.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(unix)]
mod workspace;

fn main() {
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("workspace") {
        // The task's own request failed: say why, so its script can be fixed.
        let Err(error) = workspace::entry(std::env::args().skip(2)) else {
            return;
        };
        eprintln!(
            "{}",
            serde_json::json!({"runner_error": "workspace_request_failed", "detail": error.to_string()})
        );
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    if linux::entry().is_ok() {
        return;
    }
    eprintln!("{{\"runner_error\":\"native_runner_failed\"}}");
    std::process::exit(1);
}
