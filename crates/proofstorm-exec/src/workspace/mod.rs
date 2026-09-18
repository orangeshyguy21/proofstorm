//! A cell-owned supervisor with a small local control socket.
//! Workspace code shares a user and volume: this is a lifecycle boundary, not a sandbox between tasks.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use proofstorm_core::workspace::{
    FileRequest, MAX_FILE_WRITE_BYTES, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, validate_path, wire,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod capture;
mod control;
#[cfg(target_os = "linux")]
mod manager;
#[cfg(test)]
mod tests;
#[cfg(target_os = "linux")]
mod upload;

pub(super) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const READ_BYTES: u64 = 1024;

pub(super) fn entry(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mode = args.next().ok_or("workspace mode missing")?;
    #[cfg(target_os = "linux")]
    let root = std::env::var_os("PROOFSTORM_WORKSPACE_ROOT")
        .map_or_else(|| PathBuf::from("/workspace"), PathBuf::from);
    match mode.as_str() {
        "install" => {
            let path = Path::new("/opt/proofstorm/workspace");
            fs::copy(std::env::current_exe()?, path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o555))?;
        }
        "request" => {
            let request = args.next().ok_or("workspace request missing")?;
            if request.len() > wire::MAX_MESSAGE_BYTES {
                return Err("workspace request too large".into());
            }
            let request: Value = serde_json::from_str(&request)?;
            wire::decode(request.clone())?;
            // Forward the compact form; re-encoding decoded text would expand it again.
            let reply = exchange(&request)?;
            println!("{}", serde_json::to_string(&reply)?);
            if reply.get("error").is_some() {
                return Err("workspace request refused".into());
            }
        }
        "bridge" => {
            let request = args.next().ok_or("bridge request missing")?;
            let request: proofstorm_core::workspace::control::BridgeRequest =
                serde_json::from_str(&request)?;
            request.validate()?;
            println!("{}", exchange(&json!({"kind":"bridge","request":request}))?);
        }
        "call" => control::call(&args.next().ok_or("control call missing")?)?,
        #[cfg(target_os = "linux")]
        "upload" => {
            let encoded = args.next().ok_or("upload metadata missing")?;
            if encoded.len() > wire::MAX_MESSAGE_BYTES {
                return Err("upload metadata too large".into());
            }
            let request = serde_json::from_str(&encoded)?;
            let receipt = upload::stage(&root, &request, &mut std::io::stdin().lock())?;
            println!("{receipt}");
        }
        #[cfg(target_os = "linux")]
        "upload-finish" => {
            let encoded = args.next().ok_or("upload metadata missing")?;
            if encoded.len() > wire::MAX_MESSAGE_BYTES {
                return Err("upload metadata too large".into());
            }
            let request = serde_json::from_str(&encoded)?;
            println!("{}", upload::finish(&root, &request)?);
        }
        #[cfg(target_os = "linux")]
        "capture" => {
            let request: proofstorm_core::workspace::evidence::CaptureRequest =
                serde_json::from_str(&args.next().ok_or("capture request missing")?)?;
            request.validate()?;
            let reply = exchange(&serde_json::to_value(
                proofstorm_core::workspace::WorkspaceRequest::Capture(request.clone()),
            )?)?;
            if reply.get("error").is_some() {
                return Err("workspace capture refused; inspect file selection, source integrity and capture limits".into());
            }
            std::io::stdout().write_all(&capture::read(&root, &request)?)?;
        }
        #[cfg(target_os = "linux")]
        "serve" => manager::serve(&root)?,
        #[cfg(target_os = "linux")]
        "run" => {
            let directory = PathBuf::from(args.next().ok_or("task directory missing")?);
            crate::linux::run_workspace(&directory)?;
        }
        _ => return Err("unknown workspace mode".into()),
    }
    Ok(())
}

fn exchange(request: &Value) -> Result<Value> {
    let bytes = serde_json::to_vec(request)?;
    let max = proofstorm_core::workspace::control::MAX_BRIDGE_BYTES;
    if bytes.len() as u64 > max {
        return Err("workspace request too large".into());
    }
    let mut stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    stream.write_all(&bytes)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    Ok(serde_json::from_slice(&bounded_read(&mut stream, max)?)?)
}

pub(super) fn socket_path() -> PathBuf {
    std::env::var_os("PROOFSTORM_WORKSPACE_SOCKET").map_or_else(
        || PathBuf::from("/tmp/proofstorm-workspace.sock"),
        PathBuf::from,
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn bounded_read(reader: &mut impl Read, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err("workspace data exceeds limit".into());
    }
    Ok(bytes)
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&bounded_read(
        &mut fs::File::open(path)?,
        65536,
    )?)?)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_mode(path, bytes, 0o600)
}

