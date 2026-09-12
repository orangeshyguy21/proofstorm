//! Bounded private captures shared by onboarding, GUI and agent checks.
use anyhow::{Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

pub fn capture(mut command: Command, seconds: u64) -> Result<Output> {
    let mut out = tempfile::tempfile()?;
    let mut err = tempfile::tempfile()?;
    // Stay in the worker's group so its parent deadline also reaps our children.
    let mut child = command
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(err.try_clone()?)
        .spawn()?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(seconds)
            || out.metadata()?.len() > 8 * 1024 * 1024
            || err.metadata()?.len() > 8 * 1024 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("acceptance command exceeded time/output limit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut streams = [Vec::new(), Vec::new()];
    read(&mut out, &mut streams[0])?;
    read(&mut err, &mut streams[1])?;
    let [stdout, stderr] = streams;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read(file: &mut std::fs::File, bytes: &mut Vec<u8>) -> Result<()> {
    ensure!(
        file.metadata()?.len() <= 8 * 1024 * 1024,
        "acceptance output too large"
    );
    file.seek(SeekFrom::Start(0))?;
    file.take(8 * 1024 * 1024).read_to_end(bytes)?;
    Ok(())
}

pub fn json(command: Command, seconds: u64) -> Result<serde_json::Value> {
    let output = capture(command, seconds)?;
    ensure!(
        output.status.success(),
        "acceptance CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_separate_streams_and_kills_timed_out_children() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf stdout; printf stderr >&2; exit 7"]);
        let output = capture(command, 5).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
        let mut command = Command::new("sleep");
        command.arg("30");
        assert!(capture(command, 0).is_err());
    }
}
