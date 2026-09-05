use nix::{
    errno::Errno,
    sys::{
        prctl::set_child_subreaper,
        signal::{SigSet, SigmaskHow, Signal, kill, pthread_sigmask},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::Pid,
};
use proofstorm_core::native::{NativeCommand, OutputMode, project_receipt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const CAPTURE_LIMIT: usize = 16 * 1024;

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn finish(directory: &Path, receipt: &Value) -> Result<()> {
    write_private(
        &directory.join("receipt.tmp"),
        &serde_json::to_vec(receipt)?,
    )?;
    fs::rename(
        directory.join("receipt.tmp"),
        directory.join("receipt.json"),
    )?;
    Ok(())
}

fn spec(directory: &Path) -> Result<NativeCommand> {
    let command: NativeCommand =
        serde_json::from_slice(&fs::read(directory.join("request.json"))?)?;
    command.validate()?;
    Ok(command)
}

pub fn entry() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("mode missing")?;
    let directory = PathBuf::from(args.next().ok_or("directory missing")?);
    match mode.as_str() {
        "start" => {
            let mut bytes = Vec::new();
            std::io::stdin().take(65537).read_to_end(&mut bytes)?;
            if bytes.len() > 65536 {
                return Err("request too large".into());
            }
            let command: NativeCommand = serde_json::from_slice(&bytes)?;
            command.validate()?;
            // create_new is an additional local replay fence.
            write_private(&directory.join("request.json"), &bytes)?;
            Command::new(std::env::current_exe()?)
                .arg("run")
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()?;
            println!("{{\"started\":true}}");
        }
        "run" => {
            if supervise(&directory).is_err() {
                finish(
                    &directory,
                    &json!({"runner_error":"native_runner_failed", "cleanup_verified":false}),
                )?;
            }
        }
        "child" => {
            let command = spec(&directory)?;
            pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&SigSet::empty()), None)?;
            let argv = if command.argv.is_empty() {
                vec!["/bin/sh".into(), "-c".into(), command.script]
            } else {
                command.argv
            };
            let error = Command::new(&argv[0]).args(&argv[1..]).exec();
            return Err(error.into());
        }
        "status" => match fs::read(directory.join("receipt.json")) {
            Ok(bytes) => std::io::stdout().write_all(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                println!("{{\"running\":true}}")
            }
            Err(error) => return Err(error.into()),
        },
        "cancel" => {
            match write_private(&directory.join("cancel"), b"cancel") {
                Ok(()) => (),
                Err(_) if directory.join("cancel").is_file() => (),
                Err(error) => return Err(error),
            }
            println!("{{\"cancel_requested\":true}}");
        }
        _ => return Err("unknown mode".into()),
    }
    Ok(())
}

fn capture(
    mut reader: impl Read + Send + 'static,
    path: PathBuf,
) -> mpsc::Receiver<(Vec<u8>, usize)> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| -> Result<_> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            let mut retained = Vec::new();
            let mut total = 0_usize;
            let mut buffer = [0; 4096];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                total = total.saturating_add(count);
                let keep = count.min(CAPTURE_LIMIT.saturating_sub(retained.len()));
                file.write_all(&buffer[..keep])?;
                retained.extend_from_slice(&buffer[..keep]);
            }
            file.sync_all()?;
            Ok((retained, total))
        })();
        if let Ok(value) = result {
            let _ = sender.send(value);
        }
    });
    receiver
}

