//! Supported macOS project links. No prompts, shell interpolation or trust overrides.
use super::{
    Harness,
    launch::{self, LaunchPlan},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fmt::Write,
    path::{Path, PathBuf},
};

fn identity(harness: Harness) -> (&'static str, &'static str, &'static str, [u64; 3]) {
    match harness {
        Harness::Opencode => (
            "OpenCode.app",
            "ai.opencode.desktop",
            "opencode",
            [1, 18, 30],
        ),
        Harness::Claude => (
            "Claude.app",
            "com.anthropic.claudefordesktop",
            "claude",
            [1, 40609, 1],
        ),
        Harness::Codex => unreachable!("Codex uses its bundled project launcher"),
    }
}

pub(super) fn validate(harness: Harness, info: &Value) -> Result<String> {
    let (_, bundle, scheme, minimum) = identity(harness);
    ensure!(
        info["CFBundleIdentifier"] == bundle,
        "unexpected desktop application identity"
    );
    ensure!(
        info["CFBundleURLTypes"]
            .as_array()
            .is_some_and(|types| types.iter().any(|t| t["CFBundleURLSchemes"]
                .as_array()
                .is_some_and(|schemes| schemes.iter().any(|s| s == scheme)))),
        "desktop app does not register its supported project link"
    );
    let version = info["CFBundleShortVersionString"]
        .as_str()
        .context("desktop version missing")?;
    let numbers = version
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        numbers.len() == 3 && numbers[0] == minimum[0] && numbers.as_slice() >= minimum.as_slice(),
        "update {} desktop before opening a project; this alpha requires a verified project-link version",
        harness.name()
    );
    Ok(version.into())
}

pub(super) fn project_url(harness: Harness, project: &Path) -> Result<String> {
    let path = project.to_str().context("non-UTF-8 project folder")?;
    ensure!(project.is_absolute(), "project folder must be absolute");
    // Encode every non-unreserved byte, including URL syntax and UTF-8. This
    // keeps paths containing '&q=', '#', spaces or quotes a single literal value.
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(&mut encoded, "%{byte:02X}")?;
        }
    }
    Ok(format!(
        "{}{encoded}",
        match harness {
            Harness::Opencode => "opencode://open-project?directory=",
            Harness::Claude => "claude://code/new?folder=",
            Harness::Codex => unreachable!("Codex uses its bundled project launcher"),
        }
    ))
}

pub(super) fn detect(harness: Harness, project: &Path) -> Result<LaunchPlan> {
    ensure!(
        cfg!(target_os = "macos"),
        "native agent buttons currently support macOS; run proofstorm open without --gui in a terminal"
    );
    let (filename, _, _, _) = identity(harness);
    let mut apps = vec![PathBuf::from("/Applications").join(filename)];
    if let Some(home) = std::env::var_os("HOME") {
        apps.push(PathBuf::from(home).join("Applications").join(filename));
    }
    let mut last_error = None;
    for app in apps {
        let plist = app.join("Contents/Info.plist");
        if !plist.is_file() {
            continue;
        }
        let inspected = (|| {
            let output = launch::capture(
                Path::new("/usr/bin/plutil"),
                &[
                    "-convert",
                    "json",
                    "-o",
                    "-",
                    plist.to_str().context("non-UTF-8 app path")?,
                ],
            )?;
            let info: Value = serde_json::from_str(&output)?;
            let binary = info["CFBundleExecutable"]
                .as_str()
                .context("desktop executable missing")?;
            ensure!(
                Path::new(binary).components().count() == 1 && !binary.starts_with('.'),
                "invalid desktop executable name"
            );
            ensure!(
                launch::executable(&app.join("Contents/MacOS").join(binary)),
                "desktop executable is missing; reinstall the app before retrying"
            );
            validate(harness, &info)
        })();
        match inspected {
            Ok(version) => {
                return Ok(LaunchPlan {
                    executable: "/usr/bin/open".into(),
                    version,
                    interface: "desktop",
                    arguments: vec![
                        "-a".into(),
                        app.to_str().context("non-UTF-8 app path")?.into(),
                        project_url(harness, project)?,
                    ],
                    project: project.into(),
                    desktop: Some(app),
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("{} desktop was not found in Applications; install the native app, or run proofstorm open without --gui in a terminal. Nothing was attached.", harness.name())))
}
