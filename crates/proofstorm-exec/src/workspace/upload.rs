//! Bounded staging followed by a controller-recorded atomic file replacement.
use super::{
    Result, atomic_write, atomic_write_mode, bounded_read, local_path, read_json, write_json,
};
use nix::fcntl::{Flock, FlockArg};
use proofstorm_core::workspace::upload::{
    MAX_STAGED_BYTES, MAX_STAGED_UPLOADS, STAGING_CAPACITY_ERROR, UploadRequest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, time::Duration};

fn lock(root: &Path) -> Result<Flock<fs::File>> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".proofstorm/upload.lock"))?;
    Ok(Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, error)| error)?)
}

fn verify(request: &UploadRequest, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != request.bytes
        || format!("{:x}", Sha256::digest(bytes)) != request.sha256
    {
        return Err("workspace upload size or checksum mismatch".into());
    }
    Ok(())
}

// Small terminal receipts live for the cell lifetime, independently of staging
// quotas. Expiring them would let a delayed writer recreate a completed payload.
fn finished(root: &Path, request: &UploadRequest) -> Result<bool> {
    let path = root
        .join(".proofstorm/upload-finished")
        .join(&request.upload_id);
    if !path.exists() {
        return Ok(false);
    }
    if read_json(&path)? != json!(request) {
        return Err("workspace upload identity conflict".into());
    }
    Ok(true)
}

fn finish_locked(root: &Path, request: &UploadRequest) -> Result<Value> {
    let receipts = root.join(".proofstorm/upload-finished");
    fs::create_dir_all(&receipts)?;
    if !finished(root, request)? {
        write_json(&receipts.join(&request.upload_id), &json!(request))?;
    }
    let directory = root.join(".proofstorm/uploads").join(&request.upload_id);
    match fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(json!(request))
}

pub(super) fn finish(root: &Path, request: &UploadRequest) -> Result<Value> {
    request.validate()?;
    let _lock = lock(root)?;
    finish_locked(root, request)
}

pub(super) fn stage(root: &Path, request: &UploadRequest, input: &mut impl Read) -> Result<Value> {
    request.validate()?;
    let bytes = bounded_read(input, request.bytes)?;
    verify(request, &bytes)?;
    let _lock = lock(root)?;
    if finished(root, request)? {
        // Consume and verify the transfer, but never reserve space again.
        return finish_locked(root, request);
    }
    let uploads = root.join(".proofstorm/uploads");
    fs::create_dir_all(&uploads)?;
    let directory = uploads.join(&request.upload_id);
    let mut count = 0;
    let mut retained = 0;
    for entry in fs::read_dir(&uploads)? {
        let entry = entry?;
        // Abandoned transfers expire; retries can stage the same bytes again.
        if entry.metadata()?.modified()?.elapsed().unwrap_or_default() > Duration::from_secs(3600) {
            fs::remove_dir_all(entry.path())?;
            continue;
        }
        if entry.path() != directory {
            count += 1;
            if let Ok(metadata) = fs::metadata(entry.path().join("payload")) {
                retained += metadata.len();
            }
        }
    }
    if count >= MAX_STAGED_UPLOADS || retained + request.bytes > MAX_STAGED_BYTES {
        return Err(STAGING_CAPACITY_ERROR.into());
    }
    fs::create_dir_all(&directory)?;
    let manifest = directory.join("manifest.json");
    if manifest.exists() && read_json(&manifest)? != json!(request) {
        return Err("workspace upload identity conflict".into());
    }
    write_json(&manifest, &json!(request))?;
    atomic_write(&directory.join("payload"), &bytes)?;
    Ok(json!(request))
}

