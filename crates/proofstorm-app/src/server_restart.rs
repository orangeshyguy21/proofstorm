//! Development server replacement. Never signal a process based on its port alone.
use anyhow::{Context, Result, bail, ensure};
use std::{net::TcpListener, path::Path, process::Command, time::Duration};

pub async fn stop_previous(port: u16) -> Result<()> {
    if port == 0 || TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
        return Ok(());
    }
    let checkout = std::env::current_dir()?.canonicalize()?;
    let listeners = Command::new("lsof")
        .args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
        .context("identify the existing server (lsof is required for --replace)")?;
    let pids = String::from_utf8(listeners.stdout)?;
    let pids = pids.lines().collect::<std::collections::BTreeSet<_>>();
    ensure!(
        !pids.is_empty(),
        "Port {port} is unavailable; could not identify its owner. No process was stopped."
    );
    // Validate every owner before stopping any of them (IPv4/IPv6 may have different owners).
    for pid in &pids {
        pid.parse::<u32>().context("invalid listener PID")?;
        let cwd = Command::new("lsof")
            .args(["-a", "-p", pid, "-d", "cwd", "-Fn"])
            .output()?;
        let args = Command::new("ps")
            .args(["-p", pid, "-o", "args="])
            .output()?;
        ensure!(
            cwd.status.success()
                && args.status.success()
                && owns_server(
                    &checkout,
                    &String::from_utf8(cwd.stdout)?,
                    &String::from_utf8(args.stdout)?
                ),
            "Port {port} is used by process {pid}, which is not a Proofstorm server from this checkout. No process was stopped. Choose PORT=<another port>."
        );
    }
    for pid in pids {
        eprintln!("Restarting Proofstorm server {pid} on port {port}");
        let status = Command::new("kill").args(["-TERM", pid]).status()?;
        ensure!(status.success(), "could not stop Proofstorm server {pid}");
    }
    for _ in 0..50 {
        if TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("The previous server has not released port {port} after 5 seconds; retry just serve.")
}

fn owns_server(checkout: &Path, cwd: &str, args: &str) -> bool {
    if !cwd.lines().any(|line| {
        line.strip_prefix('n')
            .is_some_and(|path| Path::new(path) == checkout)
    }) {
        return false;
    }
    ["target/debug/proofstorm", "target/release/proofstorm"]
        .iter()
        .flat_map(|relative| {
            [
                (*relative).to_owned(),
                format!("./{relative}"),
                checkout.join(relative).to_string_lossy().into_owned(),
            ]
        })
        .any(|executable| {
            args.trim()
                .strip_prefix(&format!("{executable} serve"))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_requires_same_checkout_and_server_command() {
        let checkout = Path::new("/tmp/my checkout");
        let cwd = "p123\nn/tmp/my checkout\n";
        for executable in [
            "target/debug/proofstorm",
            "./target/release/proofstorm",
            "/tmp/my checkout/target/debug/proofstorm",
        ] {
            assert!(owns_server(
                checkout,
                cwd,
                &format!("{executable} serve --port 8787\n")
            ));
        }
        for command in [
            "python3 -m http.server 8787",
            "target/debug/proofstorm environment",
            "target/debug/proofstorm serve-other",
            "target/debug/proofstorm-mcp serve",
            "/tmp/other/target/debug/proofstorm serve",
        ] {
            assert!(!owns_server(checkout, cwd, command));
        }
        assert!(!owns_server(
            checkout,
            "p123\nn/tmp/other\n",
            "target/debug/proofstorm serve"
        ));
        assert!(!owns_server(checkout, "", "target/debug/proofstorm serve"));
    }
}
