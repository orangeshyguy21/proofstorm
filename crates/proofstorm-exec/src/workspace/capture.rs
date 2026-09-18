//! Frozen transfer files let an interrupted download resume without recapturing live state.
use super::{Result, atomic_write, bounded_read, local_path, now, read_json};
use proofstorm_core::workspace::evidence::{
    CaptureRequest, CapturedFile, MAX_CAPTURE_BYTES, MAX_CAPTURE_FILE_BYTES, MAX_CAPTURE_FILES,
    TaskCapture,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn path(root: &Path, id: &str) -> PathBuf {
    root.join(".proofstorm/captures").join(format!("{id}.json"))
}

pub(super) fn read(root: &Path, request: &CaptureRequest) -> Result<Vec<u8>> {
    request.validate()?;
    let bytes = regular_bytes(&path(root, &request.capture_id), MAX_CAPTURE_BYTES)?;
    let capture: TaskCapture = serde_json::from_slice(&bytes)?;
    capture.validate()?;
    if capture.capture_id != request.capture_id
        || capture.selection != request.selection
        || capture.capture_request_digest != request.request_digest
    {
        return Err("capture ID already belongs to another selection".into());
    }
    Ok(bytes)
}

pub(super) fn release(root: &Path, id: &str) -> Result<Value> {
    proofstorm_core::workspace::validate_task_id(id)?;
    match fs::remove_file(path(root, id)) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(json!({"released":true}))
}

pub(super) fn freeze(root: &Path, request: &CaptureRequest, state: Value) -> Result<Value> {
    request.validate()?;
    let destination = path(root, &request.capture_id);
    if destination.exists() {
        read(root, request)?;
        return Ok(json!({"capture_id":request.capture_id}));
    }
    let directory = destination.parent().ok_or("capture directory missing")?;
    fs::create_dir_all(directory)?;
    let mut retained = 0;
    let mut count = 0;
    for entry in fs::read_dir(directory)? {
        retained += entry?.metadata()?.len();
        count += 1;
    }
    if count >= 32 || retained > 128 * 1024 * 1024 {
        return Err("pending capture transfers exceed retention limits".into());
    }
    let task = root
        .join(".proofstorm/tasks")
        .join(&request.selection.task_id);
    let mut files = Vec::new();
    let mut size = 0;
    let inputs = task.join("inputs");
    // Older tasks have only their execution copy; validation still requires its
    // digest to match the submitted source rather than inventing missing inputs.
    let source = if inputs.exists() {
        inputs
    } else {
        task.join("source")
    };
    collect_tree(&source, "source", &mut files, &mut size)?;
    let controls = task.join("control");
    if controls.exists() {
        collect_tree(&controls, "control", &mut files, &mut size)?;
    }
    for output in &request.selection.output_paths {
        let selected = local_path(
            root,
            &format!("output/{}/{output}", request.selection.task_id),
            false,
        )?;
        add_file(&selected, format!("output/{output}"), &mut files, &mut size)?;
    }
    if request.selection.include_logs {
        for stream in ["stdout", "stderr"] {
            for suffix in ["", ".previous"] {
                let name = format!("{stream}.log{suffix}");
                let file = task.join(&name);
                if file.exists() {
                    add_file(&file, format!("logs/{name}"), &mut files, &mut size)?;
                }
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let capture = TaskCapture {
        capture_id: request.capture_id.clone(),
        capture_request_digest: request.request_digest.clone(),
        selection: request.selection.clone(),
        observed_at_unix: now(),
        task: state,
        request: serde_json::from_value(read_json(&task.join("task.json"))?)?,
        files,
    };
    capture.validate()?;
    let bytes = serde_json::to_vec(&capture)?;
    if bytes.len() > MAX_CAPTURE_BYTES || retained + bytes.len() as u64 > 128 * 1024 * 1024 {
        return Err("capture exceeds transfer or retention byte limit".into());
    }
    atomic_write(&destination, &bytes)?;
    Ok(json!({"capture_id":request.capture_id}))
}

fn collect_tree(
    root: &Path,
    prefix: &str,
    files: &mut Vec<CapturedFile>,
    size: &mut usize,
) -> Result<()> {
    let mut pending = vec![(root.to_path_buf(), prefix.to_owned(), 0)];
    let mut entries = 0;
    while let Some((path, name, depth)) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        entries += 1;
        if entries > MAX_CAPTURE_FILES || depth > 32 {
            return Err("capture has too many entries or directory levels".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let child = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "capture filenames must be UTF-8")?;
                pending.push((entry.path(), format!("{name}/{child}"), depth + 1));
            }
        } else {
            add_file(&path, name, files, size)?;
        }
    }
    Ok(())
}

fn add_file(
    path: &Path,
    name: String,
    files: &mut Vec<CapturedFile>,
    size: &mut usize,
) -> Result<()> {
    if files.len() >= MAX_CAPTURE_FILES {
        return Err("capture has too many files".into());
    }
    let bytes = regular_bytes(path, MAX_CAPTURE_FILE_BYTES.saturating_sub(*size))?;
    *size += bytes.len();
    files.push(CapturedFile::from_bytes(
        name,
        fs::symlink_metadata(path)?.permissions().mode() & 0o111,
        &bytes,
    ));
    Ok(())
}

fn regular_bytes(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.len() > maximum as u64 {
        return Err(
            "capture requires bounded regular files; links and special files are refused".into(),
        );
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits());
    }
    let mut file = options.open(path)?;
    let opened = file.metadata()?;
    if opened.ino() != before.ino() || opened.dev() != before.dev() || !opened.is_file() {
        return Err("capture file changed while opening".into());
    }
    let bytes = bounded_read(&mut file, maximum as u64)?;
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || bytes.len() as u64 != after.len()
    {
        return Err("capture file changed while reading; retry when its writer is idle".into());
    }
    Ok(bytes)
}
