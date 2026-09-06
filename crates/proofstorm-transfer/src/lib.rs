//! Private, lab-local byte custody. This crate does not implement Cashu, query a
//! mint, or infer whether proofs are spent. Native wallet observations own that.
//!
//! Runtime-only API: grants must be constructed from freshly checked existing
//! workspace capabilities and active endpoint leases, never from agent JSON.
//! Native completion callbacks must come from the owned supervisor operation.
#![allow(
    clippy::missing_errors_doc,
    reason = "all APIs return static, non-payload Error diagnostics"
)]

use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("private transfer storage unavailable")]
    Storage,
    #[error("invalid transfer request")]
    Invalid,
    #[error("transfer access denied or reference unavailable")]
    Access,
    #[error("transfer capacity unavailable")]
    Capacity,
    #[error("transfer identity or native operation conflict")]
    Conflict,
    #[error("transfer phase does not permit this operation")]
    Phase,
    #[error("payload capture incomplete; native outcome requires reconciliation")]
    Capture,
    #[error("payload integrity check failed")]
    Integrity,
    #[error("private payload delivery interrupted")]
    Delivery,
}

type Result<T> = std::result::Result<T, Error>;

/// Authorization resolved by the embedding runtime; deliberately not deserializable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grant {
    pub workspace: String,
    pub lab: String,
    pub principal: String,
    pub wallet: String,
    pub lease: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub payload_bytes: u32,
    /// Includes both source capture and destination inbox reservations.
    pub lab_bytes: u64,
    pub active_transfers: u32,
    pub retention_seconds: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            payload_bytes: 1024 * 1024,
            lab_bytes: 32 * 1024 * 1024,
            active_transfers: 8,
            retention_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePhase {
    Reserved,
    Started,
    Ready,
    Unknown,
    Released,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPath {
    InfrastructureRelay,
}

/// Manifest computed at the private source capture, before moving the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadManifest {
    pub bytes: u32,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ProducedPayload {
    pub native: NativeReceipt,
    pub manifest: Option<PayloadManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent native supervisor receipt facts, not interchangeable states"
)]
pub struct NativeReceipt {
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub cleanup_verified: bool,
    pub streams_complete: bool,
    pub output_truncated: bool,
}

impl NativeReceipt {
    fn clean_success(&self) -> bool {
        self.exit_code == Some(0)
            && self.exit_signal.is_none()
            && !self.timed_out
            && !self.cancelled
            && self.cleanup_verified
            && self.streams_complete
            && !self.output_truncated
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeStage {
    pub input_started: bool,
    pub operation_id: Option<String>,
    pub receipt: Option<NativeReceipt>,
    /// An interrupted runtime cannot infer whether the wallet mutation happened.
    pub interrupted: bool,
}

/// Immutable recipient binding, not a claim that the lease is still active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientAuthority {
    pub principal: String,
    pub lease: String,
    pub expires_at_unix: u64,
}

/// Metadata only. No payload, private path, proof list or inferred spent state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<RecipientAuthority>,
    pub revision: u64,
    pub transport: TransportPath,
    pub id: String,
    pub workspace: String,
    pub lab: String,
    pub source_wallet: String,
    pub destination_wallet: String,
    pub maximum_bytes: u32,
    pub expires_at_unix: u64,
    pub capture: CapturePhase,
    pub bytes: Option<u32>,
    pub sha256: Option<String>,
    pub source_manifest: Option<PayloadManifest>,
    pub delivered: bool,
    pub source: NativeStage,
    pub receiver: NativeStage,
    /// Immutable references to native check/reconciliation evidence, not a shadow ledger.
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageCleanupReceipt {
    pub admission_closed: bool,
    pub retained_transfer_records: usize,
    pub remaining_payload_bytes: u32,
    pub native_operations_without_receipts: usize,
    pub storage_cleanup_verified: bool,
}

// Exactly one lab and one runtime authority own a vault directory. A new process
// may reopen it, but must reconcile interrupted native stages explicitly.
pub struct Vault {
    db: Connection,
    workspace: String,
    lab: String,
    limits: Limits,
}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| Error::Storage)
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && m.permissions().mode() & 0o777 == 0o700 => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| Error::Storage)
        }
        Err(_) | Ok(_) => Err(Error::Storage),
    }
}

