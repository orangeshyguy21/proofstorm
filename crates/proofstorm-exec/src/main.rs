//! Linux supervisor installed by the controller, independent of target utilities.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(unix)]
mod workspace;

fn main() {
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("workspace") {
        if workspace::entry(std::env::args().skip(2)).is_ok() {
            return;
        }
        eprintln!("{{\"runner_error\":\"workspace_request_failed\"}}");
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    if linux::entry().is_ok() {
        return;
    }
    eprintln!("{{\"runner_error\":\"native_runner_failed\"}}");
    std::process::exit(1);
}
