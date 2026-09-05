//! Linux supervisor installed by the controller, independent of target utilities.
#[cfg(target_os = "linux")]
mod linux;

fn main() {
    #[cfg(target_os = "linux")]
    if linux::entry().is_ok() {
        return;
    }
    eprintln!("{{\"runner_error\":\"native_runner_failed\"}}");
    std::process::exit(1);
}