// Only signal our direct children: they cannot be PID-reused until we reap them.
// Subreaping adopts descendants even when they use nohup, double-fork or setsid.
fn signal_children(signal: Signal) -> Result<()> {
    let pid = std::process::id();
    let children = fs::read_to_string(format!("/proc/self/task/{pid}/children"))?;
    for pid in children.split_whitespace() {
        match kill(Pid::from_raw(pid.parse()?), signal) {
            Ok(()) | Err(Errno::ESRCH) => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "supervision keeps process ownership, cleanup and receipt construction together"
)]
fn supervise(directory: &Path) -> Result<()> {
    let command = spec(directory)?;
    set_child_subreaper(true)?;
    let mut signals = SigSet::empty();
    signals.add(Signal::SIGTERM);
    signals.add(Signal::SIGINT);
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), None)?;
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal_flag = interrupted.clone();
    thread::spawn(move || {
        if signals.wait().is_ok() {
            signal_flag.store(true, Ordering::SeqCst);
        }
    });
    let started = Instant::now();
    let mut child = Command::new(std::env::current_exe()?)
        .arg("child")
        .arg(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let main_pid = Pid::from_raw(i32::try_from(child.id())?);
    let stdout = capture(
        child.stdout.take().ok_or("stdout unavailable")?,
        directory.join("stdout"),
    );
    let stderr = capture(
        child.stderr.take().ok_or("stderr unavailable")?,
        directory.join("stderr"),
    );
    let mut exit_code = None;
    let mut exit_signal = None;
    let mut main_finished = false;
    let mut cleanup_started = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut cleanup_verified = false;
    let mut children_reaped = 0;
    loop {
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    children_reaped += 1;
                    if pid == main_pid {
                        exit_code = Some(code);
                        main_finished = true;
                    }
                }
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    children_reaped += 1;
                    if pid == main_pid {
                        exit_signal = Some(signal as i32);
                        main_finished = true;
                    }
                }
                Err(Errno::ECHILD) => {
                    cleanup_verified = true;
                    break;
                }
                Ok(WaitStatus::StillAlive) => break,
                Err(Errno::EINTR) => (),
                Err(error) => return Err(error.into()),
                Ok(_) => (),
            }
        }
        if cleanup_verified {
            break;
        }
        if cleanup_started.is_none() {
            cancelled = directory.join("cancel").exists() || interrupted.load(Ordering::SeqCst);
            timed_out =
                started.elapsed() >= Duration::from_secs(u64::from(command.timeout_seconds));
            if main_finished || cancelled || timed_out {
                cleanup_started = Some(Instant::now());
            }
        }
        if let Some(cleanup) = cleanup_started {
            let elapsed = cleanup.elapsed();
            signal_children(if elapsed < Duration::from_millis(200) {
                Signal::SIGTERM
            } else {
                Signal::SIGKILL
            })?;
            if elapsed > Duration::from_secs(3) {
                break;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    let stdout = stdout.recv_timeout(Duration::from_secs(1)).ok();
    let stderr = stderr.recv_timeout(Duration::from_secs(1)).ok();
    let streams_complete = stdout.is_some() && stderr.is_some();
    let (out, out_bytes) = stdout.unwrap_or_default();
    let (err, err_bytes) = stderr.unwrap_or_default();
    let truncated = out_bytes > out.len() || err_bytes > err.len() || !streams_complete;
    let mut receipt = json!({
        "supervisor_version":"proofstorm-exec/v1", "exit_code":exit_code, "exit_signal":exit_signal,
        "exit_scope":if command.argv.is_empty() { "shell" } else { "command" },
        "timed_out":timed_out,"cancelled":cancelled,"cleanup_verified":cleanup_verified,
        "children_reaped":children_reaped,"streams_complete":streams_complete,
        "output_mode":command.output.mode,"output_truncated":truncated,
        "stdout":"","stderr":"", "private_output":{
            "stdout":{"bytes_observed":out_bytes,"retained_bytes":out.len(),"sha256":format!("{:x}",Sha256::digest(&out))},
            "stderr":{"bytes_observed":err_bytes,"retained_bytes":err.len(),"sha256":format!("{:x}",Sha256::digest(&err))}
        }
    });
    match command.output.mode {
        OutputMode::Public if streams_complete => {
            receipt["stdout"] = json!(String::from_utf8_lossy(&out));
            receipt["stderr"] = json!(String::from_utf8_lossy(&err));
        }
        OutputMode::JsonFields => {
            let selected = if truncated {
                Err("output_capture_incomplete")
            } else {
                project_receipt(&out, &command.output.fields)
            };
            match selected {
                Ok(value) => {
                    receipt["selected_output"] = value;
                    receipt["projection_succeeded"] = json!(true);
                }
                Err(error) => {
                    receipt["projection_succeeded"] = json!(false);
                    receipt["projection_error"] = json!(error);
                }
            }
        }
        _ => (),
    }
    finish(directory, &receipt)
}
