//! Bounded child execution with cancellation and owned process-group cleanup.
use anyhow::{Context, Result, ensure};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    io::{Read, Seek, SeekFrom},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::sync::watch;

pub struct Cancellation {
    receiver: watch::Receiver<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Cancellation {
    pub fn new() -> Result<Self> {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let (sender, receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            let _ = sender.send(true);
        });
        Ok(Self { receiver, task })
    }
    pub async fn cancelled(&mut self) {
        while !*self.receiver.borrow() {
            if self.receiver.changed().await.is_err() {
                return;
            }
        }
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub failure: Option<&'static str>,
}

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Also runs when inspection fails or this future is dropped.
        if let Ok(id) = i32::try_from(self.0.id()) {
            let _ = killpg(Pid::from_raw(id), Signal::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

pub async fn run(
    mut command: Command,
    timeout: Duration,
    cancel: Option<&mut Cancellation>,
) -> Result<Output> {
    use std::os::unix::process::CommandExt;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .process_group(0);
    let mut child = OwnedChild(command.spawn().context("start update subprocess")?);
    let group = Pid::from_raw(i32::try_from(child.0.id())?);
    let deadline = tokio::time::Instant::now() + timeout;
    let cancelled = async {
        if let Some(cancel) = cancel {
            cancel.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(cancelled);
    let (status, failure) = loop {
        if stdout.metadata()?.len() > 262_144 || stderr.metadata()?.len() > 262_144 {
            break (None, Some("output_limit"));
        }
        if let Some(status) = child.0.try_wait()? {
            break (Some(status), None);
        }
        tokio::select! {
            () = &mut cancelled => break (None, Some("cancelled")),
            () = tokio::time::sleep_until(deadline) => break (None, Some("timeout")),
            () = tokio::time::sleep(Duration::from_millis(25)) => {},
        }
    };
    // Also collect descendants when a script exits before its children.
    if killpg(group, Signal::SIGTERM).is_ok() {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = killpg(group, Signal::SIGKILL);
    }
    let status = match status {
        Some(status) => status,
        None => child.0.wait()?,
    };
    let read = |file: &mut std::fs::File| -> Result<String> {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(262_144).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    Ok(Output {
        success: status.success() && failure.is_none(),
        stdout: read(&mut stdout)?,
        stderr: read(&mut stderr)?,
        failure,
    })
}

pub async fn metadata(executable: &std::path::Path, mcp: bool) -> Result<serde_json::Value> {
    let mut command = Command::new(executable);
    if mcp {
        command.arg("--release-info");
    } else {
        command.args(["version", "--json"]);
    }
    let output = run(command, Duration::from_secs(15), None).await?;
    ensure!(
        output.success,
        "installed metadata command failed: {}",
        output.stderr
    );
    Ok(serde_json::from_str(&output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_kills_the_installer_and_its_children() {
        let (sender, receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            sender.send(true).unwrap();
        });
        let mut cancellation = Cancellation { receiver, task };
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("late-write");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "(sleep 1; touch \"$1\") & wait", "fixture"])
            .arg(&marker);
        let result = run(command, Duration::from_secs(10), Some(&mut cancellation))
            .await
            .unwrap();
        assert_eq!(result.failure, Some("cancelled"));
        assert!(!result.success);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
    }
}