impl Vault {
    pub fn open(root: &Path, workspace: &str, lab: &str, limits: Limits) -> Result<Self> {
        if !ident(workspace)
            || !ident(lab)
            || limits.payload_bytes == 0
            || limits.payload_bytes > 16 * 1024 * 1024
            || limits.lab_bytes > 256 * 1024 * 1024
            || limits.lab_bytes < u64::from(limits.payload_bytes) * 2
            || !(1..=32).contains(&limits.active_transfers)
            || !(1..=86400).contains(&limits.retention_seconds)
        {
            return Err(Error::Invalid);
        }
        private_directory(root)?;
        let path = root.join("private.sqlite3");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let m = fs::symlink_metadata(&path).map_err(|_| Error::Storage)?;
                if !m.is_file() || m.permissions().mode() & 0o777 != 0o600 {
                    return Err(Error::Storage);
                }
            }
            Err(_) => return Err(Error::Storage),
        }
        let db = Connection::open(path).map_err(|_| Error::Storage)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| Error::Storage)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), workspace TEXT NOT NULL, lab TEXT NOT NULL, closed INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS transfers (id INTEGER PRIMARY KEY, handle TEXT NOT NULL UNIQUE, request_key TEXT NOT NULL UNIQUE, request_json TEXT NOT NULL, source_json TEXT NOT NULL, destination_json TEXT NOT NULL, metadata TEXT NOT NULL, capacity INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS payloads (id INTEGER PRIMARY KEY, body BLOB NOT NULL);")
            .map_err(|_| Error::Storage)?;
        db.execute(
            "INSERT OR IGNORE INTO identity(singleton,workspace,lab) VALUES(1,?1,?2)",
            params![workspace, lab],
        )
        .map_err(|_| Error::Storage)?;
        let identity: (String, String) = db
            .query_row(
                "SELECT workspace,lab FROM identity WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        if identity != (workspace.to_owned(), lab.to_owned()) {
            return Err(Error::Access);
        }
        Ok(Self {
            db,
            workspace: workspace.into(),
            lab: lab.into(),
            limits,
        })
    }

    fn grant(&self, grant: &Grant) -> Result<()> {
        if grant.workspace != self.workspace
            || grant.lab != self.lab
            || ![
                &grant.workspace,
                &grant.lab,
                &grant.principal,
                &grant.wallet,
                &grant.lease,
            ]
            .iter()
            .all(|s| ident(s))
            || grant.expires_at_unix <= now()?
        {
            return Err(Error::Access);
        }
        Ok(())
    }

    pub fn prepare(
        &mut self,
        source: &Grant,
        destination: &Grant,
        key: &str,
        maximum_bytes: u32,
    ) -> Result<Transfer> {
        self.grant(source)?;
        self.grant(destination)?;
        if !ident(key)
            || maximum_bytes == 0
            || maximum_bytes > self.limits.payload_bytes
            || source.wallet == destination.wallet
        {
            return Err(Error::Invalid);
        }
        let request = serde_json::to_string(&(
            grant_identity(source)?,
            grant_identity(destination)?,
            maximum_bytes,
        ))
        .map_err(|_| Error::Invalid)?;
        // Scope deduplication to the authorized source principal and lease.
        let key = format!("{}/{}/{}", source.principal, source.lease, key);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        let timestamp = now()?;
        if source.expires_at_unix <= timestamp || destination.expires_at_unix <= timestamp {
            return Err(Error::Access);
        }
        let closed: bool = tx
            .query_row("SELECT closed FROM identity WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .map_err(|_| Error::Storage)?;
        if closed {
            return Err(Error::Phase);
        }
        let records: u32 = tx
            .query_row("SELECT COUNT(*) FROM transfers", [], |r| r.get(0))
            .map_err(|_| Error::Storage)?;
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT request_json,metadata FROM transfers WHERE request_key=?1",
                [&key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Storage)?;
        if let Some((original, metadata)) = previous {
            if original != request {
                return Err(Error::Conflict);
            }
            return serde_json::from_str(&metadata).map_err(|_| Error::Storage);
        }
        let (count, used): (u32, u32) = tx
            .query_row(
                "SELECT COUNT(*),COALESCE(SUM(capacity),0) FROM transfers WHERE capacity>0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let capacity = u64::from(maximum_bytes) * 2;
        if records >= 4096
            || count >= self.limits.active_transfers
            || u64::from(used) + capacity > self.limits.lab_bytes
        {
            return Err(Error::Capacity);
        }
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
        let id = format!("payload-{:x}", Sha256::digest(random));
        let metadata = Transfer {
            recipient: None,
            revision: 0,
            transport: TransportPath::InfrastructureRelay,
            id: id.clone(),
            workspace: self.workspace.clone(),
            lab: self.lab.clone(),
            source_wallet: source.wallet.clone(),
            destination_wallet: destination.wallet.clone(),
            maximum_bytes,
            expires_at_unix: now()? + u64::from(self.limits.retention_seconds),
            capture: CapturePhase::Reserved,
            bytes: None,
            sha256: None,
            source_manifest: None,
            delivered: false,
            source: NativeStage::default(),
            receiver: NativeStage::default(),
            observations: vec![],
        };
        tx.execute("INSERT INTO transfers(handle,request_key,request_json,source_json,destination_json,metadata,capacity) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,key,request,grant_identity(source)?,grant_identity(destination)?,encode(&metadata)?,i64::try_from(capacity).map_err(|_|Error::Storage)?]).map_err(|_|Error::Storage)?;
        let row = tx.last_insert_rowid();
        for index in [row * 2, row * 2 + 1] {
            tx.execute(
                "INSERT INTO payloads VALUES(?1,zeroblob(?2))",
                params![index, maximum_bytes],
            )
            .map_err(|_| Error::Storage)?;
        }
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(metadata)
    }

    fn authorized(
        &self,
        grant: &Grant,
        id: &str,
        destination: Option<bool>,
    ) -> Result<(i64, Transfer)> {
        self.grant(grant)?;
        let (row, t, source, dest) = row(&self.db, id)?;
        let identity = grant_identity(grant)?;
        let same = |text: &str| text == identity;
        let allowed = match destination {
            Some(true) => same(&dest),
            Some(false) => same(&source),
            None => same(&source) || same(&dest),
        };
        if !allowed {
            return Err(Error::Access);
        }
        Ok((row, t))
    }

    /// Transfer the fixed recipient role once, before inbox delivery or native import.
    /// The runtime must first authenticate the child lease and its parent/scope.
    pub fn handoff(&mut self, source: &Grant, destination: &Grant, id: &str) -> Result<Transfer> {
        self.grant(source)?;
        self.grant(destination)?;
        if source.principal == destination.principal || source.lease == destination.lease {
            return Err(Error::Invalid);
        }
        let identity = grant_identity(destination)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        let (_, mut t, original_source, original_destination) = row(&tx, id)?;
        if grant_identity(source)? != original_source || destination.wallet != t.destination_wallet
        {
            return Err(Error::Access);
        }
        admission(&tx, source, &t)?;
        if destination.expires_at_unix <= now()? {
            return Err(Error::Access);
        }
        if t.recipient.is_some() {
            return if original_destination == identity {
                Ok(t)
            } else {
                Err(Error::Conflict)
            };
        }
        if t.capture != CapturePhase::Ready || t.delivered || t.receiver.operation_id.is_some() {
            return Err(Error::Phase);
        }
        t.recipient = Some(RecipientAuthority {
            principal: destination.principal.clone(),
            lease: destination.lease.clone(),
            expires_at_unix: destination.expires_at_unix,
        });
        tx.execute(
            "UPDATE transfers SET destination_json=?1 WHERE handle=?2",
            params![identity, id],
        )
        .map_err(|_| Error::Storage)?;
        save(&tx, &mut t)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(t)
    }

    pub fn status(&self, grant: &Grant, id: &str) -> Result<Transfer> {
        self.authorized(grant, id, None).map(|(_, t)| t)
    }

    pub fn begin_capture(&mut self, grant: &Grant, id: &str, operation: &str) -> Result<Transfer> {
        let (_, mut t) = self.authorized(grant, id, Some(false))?;
        usable(&t)?;
        if !ident(operation) {
            return Err(Error::Invalid);
        }
        if t.capture != CapturePhase::Reserved {
            return Err(Error::Phase);
        }
        t.capture = CapturePhase::Started;
        t.source.operation_id = Some(operation.into());
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        admission(&tx, grant, &t)?;
        claim_operation(&tx, operation)?;
        save(&tx, &mut t)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(t)
    }

    /// Trusted supervisor callback for an accepted producer, including callbacks
    /// after access/retention expired. This grants no byte access.
    pub fn finish_source(
        &mut self,
        id: &str,
        operation: &str,
        receipt: NativeReceipt,
    ) -> Result<Transfer> {
        let (_, mut t, _, _) = row(&self.db, id)?;
        if t.source.operation_id.as_deref() != Some(operation) {
            return Err(Error::Conflict);
        }
        if let Some(previous) = t.source.receipt {
            return if previous == receipt {
                Ok(t)
            } else {
                Err(Error::Conflict)
            };
        }
        t.source.receipt = Some(receipt);
        if !receipt.clean_success() && t.capture == CapturePhase::Started {
            t.capture = CapturePhase::Unknown;
        }
        save(&self.db, &mut t)?;
        Ok(t)
    }

    /// Complete the already-owned native producer. On capture failure, preserve
    /// its result and mark custody unknown; never launch a replacement producer.
    pub fn capture(
        &mut self,
        grant: &Grant,
        id: &str,
        operation: &str,
        reader: &mut dyn Read,
        produced: ProducedPayload,
    ) -> Result<Transfer> {
        let native = produced.native;
        let (index, mut t) = self.authorized(grant, id, Some(false))?;
        usable(&t)?;
        if t.capture != CapturePhase::Started || t.source.operation_id.as_deref() != Some(operation)
        {
            return Err(Error::Phase);
        }
        if t.source.receipt.as_ref().is_some_and(|old| old != &native) {
            return Err(Error::Conflict);
        }
        t.source.receipt = Some(native);
        save(&self.db, &mut t)?;
        if !native.clean_success() {
            t.capture = CapturePhase::Unknown;
            save(&self.db, &mut t)?;
            return Err(Error::Capture);
        }
        let valid = produced.manifest.as_ref().is_some_and(|m| {
            m.bytes > 0
                && m.bytes <= t.maximum_bytes
                && m.sha256.len() == 64
                && m.sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        });
        if !valid {
            t.capture = CapturePhase::Unknown;
            save(&self.db, &mut t)?;
            return Err(Error::Capture);
        }
        if t.source_manifest.is_some() && t.source_manifest != produced.manifest {
            return Err(Error::Conflict);
        }
        t.source_manifest = produced.manifest;
        save(&self.db, &mut t)?;
        self.capture_staged(grant, index, t, reader)
    }

    fn capture_staged(
        &mut self,
        grant: &Grant,
        index: i64,
        mut t: Transfer,
        reader: &mut dyn Read,
    ) -> Result<Transfer> {
        let id = t.id.clone();
        let mut read_started = false;
        let result = (|| -> Result<()> {
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| Error::Storage)?;
            let (current_index, current, _, _) = row(&tx, &id)?;
            if current_index != index
                || current.capture != CapturePhase::Started
                || current.source.operation_id != t.source.operation_id
                || current.source.receipt != t.source.receipt
                || current.source_manifest != t.source_manifest
            {
                return Err(Error::Phase);
            }
            t = current;
            admission(&tx, grant, &t)?;
            let mut blob = tx
                .blob_open("main", "payloads", "body", index * 2, false)
                .map_err(|_| Error::Storage)?;
            read_started = true;
            let (length, digest) = copy_bounded(reader, &mut blob, t.maximum_bytes)?;
            if t.source_manifest.as_ref()
                != Some(&PayloadManifest {
                    bytes: length,
                    sha256: digest.clone(),
                })
            {
                return Err(Error::Integrity);
            }
            drop(blob);
            t.bytes = Some(length);
            t.sha256 = Some(digest);
            t.capture = CapturePhase::Ready;
            save(&tx, &mut t)?;
            tx.commit().map_err(|_| Error::Storage)?;
            Ok(())
        })();
        if let Err(error) = result {
            if !read_started {
                return Err(error);
            }
            let (_, mut current, _, _) = row(&self.db, &id)?;
            if current.capture == CapturePhase::Started {
                current.capture = CapturePhase::Unknown;
                save(&self.db, &mut current)?;
            }
            return Err(Error::Capture);
        }
        Ok(t)
    }

    pub fn deliver(&mut self, grant: &Grant, id: &str) -> Result<Transfer> {
        let (index, mut t) = self.authorized(grant, id, Some(true))?;
        usable(&t)?;
        if t.capture != CapturePhase::Ready {
            return Err(Error::Phase);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        admission(&tx, grant, &t)?;
        if t.delivered {
            verify_blob(&tx, index, "inbox_body", &t)?;
            return Ok(t);
        }
        verify_blob(&tx, index, "source_body", &t)?;
        let mut source = tx
            .blob_open("main", "payloads", "body", index * 2, true)
            .map_err(|_| Error::Storage)?;
        let mut inbox = tx
            .blob_open("main", "payloads", "body", index * 2 + 1, false)
            .map_err(|_| Error::Storage)?;
        let mut bounded = (&mut source).take(u64::from(t.bytes.ok_or(Error::Integrity)?));
        let (_, digest) = copy_bounded(&mut bounded, &mut inbox, t.maximum_bytes)?;
        if Some(digest) != t.sha256 {
            return Err(Error::Integrity);
        }
        drop(inbox);
        drop(source);
        t.delivered = true;
        save(&tx, &mut t)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(t)
    }

    pub fn begin_receive(&mut self, grant: &Grant, id: &str, operation: &str) -> Result<Transfer> {
        let (index, mut t) = self.authorized(grant, id, Some(true))?;
        usable(&t)?;
        if !ident(operation) {
            return Err(Error::Invalid);
        }
        if !t.delivered || t.receiver.operation_id.is_some() {
            return Err(Error::Phase);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        admission(&tx, grant, &t)?;
        verify_blob(&tx, index, "inbox_body", &t)?;
        claim_operation(&tx, operation)?;
        t.receiver.operation_id = Some(operation.into());
        // Recheck the receive fence inside the write transaction.
        if row(&tx, id)?.1.receiver.operation_id.is_some() {
            return Err(Error::Phase);
        }
        save(&tx, &mut t)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(t)
    }

    /// Stream only to an owned native consumer's private input. No agent-facing
    /// endpoint may expose this writer or accept an arbitrary output path.
    pub fn consume(
        &mut self,
        grant: &Grant,
        id: &str,
        operation: &str,
        writer: &mut dyn Write,
    ) -> Result<()> {
        let (index, mut t) = self.authorized(grant, id, Some(true))?;
        usable(&t)?;
        if t.receiver.operation_id.as_deref() != Some(operation)
            || t.receiver.receipt.is_some()
            || t.receiver.interrupted
            || t.receiver.input_started
        {
            return Err(Error::Phase);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        admission(&tx, grant, &t)?;
        verify_blob(&tx, index, "inbox_body", &t)?;
        t.receiver.input_started = true;
        save(&tx, &mut t)?;
        tx.commit().map_err(|_| Error::Storage)?;
        let copied = (|| {
            let blob = self
                .db
                .blob_open("main", "payloads", "body", index * 2 + 1, true)
                .map_err(|_| Error::Storage)?;
            let expected = u64::from(t.bytes.ok_or(Error::Integrity)?);
            let mut reader = blob.take(expected);
            let count = std::io::copy(&mut reader, writer).map_err(|_| Error::Delivery)?;
            if count != expected {
                return Err(Error::Integrity);
            }
            writer.flush().map_err(|_| Error::Delivery)
        })();
        if copied.is_err() {
            self.interrupt(id)?;
            return Err(Error::Delivery);
        }
        Ok(())
    }

    /// Runtime completion, bound to the accepted operation even if its lease expired.
    pub fn finish_receive(
        &mut self,
        id: &str,
        operation: &str,
        receipt: NativeReceipt,
    ) -> Result<Transfer> {
        let (_, mut t, _, _) = row(&self.db, id)?;
        if t.receiver.operation_id.as_deref() != Some(operation) {
            return Err(Error::Conflict);
        }
        if let Some(previous) = &t.receiver.receipt {
            return if previous == &receipt {
                Ok(t)
            } else {
                Err(Error::Conflict)
            };
        }
        t.receiver.receipt = Some(receipt);
        save(&self.db, &mut t)?;
        Ok(t)
    }

    pub fn observe(&mut self, grant: &Grant, id: &str, operation: &str) -> Result<Transfer> {
        let (_, mut t) = self.authorized(grant, id, None)?;
        if !ident(operation) {
            return Err(Error::Invalid);
        }
        if !t.observations.iter().any(|op| op == operation) {
            if t.observations.len() >= 32 {
                return Err(Error::Capacity);
            }
            t.observations.push(operation.into());
            save(&self.db, &mut t)?;
        }
        Ok(t)
    }

    /// Runtime restart recovery marks accepted native work unresolved. It never
    /// resets either execution fence or schedules a wallet command.
    pub fn interrupt(&mut self, id: &str) -> Result<Transfer> {
        let (_, mut t, _, _) = row(&self.db, id)?;
        if t.source.operation_id.is_some() && t.source.receipt.is_none() {
            t.source.interrupted = true;
        }
        if t.capture == CapturePhase::Started {
            t.capture = CapturePhase::Unknown;
        }
        if t.receiver.operation_id.is_some() && t.receiver.receipt.is_none() {
            t.receiver.interrupted = true;
        }
        save(&self.db, &mut t)?;
        Ok(t)
    }

    pub fn release(&mut self, grant: &Grant, id: &str) -> Result<Transfer> {
        let (_, t) = self.authorized(grant, id, Some(false))?;
        if t.capture == CapturePhase::Started
            || (t.receiver.operation_id.is_some()
                && t.receiver.receipt.is_none()
                && !t.receiver.interrupted)
        {
            return Err(Error::Phase);
        }
        self.erase(id, CapturePhase::Released, Some(grant))
    }

    /// Runtime retention/finalizer path, not an agent-authorized operation.
    pub fn expire(&mut self) -> Result<usize> {
        let ids = all_metadata(&self.db)?;
        let timestamp = now()?;
        let mut count = 0;
        for t in ids {
            if t.expires_at_unix <= timestamp
                && !matches!(t.capture, CapturePhase::Released | CapturePhase::Expired)
            {
                self.interrupt(&t.id)?;
                self.erase(&t.id, CapturePhase::Expired, None)?;
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn close(&mut self) -> Result<StorageCleanupReceipt> {
        self.db
            .execute("UPDATE identity SET closed=1 WHERE singleton=1", [])
            .map_err(|_| Error::Storage)?;
        let ids = all_metadata(&self.db)?;
        for t in &ids {
            self.interrupt(&t.id)?;
            self.erase(&t.id, CapturePhase::Released, None)?;
        }
        let remaining_payload_bytes: u32 = self
            .db
            .query_row(
                "SELECT COALESCE(SUM(length(body)),0) FROM payloads",
                [],
                |r| r.get(0),
            )
            .map_err(|_| Error::Storage)?;
        let capacity: u32 = self
            .db
            .query_row("SELECT COALESCE(SUM(capacity),0) FROM transfers", [], |r| {
                r.get(0)
            })
            .map_err(|_| Error::Storage)?;
        let current = all_metadata(&self.db)?;
        let native_operations_without_receipts = current
            .iter()
            .flat_map(|t| [&t.source, &t.receiver])
            .filter(|stage| stage.operation_id.is_some() && stage.receipt.is_none())
            .count();
        Ok(StorageCleanupReceipt {
            admission_closed: true,
            retained_transfer_records: current.len(),
            remaining_payload_bytes,
            native_operations_without_receipts,
            storage_cleanup_verified: remaining_payload_bytes == 0 && capacity == 0,
        })
    }

    fn erase(&mut self, id: &str, phase: CapturePhase, grant: Option<&Grant>) -> Result<Transfer> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Storage)?;
        let (index, mut t, source, _) = row(&tx, id)?;
        if let Some(grant) = grant {
            if grant.expires_at_unix <= now()? || grant_identity(grant)? != source {
                return Err(Error::Access);
            }
            if t.capture == CapturePhase::Started
                || (t.receiver.operation_id.is_some()
                    && t.receiver.receipt.is_none()
                    && !t.receiver.interrupted)
            {
                return Err(Error::Phase);
            }
        }
        t.capture = phase;
        save(&tx, &mut t)?;
        tx.execute(
            "DELETE FROM payloads WHERE id IN (?1,?2)",
            params![index * 2, index * 2 + 1],
        )
        .map_err(|_| Error::Storage)?;
        tx.execute("UPDATE transfers SET capacity=0 WHERE handle=?1", [id])
            .map_err(|_| Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(t)
    }
}

fn admission(db: &Connection, grant: &Grant, t: &Transfer) -> Result<()> {
    let closed: bool = db
        .query_row("SELECT closed FROM identity WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Storage)?;
    if closed || grant.expires_at_unix <= now()? {
        return Err(Error::Access);
    }
    // Recheck identity after acquiring the write lock: a handoff may have
    // replaced the recipient since the caller's optimistic authorization read.
    let (_, current, source, destination) = row(db, &t.id)?;
    let identity = grant_identity(grant)?;
    if identity != source && identity != destination {
        return Err(Error::Access);
    }
    usable(&current)
}

fn claim_operation(db: &Connection, operation: &str) -> Result<()> {
    let count:u32=db.query_row("SELECT COUNT(*) FROM transfers WHERE json_extract(metadata,'$.source.operation_id')=?1 OR json_extract(metadata,'$.receiver.operation_id')=?1",[operation],|r|r.get(0)).map_err(|_|Error::Storage)?;
    if count == 0 {
        Ok(())
    } else {
        Err(Error::Conflict)
    }
}

fn encode(t: &Transfer) -> Result<String> {
    serde_json::to_string(t).map_err(|_| Error::Storage)
}
fn usable(t: &Transfer) -> Result<()> {
    if t.expires_at_unix <= now()?
        || matches!(t.capture, CapturePhase::Released | CapturePhase::Expired)
    {
        Err(Error::Access)
    } else {
        Ok(())
    }
}
fn row(db: &Connection, id: &str) -> Result<(i64, Transfer, String, String)> {
    let r: (i64, String, String, String) = db
        .query_row(
            "SELECT id,metadata,source_json,destination_json FROM transfers WHERE handle=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .ok_or(Error::Access)?;
    Ok((
        r.0,
        serde_json::from_str(&r.1).map_err(|_| Error::Storage)?,
        r.2,
        r.3,
    ))
}
fn grant_identity(grant: &Grant) -> Result<String> {
    serde_json::to_string(&(
        &grant.workspace,
        &grant.lab,
        &grant.principal,
        &grant.wallet,
        &grant.lease,
    ))
    .map_err(|_| Error::Invalid)
}
fn save(db: &Connection, t: &mut Transfer) -> Result<()> {
    let revision = t.revision;
    t.revision = t.revision.checked_add(1).ok_or(Error::Storage)?;
    let updated=db.execute("UPDATE transfers SET metadata=?1 WHERE handle=?2 AND json_extract(metadata,'$.revision')=?3",params![encode(t)?,t.id,i64::try_from(revision).map_err(|_|Error::Storage)?]).map_err(|_|Error::Storage)?;
    if updated == 1 {
        Ok(())
    } else {
        t.revision = revision;
        Err(Error::Conflict)
    }
}
fn all_metadata(db: &Connection) -> Result<Vec<Transfer>> {
    let mut stmt = db
        .prepare("SELECT metadata FROM transfers")
        .map_err(|_| Error::Storage)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| Error::Storage)?;
    rows.map(|r| serde_json::from_str(&r.map_err(|_| Error::Storage)?).map_err(|_| Error::Storage))
        .collect()
}
fn copy_bounded(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    limit: u32,
) -> Result<(u32, String)> {
    let mut buffer = [0; 8192];
    let mut count = 0u32;
    let mut hash = Sha256::new();
    loop {
        let n = reader.read(&mut buffer).map_err(|_| Error::Capture)?;
        if n == 0 {
            break;
        }
        let n32 = u32::try_from(n).map_err(|_| Error::Capture)?;
        if n32 > limit - count {
            return Err(Error::Capacity);
        }
        writer.write_all(&buffer[..n]).map_err(|_| Error::Capture)?;
        hash.update(&buffer[..n]);
        count += n32;
    }
    Ok((count, format!("{:x}", hash.finalize())))
}
fn verify_blob(db: &Connection, index: i64, column: &str, t: &Transfer) -> Result<()> {
    let size = t.bytes.ok_or(Error::Integrity)?;
    let offset = match column {
        "source_body" => 0,
        "inbox_body" => 1,
        _ => return Err(Error::Invalid),
    };
    let mut blob = db
        .blob_open("main", "payloads", "body", index * 2 + offset, true)
        .map_err(|_| Error::Storage)?;
    blob.seek(SeekFrom::Start(0)).map_err(|_| Error::Storage)?;
    let (read, digest) = copy_bounded(&mut blob.take(u64::from(size)), &mut std::io::sink(), size)?;
    if read != size || Some(digest) != t.sha256 {
        return Err(Error::Integrity);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
