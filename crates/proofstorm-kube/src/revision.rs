//! A cell's desired configuration lives outside the cell object, in one
//! immutable, content-addressed revision per `revision_digest`.
//!
//! The cell object stays a few kilobytes however many components it has; its
//! `CellSpec` and `ResolvedLock` (about 1.1 KB per component) are stored once,
//! gzip-compressed, in a `ConfigMap` named for the revision. Code that needs
//! them holds a [`LoadedCell`], so a cell whose revision hasn't been read can
//! never pass for a cell with no components.
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    ops::Deref,
    sync::Arc,
};

use k8s_openapi::{
    ByteString,
    api::core::v1::ConfigMap,
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference},
};
use proofstorm_core::{CellSpec, ResolvedLock, digest_json};
use serde::{Deserialize, Serialize};

use crate::{INSTANCE_LABEL, ProofstormCell};

/// The `ConfigMap` key holding the gzip-compressed revision document.
const REVISION_KEY: &str = "revision.json.gz";
/// A revision's compressed size limit, below the 1 MiB `ConfigMap` limit; about
/// 10,000 components. Larger revisions would need chunking.
pub const MAX_REVISION_BYTES: usize = 900 * 1024;
/// Decompressed size limit, so a corrupt or hostile object can't exhaust memory.
const MAX_REVISION_JSON_BYTES: u64 = 64 * 1024 * 1024;

/// The immutable desired configuration a cell's `revision_digest` names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellRevision {
    pub workspace_id: String,
    pub instance_key: String,
    pub revision_digest: String,
    pub cell: CellSpec,
    pub lock: ResolvedLock,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RevisionError {
    #[error("revision {0} is not stored yet")]
    Missing(String),
    #[error("revision object is not readable: {0}")]
    Unreadable(String),
    #[error("revision content does not match its digest {0}")]
    DigestMismatch(String),
    #[error("revision belongs to another cell")]
    ForeignCell,
    #[error("revision is {0} bytes compressed; the limit is {MAX_REVISION_BYTES}")]
    TooLarge(usize),
    #[error("revision is invalid: {0}")]
    Invalid(String),
    #[error("revision could not be read from the API: {0}")]
    Api(String),
}

impl CellRevision {
    /// Check that the content is exactly what `revision_digest` names.
    ///
    /// # Errors
    ///
    /// [`RevisionError::DigestMismatch`] when it isn't.
    pub fn verify(&self) -> Result<(), RevisionError> {
        if proofstorm_core::publication_digest(&self.workspace_id, &self.cell, &self.lock)
            == self.revision_digest
        {
            Ok(())
        } else {
            Err(RevisionError::DigestMismatch(self.revision_digest.clone()))
        }
    }
}

/// The revision object's name: one per cell instance and digest.
#[must_use]
pub fn revision_object_name(instance_key: &str, revision_digest: &str) -> String {
    let name = digest_json(&(instance_key, revision_digest));
    format!("revision-{}", &name[7..39])
}

/// The immutable revision object, owned by its cell when `owner` is given.
///
/// # Errors
///
/// [`RevisionError::TooLarge`] when the compressed revision exceeds
/// [`MAX_REVISION_BYTES`].
pub fn revision_config_map(
    revision: &CellRevision,
    namespace: &str,
    owner: Option<OwnerReference>,
) -> Result<ConfigMap, RevisionError> {
    let json =
        serde_json::to_vec(revision).map_err(|e| RevisionError::Unreadable(e.to_string()))?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&json)
        .map_err(|e| RevisionError::Unreadable(e.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|e| RevisionError::Unreadable(e.to_string()))?;
    if compressed.len() > MAX_REVISION_BYTES {
        return Err(RevisionError::TooLarge(compressed.len()));
    }
    Ok(ConfigMap {
        metadata: ObjectMeta {
            name: Some(revision_object_name(
                &revision.instance_key,
                &revision.revision_digest,
            )),
            namespace: Some(namespace.to_owned()),
            labels: Some(BTreeMap::from([
                (INSTANCE_LABEL.to_owned(), revision.instance_key.clone()),
                ("proofstorm.dev/revision".to_owned(), "true".to_owned()),
            ])),
            owner_references: owner.map(|owner| vec![owner]),
            ..ObjectMeta::default()
        },
        immutable: Some(true),
        binary_data: Some(BTreeMap::from([(
            REVISION_KEY.to_owned(),
            ByteString(compressed),
        )])),
        ..ConfigMap::default()
    })
}

