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
    match command {
        "setup" if value["ready"] == true => "Proofstorm is ready.\n\nRun proofstorm gui to get started.\n".into(),
        "setup" if value["prepared"] == true => "Proofstorm tools are prepared. The runtime has not been started.\n\nRun proofstorm setup to finish.\n".into(),
        "gui" => format!("GUI {}: {}\nProject: {}\n", match value["browser"].as_str() {
            Some("existing_tab_focused") => "focused",
            Some("opened_default_browser") => "opened",
            _ => "ready",
        }, field(value, "url"), field(value, "project")),
        "stop" => format!("GUI {}. Labs keep running.\n", if value["stopped"] == true { "stopped" } else { "is not running" }),
        "attach" | "open" if value["attached"] == true => {
            let mut text = format!("Proofstorm MCP configured for {}.\nConfig: {}\n", field(value, "harness"), field(value, "config"));
            if let Some(backup) = value["backup"].as_str() {
                let _ = writeln!(text, "Backup: {backup}");
            }
            let _ = writeln!(text, "\n{}", field(value, "guidance"));
            text
        }
        "init" => "Local permissions configured.\n".into(),
        "install" => format!("Proofstorm {} installed.\n\nRun {} setup to get started.\n", field(value, "version"), field(value, "executable")),
        "connect" => format!("Connected to {}/{}: {}\nKeep this terminal open. Press Ctrl-C to disconnect.\n", field(value, "lab"), field(value, "component"), field(value, "url")),
        "environment" => {
            let mut text = String::new();
            if let Some(labs) = value["labs"]["items"].as_array() {
                if labs.is_empty() { text.push_str("No labs in this environment.\n"); }
                for lab in labs {
                    let _ = writeln!(text, "{}: {} ({})", lab["handle"]["name"].as_str().unwrap_or_else(|| field(lab, "id")), lab["runtime"]["phase"].as_str().unwrap_or("unknown"), field(&lab["runtime"], "state"));
                    for key in ["error", "message"] { describe(&mut text, key, &lab["runtime"][key], 2); }
                    describe(&mut text, "read error", &lab["read_error"], 2);
                }
            }
            describe(&mut text, "next cursor", &value["labs"]["next_cursor"], 0);
            text.push_str("Run proofstorm gui to explore, or use --json for full details.\n");
            text
        }
        "up" | "down" | "status" | "sync" if value.get("lab").is_some() => {
            let runtime = &value["runtime"];
            let mut text = format!("Lab {}: {}\n", field(&value["lab"], "name"), runtime["phase"].as_str().unwrap_or_else(|| field(&value["lab"], "phase")));
            for key in ["message", "retained_storage"] {
                describe(&mut text, key, &runtime[key], 0);
            }
            describe(&mut text, "reconciliation error", &value["reconciliation_error"], 0);
            if let Some(components) = runtime["components"].as_array() {
                for component in components {
                    let _ = writeln!(text, "  {}: {}", field(component, "id"), if component["ready"] == true { "ready" } else { "not ready" });
                    if component["ready"] != true {
                        describe(&mut text, "conditions", &component["conditions"], 4);
                    }
                }
            }
            text.push_str("Use --json for the full receipt.\n");
            text
        }
        "exec" | "result" => {
            let mut text = format!("Operation {}: {}\n", field(value, "id"), field(value, "phase"));
            describe(&mut text, "result", &value["artifact"]["content"], 0);
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
    fn summaries_distinguish_ready_prepared_and_browser_outcomes() {
        assert_eq!(
            human(
                "setup",
                &json!({"ready":true,"capacity":{"secret":"noise"}})
            ),
            "Proofstorm is ready.\n\nRun proofstorm gui to get started.\n"
        );
        assert!(human("setup", &json!({"prepared":true})).contains("not been started"));
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
        let lab = human(
            "up",
            &json!({"lab":{"name":"demo"},"runtime":{"phase":"failed","message":"image unavailable","components":[{"id":"node","ready":false,"conditions":[{"message":"pull failed"}]}]}}),
        );
        assert!(lab.contains("image unavailable") && lab.contains("pull failed"));
        let preview = human(
            "attach",
            &json!({"changes_applied":false,"attachment":{"project":"/project","entry":{"command":"/bin/mcp"}}}),
        );
        assert!(preview.contains("changes applied: false") && preview.contains("/bin/mcp"));
    }
}
