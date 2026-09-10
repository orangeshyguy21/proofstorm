//! Native, read-only folder selection. The browser never supplies script text.
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

const SCRIPT: &str = r#"on run argv
    try
        activate
        if (item 1 of argv) is "" then
            set selectedFolder to choose folder with prompt "Choose a project folder for Proofstorm"
        else
            set selectedFolder to choose folder with prompt "Choose a project folder for Proofstorm" default location (POSIX file (item 1 of argv))
        end if
        return POSIX path of selectedFolder
    on error number -128
        return ""
    end try
end run"#;

pub(super) fn selected(output: &[u8]) -> Result<Option<PathBuf>> {
    ensure!(
        output.len() <= 8192,
        "folder picker returned an invalid path"
    );
    let text = std::str::from_utf8(output).context("folder path is not UTF-8")?;
    // osascript adds one line ending. Do not trim meaningful spaces/newlines
    // inside a folder name; the chosen POSIX folder itself ends in '/'.
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return Ok(None);
    }
    let path = Path::new(text);
    ensure!(path.is_absolute(), "folder picker returned a relative path");
    let path = path
        .canonicalize()
        .context("selected folder no longer exists")?;
    ensure!(
        path.is_dir() && path.parent().is_some(),
        "choose a project folder, not a file or filesystem root"
    );
    if let Some(home) = std::env::var_os("HOME") {
        ensure!(
            path != PathBuf::from(home).canonicalize()?,
            "choose a project folder, not your entire home directory"
        );
    }
    Ok(Some(path))
}

pub(super) async fn choose(initial: Option<&Path>) -> Result<Option<PathBuf>> {
    ensure!(
        cfg!(target_os = "macos"),
        "the native folder picker currently requires macOS"
    );
    let initial = if let Some(path) = initial {
        ensure!(path.is_absolute(), "initial folder must be absolute");
        // A removed project need not prevent choosing its replacement.
        path.canonicalize().ok().filter(|p| p.is_dir())
    } else {
        None
    };
    let mut command = tokio::process::Command::new("/usr/bin/osascript");
    command
        .args(["-e", SCRIPT])
        .arg(initial.as_deref().unwrap_or(Path::new("")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(180), command.output())
        .await
        .context("folder selection timed out; click the folder to try again")?
        .context("could not start the native folder picker")?;
    ensure!(
        output.status.success(),
        "macOS could not open the folder picker. Check its permission prompt and try again."
    );
    selected(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_preserves_the_existing_selection() {
        assert_eq!(selected(b"\n").unwrap(), None);
        assert_eq!(selected(b"").unwrap(), None);
    }

    #[test]
    fn selection_preserves_literal_folder_names_and_rejects_non_directories() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("it's &q=literal\nwith a trailing space ");
        std::fs::create_dir(&path).unwrap();
        let output = format!("{}/\n", path.display());
        assert_eq!(
            selected(output.as_bytes()).unwrap(),
            Some(path.canonicalize().unwrap())
        );
        let file = root.path().join("file");
        std::fs::write(&file, "fixture").unwrap();
        assert!(selected(file.to_str().unwrap().as_bytes()).is_err());
        assert!(selected(b"relative\n").is_err());
        assert!(selected(b"/\n").is_err());
        assert!(selected(&[0xff]).is_err());
        assert!(selected(&vec![b'a'; 8193]).is_err());
    }

    #[test]
    fn script_uses_only_literal_arguments_and_does_not_control_other_apps() {
        assert!(SCRIPT.contains("POSIX file (item 1 of argv)"));
        assert!(SCRIPT.contains("on error number -128"));
        assert!(!SCRIPT.contains("do shell script"));
        assert!(!SCRIPT.contains("tell application"));
    }
}