/// Read and verify a revision object.
///
/// # Errors
///
/// [`RevisionError::Unreadable`] for a malformed object and
/// [`RevisionError::DigestMismatch`] when its content doesn't match its digest.
pub fn decode_revision(map: &ConfigMap) -> Result<CellRevision, RevisionError> {
    let compressed = map
        .binary_data
        .as_ref()
        .and_then(|data| data.get(REVISION_KEY))
        .ok_or_else(|| RevisionError::Unreadable("no revision data".into()))?;
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(compressed.0.as_slice())
        .take(MAX_REVISION_JSON_BYTES)
        .read_to_end(&mut json)
        .map_err(|e| RevisionError::Unreadable(e.to_string()))?;
    let revision: CellRevision =
        serde_json::from_slice(&json).map_err(|e| RevisionError::Unreadable(e.to_string()))?;
    revision.verify()?;
    Ok(revision)
}

/// A cell together with the revision whose spec and lock it runs.
///
/// It dereferences to the cell object, so identity, metadata and status read
/// as before; the desired configuration is `revision.cell` and `revision.lock`.
#[derive(Debug, Clone)]
pub struct LoadedCell {
    pub resource: ProofstormCell,
    pub revision: Arc<CellRevision>,
}

impl Deref for LoadedCell {
    type Target = ProofstormCell;

    fn deref(&self) -> &ProofstormCell {
        &self.resource
    }
}

impl std::ops::DerefMut for LoadedCell {
    fn deref_mut(&mut self) -> &mut ProofstormCell {
        &mut self.resource
    }
}

impl LoadedCell {
    /// Pair a cell with the revision its digest names.
    ///
    /// # Errors
    ///
    /// [`RevisionError::ForeignCell`] for another cell's revision, and
    /// [`RevisionError::Missing`] for one with another digest.
    pub fn new(
        resource: ProofstormCell,
        revision: Arc<CellRevision>,
    ) -> Result<Self, RevisionError> {
        if revision.revision_digest != resource.spec.revision_digest {
            return Err(RevisionError::Missing(
                resource.spec.revision_digest.clone(),
            ));
        }
        Self::at_revision(resource, revision)
    }

    /// Pair a cell with a spec and lock the caller already holds and trusts,
    /// such as a revision it is about to store. Revisions read from the API go
    /// through [`decode_revision`], which verifies them.
    #[must_use]
    pub fn from_spec(resource: ProofstormCell, cell: CellSpec, lock: ResolvedLock) -> Self {
        let revision = CellRevision {
            workspace_id: resource.spec.workspace_id.clone(),
            instance_key: resource.spec.instance_key.clone(),
            revision_digest: resource.spec.revision_digest.clone(),
            cell,
            lock,
        };
        Self {
            resource,
            revision: Arc::new(revision),
        }
    }

    /// The component plans this cell's revision compiles to.
    ///
    /// # Errors
    ///
    /// A revision the adapters can't compile.
    pub fn plans(
        &self,
    ) -> Result<Vec<proofstorm_core::ComponentPlanContract>, crate::AdapterError> {
        crate::compile_component_plans(
            &self.spec.instance_key,
            &self.spec.revision_digest,
            &self.revision.cell,
            &self.revision.lock,
        )
    }

    /// Fill `status.components` from the cell's component objects, in
    /// revision order; see [`crate::assemble_component_statuses`].
    pub fn assemble_components<'a>(
        &mut self,
        objects: impl IntoIterator<Item = &'a crate::ProofstormComponent>,
    ) {
        let Self { resource, revision } = self;
        crate::assemble_component_statuses(resource, Some(&revision.cell), objects);
    }

    /// Pair a cell with one of its earlier revisions, as an action bound to
    /// that revision sees it.
    ///
    /// # Errors
    ///
    /// [`RevisionError::ForeignCell`] for another cell's revision.
    pub fn at_revision(
        mut resource: ProofstormCell,
        revision: Arc<CellRevision>,
    ) -> Result<Self, RevisionError> {
        if revision.instance_key != resource.spec.instance_key
            || revision.workspace_id != resource.spec.workspace_id
        {
            return Err(RevisionError::ForeignCell);
        }
        resource
            .spec
            .revision_digest
            .clone_from(&revision.revision_digest);
        Ok(Self { resource, revision })
    }
}

/// Revisions kept in memory. A cell has a handful at most; evicted ones are
/// simply read again.
const CACHED_REVISIONS: usize = 256;

/// Immutable revisions, each fetched, verified and validated once.
#[derive(Default)]
pub struct RevisionCache(std::sync::Mutex<BTreeMap<(String, String), Arc<CellRevision>>>);

