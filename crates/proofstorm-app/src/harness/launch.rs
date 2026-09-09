//! No URI guessing, automatic installer, trust override, or prompt injection.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    io::{IsTerminal, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub struct LaunchPlan {
    pub executable: PathBuf,
    pub version: String,
    pub interface: &'static str,
    pub arguments: Vec<String>,
    pub project: PathBuf,
    pub desktop: Option<PathBuf>,
}

pub(crate) fn capture(executable: &Path, args: &[&str]) -> Result<String> {
    let mut output = tempfile::tempfile()?;
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        // OpenCode's successful --help uses stderr (yargs). Inspect both streams
        // for help only; do not turn ordinary launch diagnostics into output.
        .stderr(if args.last() == Some(&"--help") {
            Stdio::from(output.try_clone()?)
        } else {
            Stdio::null()
        })
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "agent command failed; check the installation"
            );
            break;
        }
        if start.elapsed() > Duration::from_secs(30) || output.metadata()?.len() > 1024 * 1024 {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("agent command exceeded its time/output limit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    output.seek(SeekFrom::Start(0))?;
    ensure!(
        output.metadata()?.len() <= 1024 * 1024,
        "agent command output exceeded its limit"
    );
    let mut result = String::new();
    output.read_to_string(&mut result)?;
    Ok(result)
}

pub(super) fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.is_file() && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

pub fn detect(project: &Path, cli: bool) -> Result<LaunchPlan> {
    let mut apps = vec![
        PathBuf::from("/Applications/Codex.app"),
        PathBuf::from("/Applications/ChatGPT.app"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        apps.extend([
            PathBuf::from(&home).join("Applications/Codex.app"),
            PathBuf::from(home).join("Applications/ChatGPT.app"),
        ]);
    }
    let path_cli = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("codex"))
            .find(|p| executable(p))
    });
    if let Some(path) = path_cli.as_ref().and_then(|p| p.canonicalize().ok()) {
        if let Some(app) = path
            .ancestors()
            .find(|p| p.extension().is_some_and(|ext| ext == "app"))
        {
            apps.insert(0, app.to_path_buf());
        }
    }
    let desktop = apps.into_iter().find(|app| {
        app.join("Contents/Info.plist").is_file()
            && executable(&app.join("Contents/Resources/codex"))
    });
    let executable = if cli { path_cli.or_else(|| desktop.as_ref().map(|app| app.join("Contents/Resources/codex"))) }
        else { desktop.as_ref().map(|app| app.join("Contents/Resources/codex")) }
        .context(if cli {"Codex CLI is not installed; install Codex, then retry"} else {"Codex desktop app was not found; install it yourself or choose --cli. Nothing was installed or attached."})?;
    let version = capture(&executable, &["--version"])?;
    ensure!(
        version.starts_with("codex-cli "),
        "unrecognized Codex executable"
    );
    let help = capture(
        &executable,
        if cli { &["--help"] } else { &["app", "--help"] },
    )?;
    ensure!(
        if cli {
            help.contains("--cd")
        } else {
            help.contains("[PATH]") && help.contains("Workspace path")
        },
        "this Codex version does not expose the supported project launch option"
    );
    Ok(LaunchPlan {
        executable,
        version: version.trim().into(),
        interface: if cli { "cli" } else { "desktop" },
        arguments: vec![
            if cli { "--cd" } else { "app" }.into(),
            project.to_str().context("non-UTF-8 project")?.into(),
        ],
        project: project.into(),
        desktop,
    })
}

pub fn require_terminal() -> Result<()> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "opening this agent needs an interactive terminal; use proofstorm attach from automation, then open the agent in your terminal"
    );
    Ok(())
}

pub fn run(plan: &LaunchPlan) -> Result<()> {
    if plan.interface == "cli" {
        require_terminal()?;
        ensure!(
            Command::new(&plan.executable)
                .args(&plan.arguments)
                .current_dir(&plan.project)
                .status()?
                .success(),
            "agent exited unsuccessfully"
        );
    } else {
        ensure!(
            plan.desktop
                .as_ref()
                .is_some_and(|app| app.join("Contents/Info.plist").is_file())
                && executable(&plan.executable),
            "desktop app disappeared; refusing the installer fallback"
        );
        capture(
            &plan.executable,
            &plan
                .arguments
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}

/// Native project handoff by default; terminal interfaces require explicit --cli.
pub fn detect_for(harness: super::Harness, project: &Path, cli: bool) -> Result<LaunchPlan> {
    if harness == super::Harness::Codex {
        return detect(project, cli);
    }
    if !cli {
        return super::desktop::detect(harness, project);
    }
    let executable = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .filter(|p| p.is_absolute())
        .map(|p| p.join(harness.name()))
        .find(|p| executable(p))
        .with_context(|| {
            format!(
                "{} CLI is not installed or is not on PATH; install it before connecting",
                harness.name()
            )
        })?;
    let version = capture(&executable, &["--version"])?;
    supported_version(harness, &version)?;
    let help = capture(&executable, &["--help"])?;
    ensure!(
        match harness {
            super::Harness::Opencode => help.contains("opencode [project]"),
            super::Harness::Claude => help.contains("Claude Code") && help.contains("--mcp-config"),
            super::Harness::Codex => unreachable!(),
        },
        "installed agent does not expose the supported CLI launch interface"
    );
    Ok(LaunchPlan {
        executable,
        version: version.trim().into(),
        interface: "cli",
        arguments: Vec::new(),
        project: project.into(),
        desktop: None,
    })
}

pub(super) fn supported_version(harness: super::Harness, version: &str) -> Result<()> {
    let version = version.trim();
    let valid = match harness {
        super::Harness::Opencode => {
            version.starts_with("1.")
                && version
                    .split('.')
                    .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        }
        super::Harness::Claude => version.starts_with("2.") && version.ends_with(" (Claude Code)"),
        super::Harness::Codex => version.starts_with("codex-cli "),
    };
    ensure!(
        valid,
        "unsupported {} version; this alpha supports OpenCode 1.x and Claude Code 2.x only",
        harness.name()
    );
    Ok(())
}

/// Display-only command for browsers without an interactive terminal.
#[must_use]
pub fn terminal_command(plan: &LaunchPlan) -> String {
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
    let mut command = format!(
        "cd -- {} && {}",
        quote(&plan.project.to_string_lossy()),
        quote(&plan.executable.to_string_lossy())
    );
    for arg in &plan.arguments {
        command.push(' ');
        command.push_str(&quote(arg));
    }
    command
}
