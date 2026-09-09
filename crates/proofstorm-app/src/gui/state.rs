use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fmt::Write as _,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) const RECORD: &str = "gui-process.json";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub format_version: u32,
    pub installation_id: String,
    pub instance: String,
    pub token: String,
    pub executable: PathBuf,
    pub pid: u32,
    pub port: u16,
}

impl Record {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    pub fn health(&self) -> serde_json::Value {
        serde_json::json!({"installation_id":self.installation_id,"instance":self.instance,
            "pid":self.pid,"executable":self.executable,"format_version":self.format_version})
    }
}

pub(super) fn random<const N: usize>() -> Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("GUI identity generation failed: {e}"))?;
    let mut value = String::with_capacity(N * 2);
    for byte in bytes {
        write!(&mut value, "{byte:02x}")?;
    }
    Ok(value)
}

#[allow(
    clippy::verbose_bit_mask,
    reason = "keep the private-file permission check expressed in octal mode bits"
)]
pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.len() <= 65536
            && meta.permissions().mode() & 0o077 == 0,
        "refusing non-private or linked GUI state: {}",
        path.display()
    );
    Ok(Some(fs::read(path)?))
}

pub(super) fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    read(path)?;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("GUI state parent missing")?)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub(super) fn record(home: &Path, installation: &str) -> Result<Option<Record>> {
    read(&home.join(RECORD))?
        .map(|bytes| {
            let record: Record = serde_json::from_slice(&bytes)
                .context("invalid GUI owner record; refusing adoption")?;
            ensure!(
                record.format_version == 1
                    && record.installation_id == installation
                    && record.instance.len() == 32
                    && record.token.len() == 64
                    && record
                        .instance
                        .bytes()
                        .chain(record.token.bytes())
                        .all(|b| b.is_ascii_hexdigit())
                    && record.executable.is_absolute(),
                "GUI owner record belongs to a different installation or format"
            );
            Ok(record)
        })
        .transpose()
}

pub(super) fn remove_owned(home: &Path, expected: &Record) -> Result<()> {
    if record(home, &expected.installation_id)?.as_ref() == Some(expected) {
        fs::remove_file(home.join(RECORD))?;
    }
    Ok(())
}

pub(super) struct Lease(Connection);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.execute_batch("ROLLBACK");
    }
}

pub(super) fn lease(home: &Path, name: &str) -> Result<Lease> {
    let path = home.join(name);
    if read(&path)?.is_none() {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                read(&path)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::ZERO)?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .context("another GUI process operation is running")?;
    Ok(Lease(connection))
}
