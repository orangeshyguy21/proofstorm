use super::{alpha_version, archive::output_path, bundle, text};
use crate::development::{directory, inventory, regular};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn host_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => bail!(
            "build on macOS Apple Silicon or Linux x86-64; cross-compilation is not supported"
        ),
    }
}

fn git(source: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()?;
    ensure!(
        output.status.success(),
        "Git source inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn names(source: &Path) -> Result<BTreeSet<String>> {
    String::from_utf8(git(
        source,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?)?
    .split('\0')
    .filter(|name| !name.is_empty())
    .map(|name| {
        ensure!(bundle::safe_name(name), "unsafe source path");
        Ok(name.to_owned())
    })
    .collect()
}

fn fingerprint(root: &Path, names: &BTreeSet<String>) -> Result<String> {
    let mut tree = Sha256::new();
    for name in names {
        let path = root.join(name);
        regular(&path)?;
        let mode = if fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
            0o644
        } else {
            0o755
        };
        tree.update(format!("{name}\0{mode}\0").as_bytes());
        let mut file = fs::File::open(path)?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        tree.update(digest.finalize());
    }
    Ok(format!("{:x}", tree.finalize()))
}

fn source_names(source: &Path, imported: bool) -> Result<BTreeSet<String>> {
    if imported {
        return Ok(inventory(source)?.into_keys().collect());
    }
    Ok(names(source)?
        .into_iter()
        .filter(|name| {
            !fs::symlink_metadata(source.join(name))
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        })
        .collect())
}

fn snapshot(
    source: &Path,
    destination: &Path,
    allow_dirty: bool,
    imported: Option<&Value>,
) -> Result<Value> {
    let (selected, revision, status): (BTreeSet<String>, String, Vec<u8>) =
        if let Some(provenance) = imported {
            ensure!(
                provenance["dirty"].is_boolean(),
                "invalid transported dirty flag"
            );
            (
                source_names(source, true)?,
                text(provenance, "revision")?.to_owned(),
                Vec::new(),
            )
        } else {
            let status = git(source, &["status", "--porcelain"])?;
            ensure!(
                allow_dirty || status.is_empty(),
                "release requires a clean committed source tree"
            );
            let selected = source_names(source, false)?;
            (
                selected,
                String::from_utf8(git(source, &["rev-parse", "HEAD"])?)?
                    .trim()
                    .to_owned(),
                status,
            )
        };
    let before = fingerprint(source, &selected)?;
    if let Some(provenance) = imported {
        ensure!(
            text(provenance, "sha256")? == before,
            "transported source checksum mismatch"
        );
        ensure!(
            allow_dirty || provenance["dirty"] == false,
            "release requires a clean committed source tree"
        );
    }
    directory(destination)?;
    for name in &selected {
        let original = source.join(name);
        let output = destination.join(name);
        directory(output.parent().context("missing snapshot parent")?)?;
        regular(&original)?;
        fs::copy(&original, &output)?;
        fs::set_permissions(
            &output,
            fs::Permissions::from_mode(
                if fs::metadata(original)?.permissions().mode() & 0o111 == 0 {
                    0o644
                } else {
                    0o755
                },
            ),
        )?;
    }
    let sha = fingerprint(destination, &selected)?;
    ensure!(
        sha == before && fingerprint(source, &selected)? == before,
        "source changed during snapshot; retry"
    );
    ensure!(
        source_names(source, imported.is_some())? == selected,
        "source file list changed during snapshot; retry"
    );
    if imported.is_none() {
        ensure!(
            String::from_utf8(git(source, &["rev-parse", "HEAD"])?)?.trim() == revision
                && git(source, &["status", "--porcelain"])? == status,
            "source changed during snapshot; retry"
        );
    }
    Ok(
        json!({"revision": revision, "sha256": sha, "dirty": imported.map_or(!status.is_empty(), |p| p["dirty"] == true)}),
    )
}

fn workspace_alpha(source: &Path) -> Result<bool> {
    regular(&source.join("Cargo.toml"))?;
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(source.join("Cargo.toml"))?)?;
    let version = manifest
        .get("workspace")
        .and_then(|v| v.get("package"))
        .and_then(|v| v.get("version"))
        .and_then(toml::Value::as_str)
        .context("missing workspace package version")?;
    Ok(alpha_version(version))
}

fn validate_trunk(source: &Path, trunk: &Path) -> Result<()> {
    regular(trunk).context("install the pinned Trunk tool before packaging")?;
    regular(&source.join("tools/versions.env"))?;
    let pins = fs::read_to_string(source.join("tools/versions.env"))?;
    let versions: Vec<_> = pins
        .lines()
        .filter_map(|line| line.strip_prefix("TRUNK_VERSION="))
        .collect();
    ensure!(
        versions.len() == 1,
        "missing or duplicate Trunk version pin"
    );
    let result = Command::new(trunk).arg("--version").output()?;
    ensure!(
        result.status.success()
            && String::from_utf8(result.stdout)?.trim() == format!("trunk {}", versions[0]),
        "Trunk version does not match tools/versions.env"
    );
    Ok(())
}

