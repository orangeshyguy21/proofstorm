//! Bounded invocation of the upstream wallet console command.
use super::{Failure, Result, fail};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

pub(super) struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub truncated: bool,
}
pub(super) trait WalletCli {
    async fn run(
        &self,
        home: &str,
        wallet: &str,
        mint: &str,
        args: &[&str],
        duration: Duration,
    ) -> Result<Output>;
}
pub(super) struct Native;
async fn drain(mut stream: impl AsyncRead + Unpin) -> std::io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut truncated = false;
    loop {
        let mut chunk = [0; 8192];
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok((retained, truncated));
        }
        let keep = count.min(crate::http::MAX_BODY.saturating_sub(retained.len()));
        retained.extend_from_slice(&chunk[..keep]);
        truncated |= keep < count;
    }
}
impl WalletCli for Native {
    async fn run(
        &self,
        home: &str,
        _wallet: &str,
        mint: &str,
        args: &[&str],
        duration: Duration,
    ) -> Result<Output> {
        let mut command = Command::new("cashu");
        command
            .args([
                "-h",
                mint,
                "-u",
                "sat",
                "-w",
                crate::wallet::NUTSHELL_WALLET_NAME,
                "-t",
                "-y",
            ])
            .args(args)
            .env("HOME", home)
            .env("CASHU_DIR", std::path::Path::new(home).join(".cashu"));
        execute(command, duration).await
    }
}

async fn execute(mut command: Command, duration: Duration) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| fail("wallet_cli_unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("wallet_cli_unavailable"))?;
    let outcome = timeout(duration, async {
        let (status, output) = tokio::join!(child.wait(), drain(stdout));
        Ok::<_, Failure>((status?, output?))
    })
    .await;
    if let Ok(value) = outcome {
        let (status, (stdout, truncated)) = value?;
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout,
            truncated,
        })
    } else {
        child
            .start_kill()
            .map_err(|_| fail("wallet_cli_cleanup_failed"))?;
        timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| fail("wallet_cli_cleanup_failed"))??;
        Ok(Output {
            code: 124,
            stdout: Vec::new(),
            truncated: false,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn large_output_is_drained_but_retention_is_bounded() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 2097152 /dev/zero; exit 7"]);
        let output = execute(command, Duration::from_secs(5)).await.unwrap();
        assert_eq!(output.code, 7);
        assert!(output.truncated);
        assert_eq!(output.stdout.len(), crate::http::MAX_BODY);
        assert!(output.stdout.iter().all(|byte| *byte == 0));
    }

    #[tokio::test]
    async fn timeout_kills_and_reaps_the_wallet_process() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut command = Command::new("sh");
        command.args(["-c", "echo $$ > \"$1\"; while :; do :; done", "fixture"]);
        command.arg(&pid_file);
        let started = Instant::now();
        let output = execute(command, Duration::from_millis(250)).await.unwrap();
        assert_eq!(output.code, 124);
        assert!(output.stdout.is_empty());
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let running = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!running.success(), "timed-out wallet process survived");
    }
}
