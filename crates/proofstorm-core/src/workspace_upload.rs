//! Metadata for a staged binary upload; bytes travel separately over stdin.
use serde::{Deserialize, Serialize};

pub const MAX_UPLOAD_BYTES: u64 = super::MAX_SOURCE_BYTES;
pub const MAX_STAGED_BYTES: u64 = 2 * MAX_UPLOAD_BYTES;
pub const MAX_STAGED_UPLOADS: usize = 16;
pub const STAGING_CAPACITY_ERROR: &str =
    "workspace upload staging capacity exceeded; retry after pending uploads finish or expire";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    pub upload_id: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub executable: bool,
}

impl UploadRequest {
    /// # Errors
    /// Rejects unsafe destinations, invalid identities and files over 16 MiB.
    pub fn validate(&self) -> Result<(), &'static str> {
        super::validate_path(&self.path)?;
        if self.bytes > MAX_UPLOAD_BYTES {
            return Err("workspace upload exceeds 16 MiB");
        }
        if [&self.upload_id, &self.sha256]
            .iter()
            .any(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err("invalid workspace upload identity or checksum");
        }
        Ok(())
    }
}