fn prepare(values: &[OsString]) -> Result<Vec<String>> {
    ensure!(
        values.len() == 8,
        "release-prepare expects eight build-plan arguments"
    );
    let source = PathBuf::from(&values[0]).canonicalize()?;
    let work = output_path(Path::new(&values[1]))?;
    let output = output_path(Path::new(&values[2]))?;
    let target = if values[3].is_empty() {
        work.join("target")
    } else {
        output_path(Path::new(&values[3]))?
    };
    let trunk = if values[4].is_empty() {
        source.join(".tools/bin/trunk")
    } else {
        PathBuf::from(&values[4])
    }
    .canonicalize()
    .context("install the pinned Trunk tool before packaging")?;
    let imported = if values[5].is_empty() {
        None
    } else {
        Some(bundle::read_json(Path::new(&values[5]))?)
    };
    let development: bool = values[6]
        .to_str()
        .context("invalid development flag")?
        .parse()?;
    let debug: bool = values[7].to_str().context("invalid debug flag")?.parse()?;
    let host = host_target()?;
    ensure!(
        fs::symlink_metadata(&work).is_err(),
        "work directory must not already exist"
    );
    ensure!(
        !work.starts_with(&source) && !output.starts_with(&source) && !target.starts_with(&source),
        "build and bundle outputs must be outside the development checkout"
    );
    ensure!(
        !output.starts_with(work.join("source")) && !target.starts_with(work.join("source")),
        "build outputs must be outside the source snapshot"
    );
    let alpha = workspace_alpha(&source)?;
    ensure!(
        development || alpha || !debug,
        "debug binaries require an alpha or development build"
    );
    validate_trunk(&source, &trunk)?;
    let parent = work.parent().context("work directory has no parent")?;
    directory(parent)?;
    // Failure only removes this private staging directory, never a caller path.
    let stage = tempfile::Builder::new()
        .prefix(".proofstorm-source-")
        .tempdir_in(parent)?;
    let provenance = snapshot(
        &source,
        &stage.path().join("source"),
        development || alpha,
        imported.as_ref(),
    )?;
    ensure!(
        workspace_alpha(&stage.path().join("source"))? == alpha,
        "source version changed during snapshot"
    );
    validate_trunk(&stage.path().join("source"), &trunk)?;
    fs::write(
        stage.path().join("source.json"),
        serde_json::to_vec_pretty(&provenance)?,
    )?;
    // Reserve work atomically rather than renaming over an existing directory.
    fs::create_dir(&work)?;
    fs::rename(stage.path().join("source"), work.join("source"))?;
    fs::rename(stage.path().join("source.json"), work.join("source.json"))?;
    let plan = vec![work.join("source"), work.clone(), output, target, trunk]
        .into_iter()
        .map(|path| {
            path.into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("build paths must be UTF-8"))
        })
        .chain([
            Ok(text(&provenance, "revision")?.to_owned()),
            Ok(text(&provenance, "sha256")?.to_owned()),
            Ok(host.to_owned()),
        ])
        .collect::<Result<Vec<_>>>()?;
    fs::write(
        work.join("build-plan.json"),
        serde_json::to_vec_pretty(
            &json!({"source": plan[0], "work": plan[1], "output": plan[2],
        "target": plan[3], "trunk": plan[4], "expected_target": plan[7], "development": development, "debug": debug}),
        )?,
    )?;
    Ok(plan)
}

pub(super) fn cli(command: &str, args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<_> = args.collect();
    match command {
        "linux-build-prepare" => {
            ensure!(
                args.len() == 4,
                "expected source, new work directory, development and debug booleans"
            );
            let development = args[2]
                .to_str()
                .context("invalid development flag")?
                .parse()?;
            let debug = args[3].to_str().context("invalid debug flag")?.parse()?;
            for value in
                linux_prepare(Path::new(&args[0]), Path::new(&args[1]), development, debug)?
            {
                std::io::stdout().write_all(value.as_bytes())?;
                std::io::stdout().write_all(&[0])?;
            }
        }
        "release-worker-prepare" => {
            ensure!(
                args.len() == 3,
                "expected input, new work directory, and new output directory"
            );
            let plan = worker_prepare(
                Path::new(&args[0]),
                Path::new(&args[1]),
                Path::new(&args[2]),
            )?;
            for value in plan {
                std::io::stdout().write_all(value.as_bytes())?;
                std::io::stdout().write_all(&[0])?;
            }
        }
        "release-prepare" => {
            let plan = prepare(&args)?;
            let mut stdout = std::io::stdout().lock();
            for value in plan {
                stdout.write_all(value.as_bytes())?;
                stdout.write_all(&[0])?;
            }
        }
        "release-host-check" => {
            ensure!(args.len() == 2, "expected metadata file and host target");
            let info = bundle::read_json(Path::new(&args[0]))?;
            ensure!(
                text(&info, "target")? == args[1].to_str().context("invalid host target")?
                    && text(&info, "target")? == host_target()?,
                "build target differs from build host"
            );
        }
        _ => bail!("unknown release build command"),
    }
    Ok(())
}

