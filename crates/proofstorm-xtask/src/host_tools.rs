//! Host-tool pin resolution and maintainer installs. Setup shares the pin model.
use crate::development::{directory, future_canonical, regular};
use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use proofstorm_core::tool_pins::{self, Pins, Tool};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
};

const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    regular(path)?;
    ensure!(
        fs::metadata(path)?.len() <= limit,
        "tool input exceeds size limit"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "tool input exceeds size limit");
    Ok(bytes)
}

fn fetch(url: &str, path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(url.starts_with("https://"), "tool source must use HTTPS");
    let output = Command::new("curl")
        .args([
            "-q",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "5",
            "--retry",
            "3",
            "--max-time",
            "180",
            "--max-filesize",
            &limit.to_string(),
            "--silent",
            "--show-error",
            url,
            "--output",
        ])
        .arg(path)
        .output()?;
    ensure!(
        output.status.success(),
        "host-tool download failed; no installed tool was replaced"
    );
    read(path, limit)
}

fn checksum(bytes: &[u8], name: Option<&str>) -> Result<String> {
    let encoded = std::str::from_utf8(bytes)?;
    let rows: Vec<Vec<&str>> = encoded
        .lines()
        .map(|line| line.split_whitespace().collect())
        .filter(|row: &Vec<&str>| !row.is_empty())
        .filter(|row| {
            name.is_none_or(|name| row.len() == 2 && row[1].trim_start_matches('*') == name)
        })
        .collect();
    ensure!(
        rows.len() == 1 && rows[0].len() == if name.is_some() { 2 } else { 1 },
        "ambiguous publisher checksum"
    );
    ensure!(tool_pins::digest(rows[0][0]), "invalid publisher checksum");
    Ok(rows[0][0].into())
}

fn executable(payload: &[u8], member: Option<&str>) -> Result<Vec<u8>> {
    let Some(member) = member else {
        return Ok(payload.to_vec());
    };
    let reader = GzDecoder::new(payload).take(512 * 1024 * 1024);
    let mut archive = tar::Archive::new(reader);
    let mut selected = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?;
        ensure!(
            name.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe tool archive path"
        );
        if name == Path::new(member) {
            ensure!(
                entry.header().entry_type().is_file() && selected.is_none(),
                "tool archive member must be one regular file"
            );
            ensure!(
                entry.size() > 0 && entry.size() <= MAX_DOWNLOAD,
                "invalid executable size"
            );
            let expected = entry.size();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 == expected, "truncated tool executable");
            selected = Some(bytes);
        }
    }
    let mut reader = archive.into_inner();
    std::io::copy(&mut reader, &mut std::io::sink())?;
    ensure!(
        reader.limit() > 0,
        "tool archive exceeds expanded size limit"
    );
    selected.context("tool archive member missing")
}

fn versions(root: &Path) -> Result<BTreeMap<String, String>> {
    let encoded = String::from_utf8(read(&root.join("tools/versions.env"), 1024 * 1024)?)?;
    let mut versions = BTreeMap::new();
    for line in encoded
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (key, value) = line.split_once('=').context("invalid tool version line")?;
        ensure!(
            versions.insert(key.into(), value.into()).is_none(),
            "duplicate tool version"
        );
    }
    Ok(versions)
}

fn resolve(root: &Path, target: &str, output: &Path) -> Result<()> {
    tool_pins::host_parts(target).map_err(anyhow::Error::msg)?;
    let output = future_canonical(&if output.is_absolute() {
        output.to_owned()
    } else {
        std::env::current_dir()?.join(output)
    })?;
    ensure!(
        fs::symlink_metadata(&output).is_err(),
        "pin output must be new; review it before replacing a shipped manifest"
    );
    let versions = versions(root)?;
    let work = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
    let mut pins = Pins {
        format_version: 1,
        target: target.into(),
        tools: vec![],
    };
    for (name, key) in [
        ("k3d", "K3D_VERSION"),
        ("kubectl", "KUBECTL_VERSION"),
        ("helm", "HELM_VERSION"),
    ] {
        let version = versions.get(key).context("missing tool version")?;
        let source = tool_pins::source(name, version, target).map_err(anyhow::Error::msg)?;
        let receipt = fetch(
            &source.checksum_url,
            &work.path().join("checksum"),
            1024 * 1024,
        )?;
        let expected = checksum(&receipt, source.checksum_name.as_deref())?;
        let payload = fetch(&source.url, &work.path().join("payload"), MAX_DOWNLOAD)?;
        ensure!(hash(&payload) == expected, "publisher checksum mismatch");
        let binary = executable(&payload, source.archive_member.as_deref())?;
        pins.tools.push(Tool {
            name: name.into(),
            version: version.clone(),
            url: source.url,
            sha256: expected,
            executable_sha256: hash(&binary),
            archive_member: source.archive_member,
        });
    }
    pins.validate(target).map_err(anyhow::Error::msg)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    file.write_all(&serde_json::to_vec_pretty(&pins)?)?;
    file.sync_all()?;
    println!(
        "Verified candidate pins: {}. Shipped manifests were not changed.",
        output.display()
    );
    Ok(())
}