pub(super) fn commit(root: &Path, request: &UploadRequest) -> Result<Value> {
    request.validate()?;
    let _lock = lock(root)?;
    if finished(root, request)? {
        return Err("workspace upload already finalized".into());
    }
    let directory = root.join(".proofstorm/uploads").join(&request.upload_id);
    if read_json(&directory.join("manifest.json"))? != json!(request) {
        return Err("workspace upload identity conflict".into());
    }
    let result = (|| -> Result<Value> {
        let bytes = bounded_read(
            &mut fs::File::open(directory.join("payload"))?,
            request.bytes,
        )?;
        verify(request, &bytes)?;
        let target = local_path(root, &request.path, true)?;
        atomic_write_mode(
            &target,
            &bytes,
            if request.executable { 0o700 } else { 0o600 },
        )?;
        Ok(
            json!({"path":request.path,"bytes":request.bytes,"sha256":request.sha256,"executable":request.executable}),
        )
    })();
    // Failed commits also release staging. A cleanup error must not hide a
    // successful destination replacement; a terminal retry can finish cleanup.
    let _ = finish_locked(root, request);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u8, bytes: &[u8]) -> UploadRequest {
        UploadRequest {
            upload_id: format!("{id:064x}"),
            path: "src/file".into(),
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            executable: false,
        }
    }

    #[test]
    fn cancellation_releases_capacity_and_fences_late_writers() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".proofstorm")).unwrap();
        let bytes = vec![
            42;
            usize::try_from(proofstorm_core::workspace::upload::MAX_UPLOAD_BYTES)
                .unwrap()
        ];
        for id in 0..2 {
            let request = request(id, &bytes);
            stage(root.path(), &request, &mut bytes.as_slice()).unwrap();
            finish(root.path(), &request).unwrap();
            // A delayed duplicate can arrive after cancellation and even after restart.
            stage(root.path(), &request, &mut bytes.as_slice()).unwrap();
            assert!(
                !root
                    .path()
                    .join(".proofstorm/uploads")
                    .join(&request.upload_id)
                    .exists()
            );
            assert!(commit(root.path(), &request).is_err());
        }
        stage(root.path(), &request(3, b"next"), &mut &b"next"[..]).unwrap();
        assert!(!root.path().join("src/file").exists());
        let early = request(4, b"early");
        finish(root.path(), &early).unwrap();
        stage(root.path(), &early, &mut &b"early"[..]).unwrap();
        assert!(
            !root
                .path()
                .join(".proofstorm/uploads")
                .join(&early.upload_id)
                .exists()
        );
    }

    #[test]
    fn completed_uploads_cannot_restage_or_overwrite_later_edits() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".proofstorm")).unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        let manifest = request(1, b"first");
        stage(root.path(), &manifest, &mut &b"first"[..]).unwrap();
        commit(root.path(), &manifest).unwrap();
        fs::write(root.path().join("src/file"), b"newer").unwrap();
        stage(root.path(), &manifest, &mut &b"first"[..]).unwrap();
        assert!(commit(root.path(), &manifest).is_err());
        assert_eq!(fs::read(root.path().join("src/file")).unwrap(), b"newer");
        assert!(
            !root
                .path()
                .join(".proofstorm/uploads")
                .join(&manifest.upload_id)
                .exists()
        );
        assert!(stage(root.path(), &request(1, b"changed"), &mut &b"changed"[..]).is_err());
    }

    #[test]
    fn failed_commits_release_staging() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".proofstorm")).unwrap();
        fs::create_dir_all(root.path().join("src/file")).unwrap();
        let request = request(1, b"content");
        stage(root.path(), &request, &mut &b"content"[..]).unwrap();
        assert!(commit(root.path(), &request).is_err());
        assert!(
            !root
                .path()
                .join(".proofstorm/uploads")
                .join(&request.upload_id)
                .exists()
        );
    }

    #[test]
    fn staging_is_bounded_and_abandoned_slots_expire() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".proofstorm")).unwrap();
        let manifest = |id: usize| UploadRequest {
            upload_id: format!("{id:064x}"),
            path: "src/file".into(),
            bytes: 0,
            sha256: format!("{:x}", Sha256::digest([])),
            executable: false,
        };
        for id in 0..MAX_STAGED_UPLOADS {
            stage(root.path(), &manifest(id), &mut &b""[..]).unwrap();
        }
        assert!(stage(root.path(), &manifest(MAX_STAGED_UPLOADS), &mut &b""[..]).is_err());
        // Retrying an existing transfer does not reserve a second slot.
        stage(root.path(), &manifest(0), &mut &b""[..]).unwrap();
        let old = root
            .path()
            .join(".proofstorm/uploads")
            .join(&manifest(0).upload_id);
        fs::File::open(&old)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() - Duration::from_secs(3601)),
            )
            .unwrap();
        stage(root.path(), &manifest(MAX_STAGED_UPLOADS), &mut &b""[..]).unwrap();
        assert!(!old.exists());
    }

    #[test]
    fn total_staging_bytes_are_bounded_independently_of_file_count() {
        let root = tempfile::tempdir().unwrap();
        let retained = root.path().join(".proofstorm/uploads/retained");
        fs::create_dir_all(&retained).unwrap();
        fs::File::create(retained.join("payload"))
            .unwrap()
            .set_len(MAX_STAGED_BYTES)
            .unwrap();
        let manifest = UploadRequest {
            upload_id: "1".repeat(64),
            path: "src/file".into(),
            bytes: 1,
            sha256: format!("{:x}", Sha256::digest(b"x")),
            executable: false,
        };
        assert!(stage(root.path(), &manifest, &mut &b"x"[..]).is_err());
        assert!(!root.path().join("src/file").exists());
    }
}