fn atomic_write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let temporary = path.with_file_name(format!(
        ".write-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(path.parent().ok_or("file parent missing")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    atomic_write(path, &serde_json::to_vec(value)?)
}

/// Reject symlinks and special files. Tasks intentionally retain ordinary native filesystem access.
fn local_path(root: &Path, relative: &str, create_parents: bool) -> Result<PathBuf> {
    validate_path(relative)?;
    let parts: Vec<_> = relative.split('/').collect();
    let mut path = root.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        let parent = index + 1 < parts.len();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() || (parent && !metadata.is_dir()) => {
                return Err("workspace path contains a symlink or non-directory parent".into());
            }
            Ok(_) => (),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && parent && create_parents =>
            {
                fs::create_dir(&path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !parent => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

fn file_request(root: &Path, request: &FileRequest) -> Result<Value> {
    match request {
        FileRequest::Write { path, content } => {
            if content.len() > MAX_FILE_WRITE_BYTES {
                return Err("file write exceeds 8192 bytes".into());
            }
            let target = local_path(root, path, true)?;
            atomic_write(&target, content.as_bytes())?;
            Ok(
                json!({"path":path,"bytes":content.len(),"sha256":format!("{:x}", Sha256::digest(content.as_bytes()))}),
            )
        }
        FileRequest::Read { path, offset } => {
            let target = local_path(root, path, false)?;
            let metadata = fs::symlink_metadata(&target)?;
            if !metadata.is_file() {
                return Err("only regular files can be read".into());
            }
            let mut file = fs::File::open(target)?;
            file.seek(SeekFrom::Start(*offset))?;
            let mut bytes = Vec::new();
            file.take(READ_BYTES).read_to_end(&mut bytes)?;
            if let Err(error) = std::str::from_utf8(&bytes) {
                if error.error_len().is_none()
                    && offset.saturating_add(bytes.len() as u64) < metadata.len()
                {
                    bytes.truncate(error.valid_up_to());
                }
            }
            let next = offset.saturating_add(bytes.len() as u64);
            Ok(
                json!({"path":path,"offset":offset,"bytes":bytes.len(),"content":String::from_utf8_lossy(&bytes),"next_offset":(next < metadata.len()).then_some(next)}),
            )
        }
        FileRequest::List { path, after } => {
            let directory = if path.is_empty() {
                root.to_path_buf()
            } else {
                local_path(root, path, false)?
            };
            let mut names = Vec::new();
            for item in fs::read_dir(directory)? {
                let item = item?;
                let name = item
                    .file_name()
                    .into_string()
                    .map_err(|_| "non-UTF-8 filename")?;
                if name != ".proofstorm" && after.as_ref().is_none_or(|after| name > *after) {
                    names.push(name);
                }
                if names.len() > 4096 {
                    return Err("directory too large to list".into());
                }
            }
            names.sort();
            let next = (names.len() > 16).then(|| names[15].clone());
            names.truncate(16);
            Ok(json!({"path":path,"entries":names,"next_after":next}))
        }
        FileRequest::Remove { path } => {
            let target = local_path(root, path, false)?;
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.is_file() => fs::remove_file(&target)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                _ => return Err("only regular files can be removed".into()),
            }
            Ok(json!({"path":path,"removed":true}))
        }
    }
}

fn snapshot(source: &Path, destination: &Path) -> Result<String> {
    let mut pending = vec![PathBuf::new()];
    let mut manifest = BTreeMap::new();
    let mut size = 0;
    let mut entries = 0;
    while let Some(relative) = pending.pop() {
        let from = source.join(&relative);
        let to = destination.join(&relative);
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.is_symlink() {
            return Err("source snapshots cannot contain symlinks".into());
        }
        if metadata.is_dir() {
            fs::create_dir(&to)?;
            for child in fs::read_dir(&from)? {
                let name = child?.file_name();
                if name == ".proofstorm" {
                    return Err("source contains reserved supervisor state".into());
                }
                entries += 1;
                if entries > MAX_SOURCE_FILES || relative.components().count() >= 32 {
                    return Err("source snapshot contains too many files or levels".into());
                }
                pending.push(relative.join(name));
            }
        } else if metadata.is_file() {
            size += metadata.len();
            if size > MAX_SOURCE_BYTES {
                return Err("source snapshot exceeds 16 MiB".into());
            }
            let bytes = bounded_read(&mut fs::File::open(&from)?, metadata.len())?;
            let executable = metadata.permissions().mode() & 0o111;
            fs::write(&to, &bytes)?;
            fs::set_permissions(&to, fs::Permissions::from_mode(0o600 | executable))?;
            let path = relative.to_str().ok_or("source filenames must be UTF-8")?;
            manifest.insert(
                path.to_owned(),
                json!({"sha256":format!("{:x}", Sha256::digest(&bytes)),"mode":executable}),
            );
        } else {
            return Err("source snapshots only support directories and regular files".into());
        }
    }
    Ok(proofstorm_core::digest_json(&manifest))
}