fn worker_prepare(input: &Path, work: &Path, output: &Path) -> Result<Vec<String>> {
    let input = input.canonicalize()?;
    let work = output_path(work)?;
    let output = output_path(output)?;
    ensure!(
        !work.starts_with(&input) && !output.starts_with(&input),
        "worker outputs must be outside transported inputs"
    );
    ensure!(
        !output.starts_with(&work) && !work.starts_with(&output),
        "worker and artifact directories must be separate"
    );
    ensure!(
        fs::symlink_metadata(&work).is_err() && fs::symlink_metadata(&output).is_err(),
        "worker output directories must be new"
    );
    let options = bundle::read_json(&input.join("options.json"))?;
    let development = options["development"]
        .as_bool()
        .context("invalid development option")?;
    let debug = options["debug"].as_bool().context("invalid debug option")?;
    let provenance = bundle::read_json(&input.join("source.json"))?;
    let source = input.join("source");
    let allow_dirty = development || workspace_alpha(&source)?;
    let parent = work.parent().context("missing work parent")?;
    directory(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".proofstorm-worker-")
        .tempdir_in(parent)?;
    // Reuse the full filename/mode/content fingerprint and owned-file copying.
    // The separate copy may acquire downloaded tools; transport stays pristine.
    snapshot(
        &source,
        &stage.path().join("source"),
        allow_dirty,
        Some(&provenance),
    )?;
    fs::create_dir(&work)?;
    fs::rename(stage.path().join("source"), work.join("source"))?;
    Ok(vec![
        work.to_str().context("non-UTF-8 work path")?.into(),
        output.to_str().context("non-UTF-8 output path")?.into(),
        development.to_string(),
        debug.to_string(),
    ])
}

fn linux_prepare(
    source: &Path,
    work: &Path,
    development: bool,
    debug: bool,
) -> Result<Vec<String>> {
    let source = source.canonicalize()?;
    let work = output_path(work)?;
    ensure!(!work.starts_with(&source), "build outside the checkout");
    ensure!(
        fs::symlink_metadata(&work).is_err(),
        "work directory must be new"
    );
    let alpha = workspace_alpha(&source)?;
    ensure!(
        development || alpha || !debug,
        "debug binaries require an alpha or development build"
    );
    let parent = work.parent().context("missing work parent")?;
    let stage = tempfile::Builder::new()
        .prefix("proofstorm-linux-build-")
        .tempdir_in(parent)?;
    let name = stage
        .path()
        .file_name()
        .context("missing container name")?
        .to_string_lossy()
        .to_ascii_lowercase();
    let inputs = stage.path().join("input");
    fs::create_dir(&inputs)?;
    let provenance = snapshot(&source, &inputs.join("source"), development || alpha, None)?;
    fs::write(
        inputs.join("source.json"),
        serde_json::to_vec_pretty(&provenance)?,
    )?;
    fs::write(
        inputs.join("options.json"),
        serde_json::to_vec_pretty(&json!({"debug":debug,"development":development}))?,
    )?;
    let dockerfile = inputs.join("source/docker/release/Dockerfile.linux-builder");
    regular(&dockerfile)?;
    let digest = bundle::checksum(&dockerfile, fs::metadata(&dockerfile)?.len())?;
    let tag = format!("proofstorm-linux-builder:{}", &digest[..16]);
    let context = stage.path().join("toolchain");
    fs::create_dir(&context)?;
    fs::copy(dockerfile, context.join("Dockerfile"))?;
    fs::write(
        stage.path().join("run.json"),
        serde_json::to_vec_pretty(&json!({
            "container":name,"toolchain_image":tag,"platform":"linux/amd64","source":provenance,
            "privileged":false,"host_mounts":[],"cpus":2,"memory":"3g"
        }))?,
    )?;
    // All checks pass before reserving the caller's output. Never overwrite it.
    fs::create_dir(&work)?;
    for entry in ["input", "toolchain", "run.json"] {
        fs::rename(stage.path().join(entry), work.join(entry))?;
    }
    Ok(vec![
        work.to_str().context("non-UTF-8 work path")?.into(),
        name,
        tag,
    ])
}

#[cfg(test)]
mod tests;
