//! Internal control transport. File contents must fit after JSON escaping too.
use super::{FileRequest, MAX_FILE_WRITE_BYTES, WORKSPACE_RUNNER, WorkspaceRequest};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_REQUEST_ARGUMENT_BYTES: usize =
    MAX_MESSAGE_BYTES - WORKSPACE_RUNNER.len() - "workspace".len() - "request".len();

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum EncodedRequest {
    FileWriteBase64 {
        path: String,
        content_base64: String,
    },
}

/// Keep accepted commands byte-for-byte stable for exact retries. Only writes
/// whose escaped JSON exceeds the native argument budget use the compact form.
///
/// # Errors
/// Rejects invalid requests or serialization failures.
pub fn encode(request: &WorkspaceRequest) -> Result<String, &'static str> {
    request.validate()?;
    let plain = serde_json::to_string(request).map_err(|_| "request serialization failed")?;
    if plain.len() > MAX_REQUEST_ARGUMENT_BYTES
        && let WorkspaceRequest::File(FileRequest::Write { path, content }) = request
    {
        return serde_json::to_string(&EncodedRequest::FileWriteBase64 {
            path: path.clone(),
            content_base64: STANDARD.encode(content.as_bytes()),
        })
        .map_err(|_| "request serialization failed");
    }
    Ok(plain)
}

/// Decode a bounded control message, accepting the original request format too.
///
/// # Errors
/// Rejects malformed messages, invalid UTF-8, unsafe paths and oversized writes.
pub fn decode(value: Value) -> Result<WorkspaceRequest, &'static str> {
    let request = if value["kind"] == "file_write_base64" {
        let EncodedRequest::FileWriteBase64 {
            path,
            content_base64,
        } = serde_json::from_value(value).map_err(|_| "invalid encoded file write")?;
        if content_base64.len() > MAX_FILE_WRITE_BYTES.div_ceil(3) * 4 {
            return Err("file write exceeds 8192 bytes");
        }
        let bytes = STANDARD
            .decode(content_base64)
            .map_err(|_| "invalid file write base64")?;
        let content = String::from_utf8(bytes).map_err(|_| "file write must be UTF-8")?;
        WorkspaceRequest::File(FileRequest::Write { path, content })
    } else {
        serde_json::from_value(value).map_err(|_| "invalid workspace request")?
    };
    request.validate()?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{NativeCommand, NativeOutput};
    use serde_json::json;

    #[test]
    fn file_writes_fit_transport_budgets_and_preserve_every_byte() {
        // A maximally escaped, 512-byte valid path also consumes envelope space.
        let path = format!("src/{}", format!("{}/", "\u{1}".repeat(126)).repeat(4));
        let path = format!("{}x", &path[..path.len() - 1]);
        assert_eq!(path.len(), 512);
        for unit in ["a", "\n", "\r", "\t", "\"", "\\", "\0", "界\n\\\"😀"] {
            let content = format!(
                "{}{}",
                unit.repeat(MAX_FILE_WRITE_BYTES / unit.len()),
                "x".repeat(MAX_FILE_WRITE_BYTES % unit.len())
            );
            let request = WorkspaceRequest::File(FileRequest::Write {
                path: path.clone(),
                content: content.clone(),
            });
            let plain = serde_json::to_string(&request).unwrap();
            let encoded = encode(&request).unwrap();
            if plain.len() <= MAX_REQUEST_ARGUMENT_BYTES {
                assert_eq!(encoded, plain, "preserve existing retry identity");
            }
            assert!(encoded.len() <= MAX_REQUEST_ARGUMENT_BYTES, "{unit:?}");
            let decoded = decode(serde_json::from_str(&encoded).unwrap()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), json!(request));
            let command = NativeCommand {
                private_io: None,
                script: String::new(),
                argv: vec![
                    WORKSPACE_RUNNER.into(),
                    "workspace".into(),
                    "request".into(),
                    encoded,
                ],
                timeout_seconds: 25,
                output: NativeOutput::default(),
            };
            command.validate().unwrap();
            assert!(serde_json::to_vec(&command).unwrap().len() <= 65536);
            let oversized = WorkspaceRequest::File(FileRequest::Write {
                path: path.clone(),
                content: format!("{content}x"),
            });
            assert_eq!(
                encode(&oversized).unwrap_err(),
                "file write exceeds 8192 bytes"
            );
        }
    }

    #[test]
    fn encoded_writes_cannot_bypass_validation() {
        for (path, content_base64) in [
            ("src/file", "!invalid!".into()),
            ("src/file", STANDARD.encode([255])),
            (
                "src/file",
                STANDARD.encode(vec![0; MAX_FILE_WRITE_BYTES + 1]),
            ),
            (
                "src/file",
                STANDARD.encode(vec![0; MAX_FILE_WRITE_BYTES + 4]),
            ),
            ("../escape", STANDARD.encode("valid")),
            (".proofstorm/private", STANDARD.encode("valid")),
        ] {
            assert!(
                decode(json!({"kind":"file_write_base64","request":{
                    "path":path,"content_base64":content_base64
                }}))
                .is_err()
            );
        }
        assert!(
            decode(json!({"kind":"file_write_base64","request":{
                "path":"src/file","content_base64":"","content":"ambiguous"
            }}))
            .is_err()
        );
        assert!(
            decode(json!({"kind":"file","request":{
                "action":"write","path":"src/file","content":"x".repeat(8193)
            }}))
            .is_err()
        );
    }
}