impl RevisionCache {
    /// Revision `digest` of `cell`, from the cell's namespace.
    ///
    /// # Errors
    ///
    /// A missing, unreadable, mismatched, foreign or invalid revision.
    ///
    /// # Panics
    ///
    /// If another thread panicked while holding the cache lock.
    pub async fn load(
        &self,
        client: &kube::Client,
        cell: &ProofstormCell,
        digest: &str,
    ) -> Result<Arc<CellRevision>, RevisionError> {
        use kube::ResourceExt;
        let key = (cell.spec.instance_key.clone(), digest.to_owned());
        if let Some(revision) = self.0.lock().expect("revision cache").get(&key) {
            return Ok(revision.clone());
        }
        let namespace = cell
            .namespace()
            .ok_or_else(|| RevisionError::Api("cell has no namespace".into()))?;
        let name = revision_object_name(&cell.spec.instance_key, digest);
        let map = kube::Api::<ConfigMap>::namespaced(client.clone(), &namespace)
            .get_opt(&name)
            .await
            .map_err(|error| RevisionError::Api(error.to_string()))?
            .ok_or_else(|| RevisionError::Missing(digest.to_owned()))?;
        let revision = decode_revision(&map)?;
        if revision.instance_key != cell.spec.instance_key
            || revision.workspace_id != cell.spec.workspace_id
            || revision.revision_digest != digest
        {
            return Err(RevisionError::ForeignCell);
        }
        // The CRD no longer sees the spec, so its admission rules can't guard it.
        let report = proofstorm_core::validate_cell(&revision.cell);
        if !report.valid {
            return Err(RevisionError::Invalid(
                report.issues.first().map_or_else(String::new, |issue| {
                    format!("{}: {}", issue.path, issue.message)
                }),
            ));
        }
        let revision = Arc::new(revision);
        let mut cache = self.0.lock().expect("revision cache");
        if cache.len() >= CACHED_REVISIONS
            && let Some(first) = cache.keys().next().cloned()
        {
            cache.remove(&first);
        }
        cache.insert(key, revision.clone());
        Ok(revision)
    }

    /// `cell` with the revision its digest names.
    ///
    /// # Errors
    ///
    /// As [`Self::load`].
    pub async fn loaded(
        &self,
        client: &kube::Client,
        cell: ProofstormCell,
    ) -> Result<LoadedCell, RevisionError> {
        let digest = cell.spec.revision_digest.clone();
        let revision = self.load(client, &cell, &digest).await?;
        LoadedCell::new(cell, revision)
    }

    /// Remember a revision this process just wrote or already holds.
    ///
    /// # Panics
    ///
    /// If another thread panicked while holding the cache lock.
    pub fn insert(&self, revision: CellRevision) {
        self.0.lock().expect("revision cache").insert(
            (
                revision.instance_key.clone(),
                revision.revision_digest.clone(),
            ),
            Arc::new(revision),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{API_VERSION, CellPolicy};

    fn revision() -> CellRevision {
        let cell = CellSpec {
            api_version: API_VERSION.into(),
            name: "cell".into(),
            components: Vec::new(),
            links: Vec::new(),
            policy: CellPolicy::default(),
        };
        let lock = ResolvedLock {
            api_version: API_VERSION.into(),
            digest: "sha256:lock".into(),
            entries: Vec::new(),
        };
        CellRevision {
            revision_digest: digest_json(&("workspace", &cell, &lock)),
            workspace_id: "workspace".into(),
            instance_key: "i0123456789".into(),
            cell,
            lock,
        }
    }

    #[test]
    fn a_revision_round_trips_compressed_and_verified() {
        let revision = revision();
        let map = revision_config_map(&revision, "proofstorm-system", None).unwrap();
        assert_eq!(map.immutable, Some(true));
        assert_eq!(
            map.metadata.name.as_deref(),
            Some(revision_object_name(&revision.instance_key, &revision.revision_digest).as_str())
        );
        assert_eq!(decode_revision(&map).unwrap(), revision);
    }

    #[test]
    fn a_revision_whose_content_does_not_match_its_digest_is_refused() {
        let mut revision = revision();
        revision.cell.name = "tampered".into();
        let map = revision_config_map(&revision, "proofstorm-system", None).unwrap();
        assert!(matches!(
            decode_revision(&map),
            Err(RevisionError::DigestMismatch(_))
        ));
        assert!(matches!(
            decode_revision(&ConfigMap::default()),
            Err(RevisionError::Unreadable(_))
        ));
    }
}
