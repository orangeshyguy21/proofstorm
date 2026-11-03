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

fn identity(harness: Harness) -> (&'static str, &'static str, Option<&'static str>, [u64; 3]) {
    match harness {
        // OpenCode 2 desktop has no project link: it opens, and the user picks the folder.
        Harness::Opencode => ("OpenCode.app", "ai.opencode.desktop", None, [2, 0, 0]),
        Harness::Claude => (
            "Claude.app",
            "com.anthropic.claudefordesktop",
            Some("claude://code/new?folder="),
            [2, 7032, 0],
        ),
        Harness::Codex => unreachable!("Codex uses its bundled project launcher"),
    }
}

/// Where a harness's native app may be installed, in search order.
pub(super) fn bundles(harness: Harness) -> Vec<PathBuf> {
    let names = match harness {
        Harness::Codex => vec!["Codex.app", "ChatGPT.app"],
        _ => vec![identity(harness).0],
    };
    let mut roots = vec![PathBuf::from("/Applications")];
    roots.extend(std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications")));
    roots
        .iter()
        .flat_map(|root| names.iter().map(move |name| root.join(name)))
        .collect()
}

pub(super) fn validate(harness: Harness, info: &Value) -> Result<String> {
    let (_, bundle, link, minimum) = identity(harness);
    ensure!(
        info["CFBundleIdentifier"] == bundle,
        "unexpected desktop application identity"
    );
    ensure!(
        link.and_then(|link| link.split("://").next())
            .is_none_or(|scheme| info["CFBundleURLTypes"]
                .as_array()
                .is_some_and(|types| types.iter().any(|t| t["CFBundleURLSchemes"]
                    .as_array()
                    .is_some_and(|schemes| schemes.iter().any(|s| s == scheme))))),
        "desktop app does not register its supported project link"
    );
    let version = info["CFBundleShortVersionString"]
        .as_str()
        .context("desktop version missing")?;
    // These apps update themselves, so newer releases are accepted: the identity
    // and project link checked above are what launching relies on.
    let numbers = version
        .split('.')
        .map_while(|part| part.parse::<u64>().ok())
        .collect::<Vec<_>>();
    ensure!(
        !numbers.is_empty() && numbers.as_slice() >= minimum.as_slice(),
        "{} desktop {version} is older than {}; update it before opening a project",
        harness.name(),
        minimum.map(|n| n.to_string()).join(".")
    );
    Ok(version.into())
}

pub(super) fn project_url(harness: Harness, project: &Path) -> Result<Option<String>> {
    let Some(prefix) = identity(harness).2 else {
        return Ok(None);
    };
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
    Ok(Some(format!("{prefix}{encoded}")))
}

pub(super) fn detect(harness: Harness, project: &Path) -> Result<LaunchPlan> {
    ensure!(
        cfg!(target_os = "macos"),
        "native agent buttons currently support macOS; run {} agent open without --desktop in a terminal",
        crate::command_name()
    );
    let mut last_error = None;
    for app in bundles(harness) {
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
                let mut arguments = vec![
                    "-a".into(),
                    app.to_str().context("non-UTF-8 app path")?.into(),
                ];
                arguments.extend(project_url(harness, project)?);
                return Ok(LaunchPlan {
                    executable: "/usr/bin/open".into(),
                    version,
                    interface: "desktop",
                    arguments,
                    project: project.into(),
                    desktop: Some(app),
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("{} desktop was not found in Applications; install the native app, or run {} agent open without --desktop in a terminal. Nothing was attached.", harness.name(), crate::command_name())))
}
