//! Local version preparation and read-only release selection for the Bash shortcuts.
use super::{alpha_version, bundle, text};
use crate::development::regular;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, ffi::OsString, fs, io::Write, path::Path, process::Command};

fn read(path: &Path) -> Result<String> {
    regular(path)?;
    ensure!(
        fs::metadata(path)?.len() <= super::MAX_METADATA_BYTES,
        "version file is too large"
    );
    Ok(fs::read_to_string(path)?)
}

fn parts(version: &str) -> Result<Vec<u64>> {
    ensure!(
        alpha_version(version),
        "expected an alpha version such as 0.1.0-alpha.2 (without v)"
    );
    version
        .replace("-alpha.", ".")
        .split('.')
        .map(|part| {
            ensure!(
                part == "0" || !part.starts_with('0'),
                "version must not have leading zeroes"
            );
            Ok(part.parse()?)
        })
        .collect()
}

fn workspace(root: &Path) -> Result<(String, BTreeSet<String>)> {
    let manifest: toml::Value = toml::from_str(&read(&root.join("Cargo.toml"))?)?;
    let workspace = manifest.get("workspace").context("missing workspace")?;
    let version = workspace
        .get("package")
        .and_then(|p| p.get("version"))
        .context("missing workspace version")?
        .as_str()
        .context("missing workspace version")?
        .to_owned();
    parts(&version)?;
    let members = workspace
        .get("members")
        .context("missing workspace members")?
        .as_array()
        .context("missing workspace members")?;
    let mut names = BTreeSet::new();
    for member in members {
        let member = member.as_str().context("invalid workspace member")?;
        ensure!(bundle::safe_name(member), "unsafe workspace member");
        let package: toml::Value = toml::from_str(&read(&root.join(member).join("Cargo.toml"))?)?;
        let package = package.get("package").context("missing member package")?;
        ensure!(
            package
                .get("version")
                .and_then(|v| v.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true),
            "member does not inherit workspace version"
        );
        let name = package
            .get("name")
            .context("missing package name")?
            .as_str()
            .context("missing package name")?;
        ensure!(names.insert(name.to_owned()), "duplicate workspace package");
    }
    ensure!(!names.is_empty(), "empty workspace");
    Ok((version, names))
}

fn replace_line(input: &str, old: &str, new: &str) -> Result<String> {
    ensure!(
        input.lines().filter(|line| *line == old).count() == 1,
        "expected exactly one version field: {old}"
    );
    Ok(input
        .split_inclusive('\n')
        .map(|line| {
            if line.trim_end_matches('\n') == old {
                format!("{new}{}", if line.ends_with('\n') { "\n" } else { "" })
            } else {
                line.to_owned()
            }
        })
        .collect())
}

fn lock_versions(input: &str, names: &BTreeSet<String>, old: &str, new: &str) -> Result<String> {
    let mut seen = BTreeSet::new();
    let mut output = String::new();
    for (index, chunk) in input.split("[[package]]\n").enumerate() {
        if index == 0 {
            output.push_str(chunk);
            continue;
        }
        let entry: toml::Value = toml::from_str(chunk)?;
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .context("lock entry has no name")?;
        output.push_str("[[package]]\n");
        if names.contains(name) {
            ensure!(
                entry.get("source").is_none()
                    && entry.get("version").and_then(toml::Value::as_str) == Some(old)
                    && seen.insert(name.to_owned()),
                "workspace lock entry is inconsistent: {name}"
            );
            output.push_str(&replace_line(
                chunk,
                &format!("version = \"{old}\""),
                &format!("version = \"{new}\""),
            )?);
        } else {
            output.push_str(chunk);
        }
    }
    ensure!(
        &seen == names,
        "workspace package is missing from Cargo.lock"
    );
    for name in names {
        output = output.replace(&format!("\"{name} {old}\""), &format!("\"{name} {new}\""));
    }
    let _: toml::Value = toml::from_str(&output)?;
    Ok(output)
}

fn changes(root: &Path, new: &str) -> Result<Vec<(String, String, String)>> {
    let (old, names) = workspace(root)?;
    let mut edits = Vec::new();
    for (name, prefix, suffix) in [
        ("Cargo.toml", "version = \"", "\""),
        ("install.sh", "install_version=\"", "\""),
        ("tools/versions.env", "PROOFSTORM_VERSION=", ""),
        ("charts/proofstorm/Chart.yaml", "version: ", ""),
        ("charts/proofstorm/values.yaml", "  tag: ", ""),
    ] {
        let before = read(&root.join(name))?;
        let mut after = replace_line(
            &before,
            &format!("{prefix}{old}{suffix}"),
            &format!("{prefix}{new}{suffix}"),
        )?;
        if name.ends_with("/Chart.yaml") {
            after = replace_line(
                &after,
                &format!("appVersion: {old}"),
                &format!("appVersion: {new}"),
            )?;
        }
        edits.push((name.into(), before, after));
    }
    let before = read(&root.join("Cargo.lock"))?;
    let after = lock_versions(&before, &names, &old, new)?;
    edits.push(("Cargo.lock".into(), before, after));
    Ok(edits)
}

