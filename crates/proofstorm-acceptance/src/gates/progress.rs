//! Real setup/GUI terminal output, separate from HTTP and visual acceptance.
use crate::{GateContext, process};
use anyhow::{Result, ensure};
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    pty::openpty,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::fd::AsRawFd,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn terminal(mut command: Command) -> Result<(String, Value)> {
    let pty = openpty(None, None)?;
    fcntl(pty.master.as_raw_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    let mut master = File::from(pty.master);
    let mut output = tempfile::tempfile()?;
    command
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(File::from(pty.slave));
    let mut child = command.spawn()?;
    drop(command);
    let started = Instant::now();
    let mut first = None;
    let mut progress = Vec::new();
    let outcome = (|| -> Result<()> {
        loop {
            let mut bytes = [0; 8192];
            match master.read(&mut bytes) {
                Ok(n) if n > 0 => {
                    first.get_or_insert(started.elapsed());
                    progress.extend_from_slice(&bytes[..n]);
                }
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(5) => {}
                Err(error) => return Err(error.into()),
            }
            ensure!(
                progress.len() < 8 * 1024 * 1024,
                "terminal output exceeded limit"
            );
            if let Some(status) = child.try_wait()? {
                while let Ok(n) = master.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    progress.extend_from_slice(&bytes[..n]);
                }
                ensure!(
                    status.success(),
                    "terminal command failed: {}",
                    String::from_utf8_lossy(&progress)
                );
                return Ok(());
            }
            ensure!(
                started.elapsed() < Duration::from_secs(420),
                "terminal command timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if outcome.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    outcome?;
    ensure!(
        first.is_some_and(|time| time < Duration::from_secs(5)),
        "initial progress was delayed"
    );
    let progress = String::from_utf8(progress)?;
    let frames: Vec<_> = progress
        .split('\r')
        .filter(|line| {
            line.as_bytes()
                .first()
                .is_some_and(|c| b"|/\\-".contains(c))
                && line.as_bytes().get(1) == Some(&b' ')
        })
        .collect();
    ensure!(
        frames.len() >= 2 && progress.ends_with('\r'),
        "spinner did not animate/clear"
    );
    ensure!(
        !progress.contains('\x1b') && !progress.contains("s)"),
        "escape codes or elapsed timer in progress"
    );
    output.seek(SeekFrom::Start(0))?;
    let mut result = String::new();
    output.read_to_string(&mut result)?;
    ensure!(
        !result.contains('{'),
        "default output is not human-readable"
    );
    Ok((
        result,
        json!({"first_progress_ms":first.unwrap().as_millis(),"frames":frames.len(),"line_cleared":true,"no_elapsed_timer":true}),
    ))
}

pub fn run(context: &GateContext) -> Result<()> {
    ensure!(
        !context.installation.home.join("gui-process.json").exists(),
        "owned GUI must be stopped"
    );
    let before = super::onboarding::runtime(context)?;
    let result = (|| -> Result<()> {
        let (setup, setup_progress) =
            terminal(context.command(&["setup", "--allow-development"])?)?;
        ensure!(setup.contains("Runtime ready"), "setup summary missing");
        let (gui, gui_progress) =
            terminal(context.command(&["gui", "start", "--allow-development"])?)?;
        ensure!(
            gui.contains("GUI ready:") && gui.contains("gui stop"),
            "GUI summary missing"
        );
        let (reuse, reuse_progress) =
            terminal(context.command(&["gui", "start", "--allow-development"])?)?;
        ensure!(reuse.contains("GUI ready:"), "reuse summary missing");
        for (args, key) in [
            (&["setup", "--allow-development"][..], "ready"),
            (
                &["gui", "start", "--allow-development"][..],
                "reused_server",
            ),
            (&["doctor"][..], "ok"),
        ] {
            let mut command = context.command(args)?;
            command.arg("--json");
            let output = process::capture(command, 420)?;
            ensure!(
                output.status.success()
                    && serde_json::from_slice::<Value>(&output.stdout)?[key] == true,
                "JSON result failed"
            );
            ensure!(
                !output.stderr.contains(&b'\r') && !output.stderr.contains(&0x1b),
                "progress leaked into JSON mode"
            );
        }
        context.record("cli-progress.json", &json!({"passed":true,"setup":setup_progress,"gui":gui_progress,"reuse":reuse_progress,"json_results_parse":true}))
    })();
    context.cli(&["gui", "stop"])?;
    result?;
    ensure!(
        before == super::onboarding::runtime(context)?,
        "progress check changed controller/cells"
    );
    Ok(())
}
