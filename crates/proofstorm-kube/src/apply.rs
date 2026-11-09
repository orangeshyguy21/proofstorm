//! Server-side apply patches that the API server's YAML parser reads exactly.

use kube::api::Patch;
use serde::{Serialize, Serializer, ser::Error as _};
use serde_json::value::RawValue;

/// A server-side apply patch of `document`. Use this, not `Patch::Apply`:
/// Kubernetes parses apply bodies as YAML, which refuses some characters
/// plain JSON leaves raw (see [`proofstorm_core::apply_json`]).
pub fn apply_patch<T: Serialize>(document: T) -> Patch<ApplyDocument<T>> {
    Patch::Apply(ApplyDocument(document))
}

/// A document that serializes through [`proofstorm_core::apply_json`].
#[derive(Debug)]
pub struct ApplyDocument<T>(T);

impl<T: Serialize> Serialize for ApplyDocument<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let json = proofstorm_core::apply_json::to_vec(&self.0).map_err(S::Error::custom)?;
        let json = String::from_utf8(json).map_err(S::Error::custom)?;
        RawValue::from_string(json)
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_patches_send_the_escaped_document() {
        let document =
            serde_json::json!({"status": {"stdout": "SQLite format 3\u{0}\u{7f}\u{85}"}});
        let Patch::Apply(body) = apply_patch(&document) else {
            panic!("apply patch expected");
        };
        // kube sends an apply body as `serde_json::to_vec` of the document.
        let sent = serde_json::to_vec(&body).unwrap();
        assert_eq!(
            sent,
            br#"{"status":{"stdout":"SQLite format 3\u0000\u007f\u0085"}}"#
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sent).unwrap(),
            document
        );
    }

    /// A plain `Patch::Apply` brings the season 2 wedge back for any text
    /// YAML refuses, so every crate builds apply patches with `apply_patch`.
    #[test]
    fn no_crate_sends_a_plain_apply_patch() {
        fn rust_files(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    rust_files(&path, files);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        rust_files(&crates, &mut files);
        assert!(files.len() > 100, "scanned only {} files", files.len());
        let plain = files
            .iter()
            .filter(|path| !path.ends_with("proofstorm-kube/src/apply.rs"))
            .filter(|path| {
                std::fs::read_to_string(path)
                    .unwrap()
                    .contains("Patch::Apply(")
            })
            .collect::<Vec<_>>();
        assert!(
            plain.is_empty(),
            "use proofstorm_kube::apply_patch in {plain:?}"
        );
    }
}