fn checked(root: &Path, target: &str) -> Result<Pins> {
    let filename = match target {
        tool_pins::MAC_ARM64 => "bootstrap-tools.json",
        tool_pins::LINUX_AMD64 => "bootstrap-tools-linux-amd64.json",
        _ => bail!("unsupported host-tool target"),
    };
    let pins = Pins::parse(
        target,
        &String::from_utf8(read(&root.join("release").join(filename), 1024 * 1024)?)?,
    )
    .map_err(anyhow::Error::msg)?;
    let versions = versions(root)?;
    for tool in &pins.tools {
        ensure!(
            versions.get(&format!("{}_VERSION", tool.name.to_uppercase())) == Some(&tool.version),
            "tool versions and reviewed pins differ; resolve and review new pins first"
        );
    }
    Ok(pins)
}

fn install(root: &Path, target: &str) -> Result<()> {
    install_with(root, target, &mut fetch)
}

fn install_with(
    root: &Path,
    target: &str,
    download: &mut impl FnMut(&str, &Path, u64) -> Result<Vec<u8>>,
) -> Result<()> {
    let pins = checked(root, target)?;
    let destination = root.join(".tools/bin");
    directory(&destination)?;
    // Verify every existing executable before any download. Never adopt by filename.
    for tool in &pins.tools {
        let path = destination.join(&tool.name);
        if fs::symlink_metadata(&path).is_ok() {
            ensure!(
                hash(&read(&path, MAX_DOWNLOAD)?) == tool.executable_sha256
                    && fs::metadata(&path)?.permissions().mode() & 0o111 != 0,
                "{} does not match the reviewed pin; remove only that file before retrying",
                path.display()
            );
        }
    }
    let work = tempfile::tempdir_in(&destination)?;
    for tool in &pins.tools {
        let path = destination.join(&tool.name);
        if path.exists() {
            continue;
        }
        println!("Downloading reviewed {} {}...", tool.name, tool.version);
        let payload = download(&tool.url, &work.path().join("payload"), MAX_DOWNLOAD)?;
        ensure!(
            hash(&payload) == tool.sha256,
            "tool download checksum mismatch"
        );
        let bytes = executable(&payload, tool.archive_member.as_deref())?;
        ensure!(
            hash(&bytes) == tool.executable_sha256,
            "tool executable checksum mismatch"
        );
        let temporary = work.path().join(&tool.name);
        fs::write(&temporary, bytes)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
        fs::hard_link(temporary, path)?;
    }
    println!(
        "Maintainer tools verified in {}. Installation runtimes unchanged.",
        destination.display()
    );
    Ok(())
}

pub(super) fn cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<_> = args
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("UTF-8 arguments required"))
        })
        .collect::<Result<_>>()?;
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["install", root] => install(
            &Path::new(root).canonicalize()?,
            match (std::env::consts::OS, std::env::consts::ARCH) {
                ("macos", "aarch64") => tool_pins::MAC_ARM64,
                ("linux", "x86_64") => tool_pins::LINUX_AMD64,
                _ => bail!("maintainer tools support macOS Apple Silicon and Linux x86-64"),
            },
        ),
        ["resolve", root, target, output] => {
            resolve(&Path::new(root).canonicalize()?, target, Path::new(output))
        }
        ["check", root, target] => {
            checked(&Path::new(root).canonicalize()?, target)?;
            Ok(())
        }
        _ => bail!(
            "expected host-tools install ROOT, resolve ROOT TARGET NEW_OUTPUT, or check ROOT TARGET"
        ),
    }
}

#[cfg(test)]
mod tests;
