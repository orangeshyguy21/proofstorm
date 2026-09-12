//! Human-first CLI output. Machine output never contains progress or terminal escapes.
use anyhow::Result;
use serde_json::Value;
use std::{
    io::{IsTerminal, Write},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct Output {
    json: bool,
    command: &'static str,
    progress: Option<Progress>,
}

impl Output {
    pub fn new(json: bool, command: &'static str, label: Option<&str>) -> Self {
        Self {
            json,
            command,
            progress: label.filter(|_| !json).map(Progress::start),
        }
    }

    pub fn update(&self, label: &str) {
        if let Some(progress) = &self.progress {
            let _ = progress.sender.send(Some(label.to_owned()));
        }
    }

    pub fn stop(&mut self) {
        self.progress.take();
    }

    pub fn show(&mut self, value: &impl serde::Serialize) -> Result<()> {
        self.stop();
        let value = serde_json::to_value(value)?;
        let text = if self.json {
            serde_json::to_string_pretty(&value)? + "\n"
        } else {
            human(self.command, &value)
        };
        std::io::stdout().lock().write_all(text.as_bytes())?;
        Ok(())
    }
}

struct Progress {
    sender: Sender<Option<String>>,
    worker: Option<JoinHandle<()>>,
}

impl Progress {
    fn start(label: &str) -> Self {
        let (sender, receiver) = mpsc::channel();
        let animated = std::io::stderr().is_terminal()
            && std::env::var("TERM").map_or(true, |term| term != "dumb");
        let mut label = label.to_owned();
        // Render synchronously, before any artifact verification or runtime checks.
        let mut stderr = std::io::stderr();
        let mut width = render(&mut stderr, animated, &label, 0, 0);
        let worker = thread::spawn(move || {
            let mut frame = 0;
            loop {
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(next)) if next == label => continue,
                    Ok(Some(next)) => label = next,
                    Ok(None) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) if !animated => continue,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                frame += 1;
                width = render(&mut stderr, animated, &label, frame, width);
            }
            if animated {
                let _ = write!(stderr, "\r{:width$}\r", "");
                let _ = stderr.flush();
            }
        });
        Self {
            sender,
            worker: Some(worker),
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.sender.send(None);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn render(
    writer: &mut impl Write,
    animated: bool,
    label: &str,
    frame: usize,
    previous: usize,
) -> usize {
    if !animated {
        let _ = writeln!(writer, "{label}...");
        let _ = writer.flush();
        return 0;
    }
    // Keep the label short, ASCII-only and control-character-free.
    let label: String = label
        .chars()
        .filter(char::is_ascii)
        .filter(|c| !c.is_control())
        .take(48)
        .collect();
    let line = format!("{} {label}", ['|', '/', '-', '\\'][frame % 4]);
    let width = line.len();
    let _ = write!(
        writer,
        "\r{line}{:padding$}",
        "",
        padding = previous.saturating_sub(width)
    );
    let _ = writer.flush();
    width
}

fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().unwrap_or("unknown")
}

fn human(command: &str, value: &Value) -> String {
    use std::fmt::Write;
    let bin = proofstorm_app::command_name();
    match command {
        "setup" if value["ready"] == true => format!("Runtime ready.\nOpen the GUI: {bin} gui\n"),
        "setup" if value["prepared"] == true => {
            format!("Tools prepared. Runtime not started.\nStart it: {bin} setup\n")
        }
        "gui" => format!(
            "GUI {}: {}\nStop the GUI: {bin} gui stop\n",
            match value["browser"].as_str() {
                Some("existing_tab_focused") => "focused",
                Some("opened_default_browser") => "opened",
                _ => "ready",
            },
            field(value, "url")
        ),
        "gui-status" => {
            let mut text = format!("GUI {}.\n", field(value, "state"));
            if let Some(url) = value["url"].as_str() {
                let _ = writeln!(text, "{url}");
            }
            text
        }
        "stop" => format!(
            "GUI {}. Cells keep running.\n",
            if value["stopped"] == true {
                "stopped"
            } else {
                "is not running"
            }
        ),
        "attach" | "open" if value["attached"] == true => {
            let mut text = format!(
                "Proofstorm MCP configured for {}.\nConfig: {}\n",
                field(value, "harness"),
                field(value, "config")
            );
            if let Some(backup) = value["backup"].as_str() {
                let _ = writeln!(text, "Backup: {backup}");
            }
            text.push_str("Open a new agent session to load the tools.\n");
            text
        }
        "init" => "Local permissions configured.\n".into(),
        "install" => installation_summary(value),
        "connect" => format!(
            "Connected to {}/{}: {}\nCtrl-C to disconnect.\n",
            field(value, "cell"),
            field(value, "component"),
            field(value, "url")
        ),
        "environment" => cells_summary(value),
        "down" if value["cell"]["phase"] == "closed" => format!(
            "Removed cell {}.\nDeleted its workloads, storage, and activity history.\n",
            field(&value["cell"], "name")
        ),
        "up" | "down" | "status" | "sync" if value.get("cell").is_some() => cell_summary(value),
        "ops-list" => {
            let mut text = String::new();
            if let Some(items) = value["items"].as_array() {
                if items.is_empty() {
                    text.push_str("No recorded operations.\n");
                }
                for item in items {
                    let _ = writeln!(text, "{}  {}", field(item, "id"), field(item, "phase"));
                }
            }
            describe(&mut text, "next cursor", &value["next_cursor"], 0);
            text
        }
        "exec" | "result" => {
            let mut text = format!(
                "Operation {}: {}\n",
                field(value, "id"),
                field(value, "phase")
            );
            describe(&mut text, "result", &value["artifact"]["content"], 0);
            if value["artifact"]["content"].get("private_output").is_some() {
                text.push_str("Command output kept private.\n");
            }
            text
        }
        _ => {
            // Inspection and dry-run details use labeled fields rather than JSON punctuation.
            let mut text = String::new();
            describe(&mut text, "", value, 0);
            text
        }
    }
}

fn installation_summary(value: &Value) -> String {
    use std::fmt::Write;

    let executable = value["short_executable"]
        .as_str()
        .unwrap_or_else(|| field(value, "executable"));
    let mut text = format!(
        "Proofstorm {} installed.\nRun {executable} setup\n",
        field(value, "version")
    );
    if let Some(path) = value["short_command_conflict"].as_str() {
        let _ = writeln!(text, "Existing storm command kept: {path}");
    }
    text
}

fn cells_summary(value: &Value) -> String {
    use std::fmt::Write;

    let mut text = String::new();
    if let Some(cells) = value["cells"]["items"].as_array() {
        if cells.is_empty() {
            text.push_str("No cells.\n");
        }
        for cell in cells {
            let _ = writeln!(
                text,
                "{}: {} ({})",
                cell["handle"]["name"]
                    .as_str()
                    .unwrap_or_else(|| field(cell, "id")),
                cell["runtime"]["phase"].as_str().unwrap_or("unknown"),
                field(&cell["runtime"], "state")
            );
            for key in ["error", "message"] {
                describe(&mut text, key, &cell["runtime"][key], 2);
            }
            describe(&mut text, "read error", &cell["read_error"], 2);
        }
    }
    describe(&mut text, "next cursor", &value["cells"]["next_cursor"], 0);
    text
}

fn cell_summary(value: &Value) -> String {
    use std::fmt::Write;

    let runtime = &value["runtime"];
    let mut text = format!(
        "Cell {}: {}\n",
        field(&value["cell"], "name"),
        runtime["phase"]
            .as_str()
            .unwrap_or_else(|| field(&value["cell"], "phase"))
    );
    for key in ["message", "retained_storage"] {
        describe(&mut text, key, &runtime[key], 0);
    }
    describe(
        &mut text,
        "reconciliation error",
        &value["reconciliation_error"],
        0,
    );
    if let Some(components) = runtime["components"].as_array() {
        for component in components {
            let _ = writeln!(
                text,
                "  {}: {}",
                field(component, "id"),
                if component["ready"] == true {
                    "ready"
                } else {
                    "not ready"
                }
            );
            if component["ready"] != true {
                describe(&mut text, "conditions", &component["conditions"], 4);
            }
        }
    }
    text
}

fn describe(text: &mut String, label: &str, value: &Value, indent: usize) {
    use std::fmt::Write;
    if value.is_null()
        || value.as_object().is_some_and(serde_json::Map::is_empty)
        || value.as_array().is_some_and(Vec::is_empty)
    {
        return;
    }
    let label = label.replace('_', " ");
    let pad = " ".repeat(indent);
    match value {
        Value::Object(fields) => {
            let child_indent = if label.is_empty() {
                indent
            } else {
                let _ = writeln!(text, "{pad}{label}:");
                indent + 2
            };
            for (key, child) in fields {
                describe(text, key, child, child_indent);
            }
        }
        Value::Array(items) => {
            let _ = writeln!(text, "{pad}{label}:");
            for (index, item) in items.iter().enumerate() {
                describe(text, &format!("{}", index + 1), item, indent + 2);
            }
        }
        _ => {
            let _ = writeln!(
                text,
                "{pad}{label}: {}",
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_progress_is_prompt_animated_and_cleared_without_a_timer() {
        use nix::{
            fcntl::{FcntlArg, OFlag, fcntl},
            pty::openpty,
        };
        use std::{
            fs::File,
            io::{Read, Seek, SeekFrom},
            os::fd::AsRawFd,
            process::{Command, Stdio},
            time::Instant,
        };
        let pty = openpty(None, None).unwrap();
        fcntl(pty.master.as_raw_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        let mut master = File::from(pty.master);
        let mut output = tempfile::tempfile().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "cli_output::tests::terminal_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("PROOFSTORM_PROGRESS_FIXTURE", "1")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(File::from(pty.slave));
        let mut child = command.spawn().unwrap();
        drop(command); // The parent must not retain the slave descriptor.
        let start = Instant::now();
        let mut first = None;
        let mut progress = Vec::new();
        let status = loop {
            let mut bytes = [0; 4096];
            match master.read(&mut bytes) {
                Ok(n) if n > 0 => {
                    first.get_or_insert(start.elapsed());
                    progress.extend_from_slice(&bytes[..n]);
                }
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(5) => {}
                Err(error) => panic!("PTY read: {error}"),
            }
            if let Some(status) = child.try_wait().unwrap() {
                // Drain final clear-line bytes after the child's output flush.
                while let Ok(n) = master.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    progress.extend_from_slice(&bytes[..n]);
                }
                break status;
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("PTY fixture timed out");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success());
        assert!(first.is_some_and(|first| first < Duration::from_secs(5)));
        let progress = String::from_utf8(progress).unwrap();
        assert!(
            progress.contains("| Checking installation")
                && progress.contains("/ Checking installation")
        );
        assert!(progress.contains("Ready") && progress.ends_with('\r'));
        assert!(!progress.contains(['(', ')', '\x1b']));
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut result = String::new();
        output.read_to_string(&mut result).unwrap();
        assert!(result.contains("Runtime ready.") && !result.contains("\"ready\""));
    }

    #[test]
    #[ignore = "child fixture for the PTY test; not a separate acceptance check"]
    fn terminal_fixture() {
        assert_eq!(
            std::env::var("PROOFSTORM_PROGRESS_FIXTURE").as_deref(),
            Ok("1")
        );
        let mut output = Output::new(false, "setup", Some("Checking installation"));
        thread::sleep(Duration::from_millis(350));
        output.update("Ready");
        thread::sleep(Duration::from_millis(150));
        output.show(&json!({"ready":true})).unwrap();
    }

    #[test]
    fn summaries_distinguish_ready_prepared_and_browser_outcomes() {
        assert_eq!(
            human(
                "setup",
                &json!({"ready":true,"capacity":{"secret":"noise"}})
            ),
            "Runtime ready.\nOpen the GUI: proofstorm gui\n"
        );
        assert!(human("setup", &json!({"prepared":true})).contains("not started"));
        assert!(
            human(
                "gui",
                &json!({"browser":"not_opened","url":"http://localhost:123","project":"/test"})
            )
            .starts_with("GUI ready:")
        );
        assert!(human("stop", &json!({"stopped":false})).contains("not running"));
    }

    #[test]
    fn plain_progress_has_no_terminal_controls_and_frames_animate() {
        let mut plain = Vec::new();
        render(&mut plain, false, "Checking installation", 0, 0);
        assert_eq!(plain, b"Checking installation...\n");
        let mut terminal = Vec::new();
        let width = render(&mut terminal, true, "Checking installation", 0, 0);
        render(&mut terminal, true, "Ready", 1, width);
        let text = String::from_utf8(terminal).unwrap();
        assert!(text.contains("| Checking installation"));
        assert!(text.contains("/ Ready"));
        assert!(!text.contains(['(', ')']));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn failures_and_dry_runs_keep_actionable_details() {
        let cell = human(
            "up",
            &json!({"cell":{"name":"demo"},"runtime":{"phase":"failed","message":"image unavailable","components":[{"id":"node","ready":false,"conditions":[{"message":"pull failed"}]}]}}),
        );
        assert!(cell.contains("image unavailable") && cell.contains("pull failed"));
        let preview = human(
            "attach",
            &json!({"changes_applied":false,"attachment":{"project":"/project","entry":{"command":"/bin/mcp"}}}),
        );
        assert!(preview.contains("changes applied: false") && preview.contains("/bin/mcp"));
    }
}
