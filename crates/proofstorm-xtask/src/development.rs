use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, Permissions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus},
};

const RUNTIME: &[&str] = &[
    "proofstorm-core",
    "proofstorm-kube",
    "proofstorm-transfer",
    "proofstorm-exec",
    "proofstormd",
];
const LAUNCHER_HEADER: &str = "#!/bin/sh\n# Proofstorm checkout launcher v1\n";

pub(super) fn directory(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        directory(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_dir(),
            "linked/non-directory output refused: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn regular(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "linked/non-file input refused: {}",
        path.display()
    );
    for parent in path.ancestors().skip(1) {
        ensure!(
            !fs::symlink_metadata(parent)?.file_type().is_symlink(),
            "linked parent refused"
        );
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    regular(path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_owned(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        regular(path)?;
    }
    let parent = path.parent().context("output has no parent")?;
    directory(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(mode))?;
    temporary.persist(path)?;
    Ok(())
}

pub(super) fn future_canonical(path: &Path) -> Result<PathBuf> {
    if path.try_exists()? {
        return Ok(path.canonicalize()?);
    }
    ensure!(
        fs::symlink_metadata(path).is_err(),
        "dangling linked target refused"
    );
    let parent = path.parent().context("target has no parent")?;
    Ok(future_canonical(parent)?.join(path.file_name().context("invalid target path")?))
}

fn owned_work(source: &Path) -> Result<PathBuf> {
    let work = source.join(".proofstorm-dev");
    let marker = work.join("owner.json");
    ensure!(
        read_json(&marker)? == json!({"source": source}),
        "development directory belongs to a different checkout"
    );
    directory(&work)?;
    Ok(work)
}

fn target(source: &Path, work: &Path) -> Result<PathBuf> {
    let settings = read_json(&work.join("build.json"))?;
    let target = PathBuf::from(
        settings["target"]
            .as_str()
            .context("invalid build target")?,
    );
    ensure!(
        target.is_absolute() && future_canonical(&target)? == target,
        "build target must be an absolute canonical directory"
    );
    ensure!(
        !target.starts_with(source.join("target")),
        "use a dedicated dev target, not the legacy checkout target"
    );
    directory(&target)?;
    Ok(target)
}

pub(super) fn prepare(source: &Path, selected: Option<&Path>) -> Result<PathBuf> {
    let work = source.join(".proofstorm-dev");
    let marker = work.join("owner.json");
    if fs::symlink_metadata(&work).is_ok() {
        ensure!(
            marker.is_file(),
            "unowned .proofstorm-dev directory; resolve it explicitly"
        );
        owned_work(source)?;
    } else {
        directory(&work)?;
        write_owned(
            &marker,
            &serde_json::to_vec(&json!({"source": source}))?,
            0o600,
        )?;
    }
    let settings = work.join("build.json");
    if selected.is_none() && fs::symlink_metadata(&settings).is_ok() {
        return target(source, &work);
    }
    let selected = selected.map_or_else(|| work.join("target"), |p| source.join(p));
    let selected = future_canonical(&selected)?;
    ensure!(
        !selected.starts_with(source.join("target")),
        "use a dedicated dev target, not the legacy checkout target"
    );
    directory(&selected)?;
    write_owned(
        &settings,
        &serde_json::to_vec(&json!({"target": selected}))?,
        0o600,
    )?;
    Ok(selected)
}

pub(super) fn inventory(root: &Path) -> Result<BTreeMap<String, String>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
        ensure!(
            fs::symlink_metadata(path)?.is_dir(),
            "linked/non-directory resource refused"
        );
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                visit(root, &path, files)?;
            } else {
                regular(&path)?;
                let mut file = fs::File::open(&path)?;
                let mut digest = Sha256::new();
                let mut buffer = vec![0u8; 65536];
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    digest.update(&buffer[..count]);
                }
                files.insert(
                    path.strip_prefix(root)?
                        .to_str()
                        .context("resource path must be UTF-8")?
                        .into(),
                    format!("{:x}", digest.finalize()),
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn tree_sha(files: &BTreeMap<String, String>) -> String {
    let mut digest = Sha256::new();
    for (name, sha) in files {
        digest.update(name.as_bytes());
        digest.update(b"\0");
        digest.update(sha.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}

fn selected(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "Dockerfile.proofstormd" | ".dockerignore"
    ) || (parts.len() >= 3
        && parts[0] == "crates"
        && (RUNTIME.contains(&parts[1]) || (parts.len() == 3 && parts[2] == "Cargo.toml")))
        || (parts.len() == 3
            && parts[0] == "docker"
            && ["wallet", "mint", "bitcoin"].contains(&parts[1])
            && parts[2].ends_with("-provenance.json"))
}

fn controller_snapshot(source: &Path, destination: &Path, names: &[&str]) -> Result<String> {
    directory(destination)?;
    for name in names.iter().filter(|name| !name.is_empty()) {
        let relative = Path::new(name);
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "unsafe controller source path"
        );
        if !selected(name) {
            continue;
        }
        let original = source.join(relative);
        if fs::symlink_metadata(&original).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        regular(&original)?;
        let output = destination.join(relative);
        directory(output.parent().context("missing output parent")?)?;
        fs::copy(original, output)?;
    }
    if destination.join("crates").is_dir() {
        for entry in fs::read_dir(destination.join("crates"))? {
            let path = entry?.path();
            if !RUNTIME.iter().any(|name| path.ends_with(name)) && path.join("Cargo.toml").is_file()
            {
                directory(&path.join("src"))?;
                fs::write(path.join("src/lib.rs"), "// Unbuilt workspace member.\n")?;
                fs::write(path.join("src/main.rs"), "fn main() {}\n")?;
            }
        }
    }
    Ok(tree_sha(&inventory(destination)?))
}

pub(super) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    // Validate the whole tree before copying; never follow chart links to secrets.
    let files = inventory(source)?;
    directory(destination)?;
    for name in files.keys() {
        let output = destination.join(name);
        directory(output.parent().context("missing chart parent")?)?;
        fs::copy(source.join(name), output)?;
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let result = command
        .output()
        .with_context(|| format!("launch {command:?}"))?;
    ensure!(
        result.status.success(),
        "command failed: {command:?}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

pub(super) fn resources(source: &Path) -> Result<PathBuf> {
    let work = owned_work(source)?;
    let target = target(source, &work)?;
    let resources = work.join("resources");
    directory(&resources)?;
    let scratch = tempfile::Builder::new()
        .prefix(".build-")
        .tempdir_in(&resources)?;
    let stage = scratch.path().join("payload");
    directory(&stage)?;
    copy_tree(&source.join("charts/proofstorm"), &stage.join("chart"))?;
    output(
        Command::new(target.join("debug/examples/export_crds"))
            .arg(stage.join("chart/crds"))
            .current_dir(source),
    )?;
    let info = output(
        Command::new(target.join("debug/proofstorm"))
            .arg("release-info")
            .current_dir(source),
    )?;
    serde_json::from_slice::<Value>(&info).context("invalid release-info JSON")?;
    fs::write(stage.join("release-info.json"), info)?;
    let names = output(
        Command::new("git")
            .args([
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ])
            .current_dir(source),
    )?;
    let names = std::str::from_utf8(&names).context("source paths must be UTF-8")?;
    let sha = controller_snapshot(
        source,
        &stage.join("controller-source"),
        &names.split('\0').collect::<Vec<_>>(),
    )?;
    fs::write(
        stage.join("controller-source.json"),
        serde_json::to_vec(&json!({"format_version":1,"sha256":sha}))?,
    )?;
    publish(&stage, &resources)
}

fn publish(stage: &Path, resources: &Path) -> Result<PathBuf> {
    let files = inventory(stage)?;
    let name = format!("{:x}", Sha256::digest(serde_json::to_vec(&files)?));
    let destination = resources.join(name);
    if fs::symlink_metadata(&destination).is_ok() {
        ensure!(
            inventory(&destination)? == files,
            "resource snapshot was modified; refusing overwrite"
        );
    } else {
        fs::rename(stage, &destination)?;
    }
    Ok(destination)
}

fn quote(path: &Path) -> Result<String> {
    Ok(format!(
        "'{}'",
        path.to_str()
            .context("launcher paths must be UTF-8")?
            .replace('\'', "'\"'\"'")
    ))
}

pub(super) fn launchers(source: &Path) -> Result<()> {
    let work = owned_work(source)?;
    let target = target(source, &work)?;
    for name in ["proofstorm", "proofstorm-mcp"] {
        let path = work.join("bin").join(name);
        if fs::symlink_metadata(&path).is_ok() {
            regular(&path)?;
            ensure!(
                fs::read_to_string(&path)?.starts_with(LAUNCHER_HEADER),
                "refusing foreign launcher: {}",
                path.display()
            );
        }
    }
    for name in ["proofstorm", "proofstorm-mcp"] {
        let text = format!(
            "{LAUNCHER_HEADER}export PROOFSTORM_HOME={}\nexec {} \"$@\"\n",
            quote(&work.join("state"))?,
            quote(&target.join("debug").join(name))?
        );
        write_owned(&work.join("bin").join(name), text.as_bytes(), 0o755)?;
    }
    Ok(())
}

fn shell_exit(status: ExitStatus) -> i32 {
    // Ordinary exit/EOF may inherit 130 from a previous interrupted command.
    status.signal().map_or(0, |signal| 128 + signal)
}

fn scrubbed(name: &str) -> bool {
    name.starts_with("PROOFSTORM_")
        || name.starts_with("TRUNK_")
        || matches!(name, "CARGO_TARGET_DIR" | "CARGO_BUILD_TARGET")
}

pub(super) fn shell(source: &Path) -> Result<i32> {
    let work = owned_work(source)?;
    let selected = std::env::var_os("SHELL").map(PathBuf::from);
    let selected = selected
        .filter(|p| p.ends_with("bash") || p.ends_with("zsh"))
        .unwrap_or_else(|| {
            PathBuf::from(if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            })
        });
    let mut command = Command::new(&selected);
    if selected.ends_with("zsh") {
        command
            .args(["-f", "-i"])
            .env("PROMPT", "(proofstorm dev) %~ %# ");
    } else {
        command
            .args(["--noprofile", "--norc", "-i"])
            .env("PS1", "(proofstorm dev) \\w \\$ ");
    }
    for (name, _) in std::env::vars_os() {
        if scrubbed(&name.to_string_lossy()) {
            command.env_remove(name);
        }
    }
    command.env("PROOFSTORM_HOME", work.join("state"));
    let mut paths = vec![work.join("bin")];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    command.env("PATH", std::env::join_paths(paths)?);
    Ok(shell_exit(
        command
            .current_dir(source)
            .status()
            .context("launch development shell")?,
    ))
}

#[cfg(test)]
mod tests;