fn prepare(root: &Path, new: &str) -> Result<()> {
    let root = root.canonicalize()?;
    let (old, _) = workspace(&root)?;
    ensure!(
        parts(new)? > parts(&old)?,
        "choose a version newer than {old}"
    );
    let git = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(&root)
        .output()?;
    ensure!(
        git.status.success() && git.stdout.is_empty(),
        "start from a clean checkout; commit or stash your changes first"
    );
    let edits = changes(&root, new)?;
    // Stage and validate every edit before replacing any user file.
    let mut staged = Vec::new();
    for (name, before, after) in &edits {
        let path = root.join(name);
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
        temporary.write_all(after.as_bytes())?;
        temporary
            .as_file()
            .set_permissions(fs::metadata(&path)?.permissions())?;
        temporary.as_file().sync_all()?;
        ensure!(
            read(&path)? == *before,
            "version input changed during preparation: {name}"
        );
        staged.push((path, temporary));
    }
    for (path, temporary) in staged {
        temporary.persist(&path).with_context(|| {
            format!(
                "could not update {}; inspect the version diff before retrying",
                path.display()
            )
        })?;
    }
    println!(
        "Prepared source version {old} -> {new}. Review the six-file diff; nothing committed, pushed, tagged, or published."
    );
    println!(
        "Main CI will build, verify, and publish matching AMD64 and ARM64 controllers automatically. Existing image records were not relabelled."
    );
    println!(
        "Run just check, then review and merge. Wait for main's Linux and Mac bundle checks before running just release."
    );
    Ok(())
}

fn select_run(path: &Path, repo: &str, sha: &str) -> Result<u64> {
    let pages = bundle::read_json(path)?;
    let mut selected: Option<&Value> = None;
    for page in pages.as_array().context("invalid run pages")? {
        for run in page["workflow_runs"]
            .as_array()
            .context("invalid run list")?
        {
            ensure!(
                run["repository"]["full_name"] == repo
                    && run["head_sha"] == sha
                    && run["head_branch"] == "main",
                "run query returned a different repository/commit/branch"
            );
            if !matches!(run["event"].as_str(), Some("push" | "workflow_dispatch")) {
                continue;
            }
            let id = run["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .context("invalid run id")?;
            if selected.is_none_or(|previous| previous["id"].as_u64().unwrap_or(0) < id) {
                selected = Some(run);
            }
        }
    }
    let run = selected.context("No Checks run for current main yet. Wait for its Linux and Mac bundle builds; older commits are not selected.")?;
    ensure!(
        run["status"] == "completed" && run["conclusion"] == "success",
        "Latest Checks run for current main is not green yet. Finish/fix it before releasing."
    );
    run["id"].as_u64().context("missing run id")
}

pub(super) fn cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    let values: Vec<_> = args.collect();
    let args: Vec<_> = values
        .iter()
        .map(|s| s.to_str().context("arguments must be UTF-8"))
        .collect::<Result<_>>()?;
    match args.as_slice() {
        ["prepare", root, new] => prepare(Path::new(root), new)?,
        ["version", root] => {
            let (version, _) = workspace(Path::new(root))?;
            changes(Path::new(root), &version)?;
            println!("v{version}");
        }
        ["main", file] => {
            let value = bundle::read_json(Path::new(file))?;
            let sha = text(&value["commit"], "sha")?;
            ensure!(
                value["name"] == "main"
                    && sha.len() == 40
                    && sha
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid main commit"
            );
            println!("{sha}");
        }
        ["select", file, repo, sha] => println!("{}", select_run(Path::new(file), repo, sha)?),
        ["dispatch", id, tag] => {
            ensure!(id.parse::<u64>().is_ok_and(|n| n > 0), "invalid run id");
            parts(tag.strip_prefix('v').context("expected vVERSION")?)?;
            println!(
                "{}",
                json!({"ref":"main","inputs":{"run_id":id,"tag":tag,"create_draft":"true"}})
            );
        }
        _ => bail!("invalid release-shortcut arguments"),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
