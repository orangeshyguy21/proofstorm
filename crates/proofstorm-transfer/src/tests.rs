use super::*;
use std::io::Cursor;
use std::os::unix::process::ExitStatusExt;

fn tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn grants() -> (Grant, Grant) {
    let a = Grant {
        workspace: "workspace".into(),
        lab: "lab".into(),
        principal: "alice".into(),
        wallet: "cdk".into(),
        authority: "source-session".into(),
    };
    let mut b = a.clone();
    b.principal = "bob".into();
    b.wallet = "cocod".into();
    b.authority = "destination-session".into();
    (a, b)
}
fn native() -> NativeReceipt {
    NativeReceipt {
        exit_code: Some(0),
        exit_signal: None,
        timed_out: false,
        cancelled: false,
        cleanup_verified: true,
        streams_complete: true,
        output_truncated: false,
    }
}
fn produced(payload: &[u8], native: NativeReceipt) -> ProducedPayload {
    ProducedPayload {
        native,
        manifest: Some(PayloadManifest {
            bytes: u32::try_from(payload.len()).unwrap(),
            sha256: format!("{:x}", Sha256::digest(payload)),
        }),
    }
}
fn vault(root: &Path) -> Vault {
    Vault::open(root, "workspace", "lab", Limits::default()).unwrap()
}
fn captured(v: &mut Vault, a: &Grant, b: &Grant, key: &str, payload: &[u8]) -> Transfer {
    let t = v
        .prepare(a, b, key, u32::try_from(payload.len()).unwrap())
        .unwrap();
    v.begin_capture(a, &t.id, &format!("{key}-send")).unwrap();
    v.capture(
        a,
        &t.id,
        &format!("{key}-send"),
        &mut Cursor::new(payload),
        produced(payload, native()),
    )
    .unwrap()
}

#[test]
fn large_private_transfer_survives_restart_and_separates_native_receipts() {
    let root = tempdir();
    let (a, b) = grants();
    let payload = b"cashu-fixture-private-canary-".repeat(20000);
    let mut v = vault(root.path());
    let t = captured(&mut v, &a, &b, "first", &payload);
    assert_eq!(t.capture, CapturePhase::Ready);
    assert!(!t.delivered);
    assert!(
        !serde_json::to_string(&t)
            .unwrap()
            .contains("private-canary")
    );
    drop(v);
    let mut v = vault(root.path());
    let delivered = v.deliver(&b, &t.id).unwrap();
    assert!(delivered.delivered);
    assert!(delivered.receiver.operation_id.is_none());
    assert_eq!(v.deliver(&b, &t.id).unwrap(), delivered);
    v.begin_receive(&b, &t.id, "receive-1").unwrap();
    let mut received = vec![];
    v.consume(&b, &t.id, "receive-1", &mut received).unwrap();
    assert_eq!(received, payload);
    assert_eq!(
        v.consume(&b, &t.id, "receive-1", &mut vec![]),
        Err(Error::Phase)
    );
    let finished = v.finish_receive(&t.id, "receive-1", native()).unwrap();
    assert_eq!(finished.receiver.receipt, Some(native()));
    assert_eq!(finished.source.receipt, Some(native()));
    assert_eq!(
        v.begin_receive(&b, &t.id, "another-receive"),
        Err(Error::Phase)
    );
    assert_eq!(
        v.finish_receive(&t.id, "receive-1", native()).unwrap(),
        finished
    );
    v.observe(&b, &t.id, "native-wallet-check-1").unwrap();
    let receipt = v.status(&a, &t.id).unwrap();
    assert_eq!(receipt.observations, ["native-wallet-check-1"]);
    assert!(!serde_json::to_string(&receipt).unwrap().contains("spent"));
    v.release(&a, &t.id).unwrap();
    assert_eq!(v.deliver(&b, &t.id), Err(Error::Access));
    assert_eq!(
        v.db.query_row("SELECT COUNT(*) FROM payloads", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        fs::metadata(root.path().join("private.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn handles_do_not_authorize_other_principals_wallets_labs_or_authorities() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = captured(&mut v, &a, &b, "access", b"private-canary");
    for kind in 0..5 {
        let mut wrong = b.clone();
        match kind {
            0 => wrong.principal = "mallory".into(),
            1 => wrong.wallet = "other".into(),
            2 => wrong.lab = "other".into(),
            3 => wrong.authority = "other".into(),
            _ => wrong.workspace = "other".into(),
        }
        assert_eq!(v.status(&wrong, &t.id), Err(Error::Access));
        assert_eq!(v.deliver(&wrong, &t.id), Err(Error::Access));
    }
    assert_eq!(v.deliver(&a, &t.id), Err(Error::Access));
    assert_eq!(v.begin_capture(&b, &t.id, "wrong"), Err(Error::Access));
    assert_eq!(v.status(&b, "guessed-reference"), Err(Error::Access));
    assert!(matches!(
        Vault::open(root.path(), "workspace", "other-lab", Limits::default()),
        Err(Error::Access)
    ));
}

#[test]
fn capacity_is_reserved_before_producer_and_idempotency_survives_reconnection() {
    let root = tempdir();
    let limits = Limits {
        payload_bytes: 100,
        lab_bytes: 200,
        active_transfers: 1,
        retention_seconds: 60,
    };
    let mut v = Vault::open(root.path(), "workspace", "lab", limits).unwrap();
    let (a, b) = grants();
    let t = v.prepare(&a, &b, "same", 100).unwrap();
    assert_eq!(v.prepare(&a, &b, "same", 100).unwrap(), t);
    assert_eq!(v.prepare(&a, &b, "same", 99), Err(Error::Conflict));
    assert_eq!(v.prepare(&a, &b, "second", 100), Err(Error::Capacity));
    assert_eq!(
        v.db.query_row("SELECT SUM(length(body)) FROM payloads", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        200
    );
    let mut reopened = Vault::open(root.path(), "workspace", "lab", limits).unwrap();
    assert_eq!(
        reopened.prepare(&a, &b, "second", 100),
        Err(Error::Capacity)
    );
    v.begin_capture(&a, &t.id, "one-native-send").unwrap();
    assert_eq!(
        reopened.begin_capture(&a, &t.id, "second-native-send"),
        Err(Error::Phase)
    );
    assert_eq!(v.release(&a, &t.id), Err(Error::Phase));
    v.interrupt(&t.id).unwrap();
    v.release(&a, &t.id).unwrap();
    assert!(reopened.prepare(&a, &b, "second", 100).is_ok());
}

struct BrokenRead {
    done: bool,
}
impl Read for BrokenRead {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Err(std::io::Error::other("private-canary"));
        }
        self.done = true;
        out[..3].copy_from_slice(b"abc");
        Ok(3)
    }
}

#[test]
fn partial_oversized_and_unsuccessful_capture_never_replay_native_send() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    for (key, mut reader, native) in [
        (
            "partial",
            Box::new(BrokenRead { done: false }) as Box<dyn Read>,
            native(),
        ),
        (
            "oversized",
            Box::new(Cursor::new(b"12345")) as Box<dyn Read>,
            native(),
        ),
        (
            "nonzero",
            Box::new(Cursor::new(b"123")) as Box<dyn Read>,
            NativeReceipt {
                exit_code: Some(3),
                ..native()
            },
        ),
    ] {
        let t = v.prepare(&a, &b, key, 4).unwrap();
        v.begin_capture(&a, &t.id, key).unwrap();
        let error = v
            .capture(&a, &t.id, key, &mut reader, produced(b"test", native))
            .unwrap_err();
        assert_eq!(error, Error::Capture);
        assert!(!format!("{error:?}").contains("private-canary"));
        let t = v.status(&a, &t.id).unwrap();
        assert_eq!(t.capture, CapturePhase::Unknown);
        assert_eq!(t.source.receipt, Some(native));
        assert_eq!(v.deliver(&b, &t.id), Err(Error::Phase));
        assert_eq!(v.begin_capture(&a, &t.id, "repeat-send"), Err(Error::Phase));
    }
}

#[test]
fn tampering_is_rejected_before_native_receive_admission() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = captured(&mut v, &a, &b, "tamper", b"private-canary");
    let index = row(&v.db, &t.id).unwrap().0;
    v.db.execute(
        "UPDATE payloads SET body=zeroblob(length(body)) WHERE id=?1",
        [index * 2],
    )
    .unwrap();
    assert_eq!(v.deliver(&b, &t.id), Err(Error::Integrity));
    assert!(!v.status(&b, &t.id).unwrap().delivered);
    let t = captured(&mut v, &a, &b, "inbox", b"private-canary");
    v.deliver(&b, &t.id).unwrap();
    let index = row(&v.db, &t.id).unwrap().0;
    v.db.execute(
        "UPDATE payloads SET body=zeroblob(length(body)) WHERE id=?1",
        [index * 2 + 1],
    )
    .unwrap();
    assert_eq!(v.begin_receive(&b, &t.id, "receive"), Err(Error::Integrity));
}

#[test]
fn native_interruption_is_unknown_and_expiry_does_not_mean_spent() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = captured(&mut v, &a, &b, "interrupt", b"private-canary");
    v.deliver(&b, &t.id).unwrap();
    v.begin_receive(&b, &t.id, "receive").unwrap();
    drop(v);
    let mut v = vault(root.path());
    let interrupted = v.interrupt(&t.id).unwrap();
    assert!(interrupted.receiver.interrupted);
    assert_eq!(v.begin_receive(&b, &t.id, "retry"), Err(Error::Phase));
    assert_eq!(
        v.consume(&b, &t.id, "receive", &mut vec![]),
        Err(Error::Phase)
    );
    let (_, mut expiring, _, _) = row(&v.db, &t.id).unwrap();
    expiring.expires_at_unix = 0;
    save(&v.db, &mut expiring).unwrap();
    assert_eq!(v.expire().unwrap(), 1);
    let expired = v.status(&a, &t.id).unwrap();
    assert_eq!(expired.capture, CapturePhase::Expired);
    assert!(expired.receiver.receipt.is_none());
    // The accepted native operation may report after payload retention expiry.
    assert_eq!(
        v.finish_receive(&t.id, "receive", native())
            .unwrap()
            .capture,
        CapturePhase::Expired
    );
    assert_eq!(
        v.db.query_row("SELECT COUNT(*) FROM payloads", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    let cleanup = v.close().unwrap();
    assert!(cleanup.admission_closed && cleanup.storage_cleanup_verified);
    assert_eq!(cleanup.remaining_payload_bytes, 0);
    assert_eq!(v.prepare(&a, &b, "after-close", 1), Err(Error::Phase));
}

#[test]
fn unsafe_storage_permissions_and_symlinks_are_refused() {
    let root = tempdir();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        Vault::open(root.path(), "workspace", "lab", Limits::default()),
        Err(Error::Storage)
    ));
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(target.path(), root.path().join("private.sqlite3")).unwrap();
    assert!(matches!(
        Vault::open(root.path(), "workspace", "lab", Limits::default()),
        Err(Error::Storage)
    ));
}

#[test]
fn release_rechecks_the_receiver_fence_inside_the_erase_transaction() {
    let root = tempdir();
    let mut a = vault(root.path());
    let (source, destination) = grants();
    let t = captured(&mut a, &source, &destination, "race", b"private-canary");
    a.deliver(&destination, &t.id).unwrap();
    // Pause a release after its initial authorization/read, then let another
    // connection accept receive before release enters its erase transaction.
    let (_, snapshot) = a.authorized(&source, &t.id, Some(false)).unwrap();
    assert!(snapshot.receiver.operation_id.is_none());
    let mut b = vault(root.path());
    b.begin_receive(&destination, &t.id, "concurrent-receive")
        .unwrap();
    assert_eq!(
        a.erase(&t.id, CapturePhase::Released, Some(&source)),
        Err(Error::Phase)
    );
    assert_eq!(
        a.db.query_row("SELECT COUNT(*) FROM payloads", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(
        a.status(&source, &t.id)
            .unwrap()
            .receiver
            .operation_id
            .is_some()
    );
}

#[test]
fn concurrent_observation_cannot_discard_a_one_shot_producer_stream() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = v.prepare(&a, &b, "observe-race", 100).unwrap();
    v.begin_capture(&a, &t.id, "send").unwrap();
    let mut staged = v.finish_source(&t.id, "send", native()).unwrap();
    staged.source_manifest = produced(b"one-shot-private-payload", native()).manifest;
    save(&v.db, &mut staged).unwrap();
    let index = row(&v.db, &t.id).unwrap().0;
    let mut other = vault(root.path());
    other.observe(&a, &t.id, "native-check").unwrap();
    let mut stream = Cursor::new(b"one-shot-private-payload");
    let captured = v.capture_staged(&a, index, staged, &mut stream).unwrap();
    assert_eq!(captured.capture, CapturePhase::Ready);
    assert_eq!(captured.observations, ["native-check"]);
    v.deliver(&b, &t.id).unwrap();
    v.begin_receive(&b, &t.id, "receive").unwrap();
    let mut input = vec![];
    v.consume(&b, &t.id, "receive", &mut input).unwrap();
    assert_eq!(input, b"one-shot-private-payload");
}

#[test]
fn native_operation_identity_cannot_be_reused_across_transfers_or_roles() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let one = captured(&mut v, &a, &b, "identity", b"payload");
    v.deliver(&b, &one.id).unwrap();
    assert_eq!(
        v.begin_receive(&b, &one.id, "identity-send"),
        Err(Error::Conflict)
    );
    let two = v.prepare(&a, &b, "another", 7).unwrap();
    assert_eq!(
        v.begin_capture(&a, &two.id, "identity-send"),
        Err(Error::Conflict)
    );
    v.release(&a, &one.id).unwrap();
    assert_eq!(
        v.begin_capture(&a, &two.id, "identity-send"),
        Err(Error::Conflict)
    );
}

#[test]
fn competing_admission_cannot_exceed_capacity_or_start_native_work_twice() {
    let root = tempdir();
    let limits = Limits {
        payload_bytes: 100,
        lab_bytes: 200,
        active_transfers: 1,
        retention_seconds: 60,
    };
    let _initial = Vault::open(root.path(), "workspace", "lab", limits).unwrap();
    let (a, b) = grants();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = vec![];
    for key in ["one", "two"] {
        let path = root.path().to_owned();
        let a = a.clone();
        let b = b.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut v = Vault::open(&path, "workspace", "lab", limits).unwrap();
            barrier.wait();
            v.prepare(&a, &b, key, 100)
        }));
    }
    let results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(Error::Capacity))
            .count(),
        1
    );
    let id = results.into_iter().find_map(Result::ok).unwrap().id;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = vec![];
    for op in ["one-native", "two-native"] {
        let path = root.path().to_owned();
        let a = a.clone();
        let id = id.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut v = vault(&path);
            barrier.wait();
            v.begin_capture(&a, &id, op)
        }));
    }
    let results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, Error::Conflict | Error::Phase))
    );
}

struct BrokenWrite {
    done: bool,
}
impl Write for BrokenWrite {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.done {
            return Err(std::io::Error::other("private-canary"));
        }
        self.done = true;
        Ok(bytes.len().min(3))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_consumer_input_is_not_replayed_and_late_source_receipts_are_retained() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = captured(&mut v, &a, &b, "input", b"private-canary");
    v.deliver(&b, &t.id).unwrap();
    v.begin_receive(&b, &t.id, "receive").unwrap();
    assert_eq!(
        v.consume(&b, &t.id, "receive", &mut BrokenWrite { done: false }),
        Err(Error::Delivery)
    );
    assert!(v.status(&b, &t.id).unwrap().receiver.interrupted);
    assert_eq!(
        v.consume(&b, &t.id, "receive", &mut vec![]),
        Err(Error::Phase)
    );
    let t = v.prepare(&a, &b, "late", 10).unwrap();
    v.begin_capture(&a, &t.id, "late-source").unwrap();
    let (_, mut expired, _, _) = row(&v.db, &t.id).unwrap();
    expired.expires_at_unix = 0;
    save(&v.db, &mut expired).unwrap();
    v.expire().unwrap();
    let completed = v.finish_source(&t.id, "late-source", native()).unwrap();
    assert_eq!(completed.capture, CapturePhase::Expired);
    assert_eq!(completed.source.receipt, Some(native()));
}

#[test]
fn an_incomplete_native_capture_is_not_published_as_a_complete_payload() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    for (key, native) in [
        (
            "truncated",
            NativeReceipt {
                output_truncated: true,
                ..native()
            },
        ),
        (
            "incomplete",
            NativeReceipt {
                streams_complete: false,
                ..native()
            },
        ),
    ] {
        let t = v.prepare(&a, &b, key, 100).unwrap();
        v.begin_capture(&a, &t.id, key).unwrap();
        let mut reader = Cursor::new(b"partial-token");
        assert_eq!(
            v.capture(&a, &t.id, key, &mut reader, produced(b"test", native)),
            Err(Error::Capture)
        );
        assert_eq!(reader.position(), 0);
        let t = v.status(&a, &t.id).unwrap();
        assert_eq!(t.capture, CapturePhase::Unknown);
        assert_eq!(t.source.receipt, Some(native));
    }
}

#[test]
fn early_eof_or_altered_bytes_on_the_source_hop_fail_the_manifest_check() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    for (key, bytes) in [
        ("early-eof", b"abc".as_slice()),
        ("altered", b"abczef".as_slice()),
    ] {
        let t = v.prepare(&a, &b, key, 100).unwrap();
        v.begin_capture(&a, &t.id, key).unwrap();
        let mut reader = Cursor::new(bytes);
        assert_eq!(
            v.capture(&a, &t.id, key, &mut reader, produced(b"abcdef", native())),
            Err(Error::Capture)
        );
        assert_eq!(v.status(&a, &t.id).unwrap().capture, CapturePhase::Unknown);
        assert_eq!(v.deliver(&b, &t.id), Err(Error::Phase));
    }
}

#[test]
#[ignore = "subprocess fixture for the process-kill test"]
fn crash_capture_worker() {
    struct BlockingRead {
        root: std::path::PathBuf,
        calls: u8,
    }
    impl Read for BlockingRead {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                bytes[..4096].fill(b'x');
                return Ok(4096);
            }
            fs::write(self.root.join("capture-started"), b"ready")?;
            loop {
                std::thread::park();
            }
        }
    }
    let root = std::path::PathBuf::from(std::env::var("PROOFSTORM_TRANSFER_CRASH_ROOT").unwrap());
    let id = std::env::var("PROOFSTORM_TRANSFER_CRASH_ID").unwrap();
    let mut v = vault(&root);
    let (a, _) = grants();
    v.begin_capture(&a, &id, "crash-source").unwrap();
    v.capture(
        &a,
        &id,
        "crash-source",
        &mut BlockingRead { root, calls: 0 },
        produced(&vec![b'x'; 100_000], native()),
    )
    .unwrap();
}

#[test]
fn process_kill_rolls_back_partial_bytes_without_resetting_native_admission() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = v.prepare(&a, &b, "kill", 100_000).unwrap();
    drop(v);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "tests::crash_capture_worker"])
        .env("PROOFSTORM_TRANSFER_CRASH_ROOT", root.path())
        .env("PROOFSTORM_TRANSFER_CRASH_ID", &t.id)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !root.path().join("capture-started").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let started = root.path().join("capture-started").exists();
    let _ = child.kill();
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(9));
    assert!(started);
    let mut v = vault(root.path());
    // Inspect custody before interrupt/release can erase the crash evidence.
    let (index, _, _, _) = row(&v.db, &t.id).unwrap();
    for blob_id in [index * 2, index * 2 + 1] {
        let body: Vec<u8> =
            v.db.query_row("SELECT body FROM payloads WHERE id=?1", [blob_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body, vec![0; 100_000]);
    }
    let interrupted = v.interrupt(&t.id).unwrap();
    assert_eq!(interrupted.capture, CapturePhase::Unknown);
    assert_eq!(interrupted.source.receipt, Some(native()));
    assert_eq!(
        v.begin_capture(&a, &t.id, "duplicate-send"),
        Err(Error::Phase)
    );
    assert_eq!(v.deliver(&b, &t.id), Err(Error::Phase));
    v.release(&a, &t.id).unwrap();
    assert_eq!(
        v.db.query_row("SELECT COUNT(*) FROM payloads", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
}

#[test]
fn competing_receivers_and_consumers_get_only_one_admission() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let payload = b"private-concurrent-input";
    let t = captured(&mut v, &a, &b, "competing-input", payload);
    v.deliver(&b, &t.id).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = ["receive-one", "receive-two"]
        .into_iter()
        .map(|op| {
            let mut peer = vault(root.path());
            let b = b.clone();
            let id = t.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                peer.begin_receive(&b, &id, op)
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let operation = results
        .into_iter()
        .find_map(Result::ok)
        .unwrap()
        .receiver
        .operation_id
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let mut peer = vault(root.path());
            let b = b.clone();
            let id = t.id.clone();
            let operation = operation.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut output = vec![];
                barrier.wait();
                let result = peer.consume(&b, &id, &operation, &mut output);
                (result, output)
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(r, _)| r.is_ok()).count(), 1);
    for (result, bytes) in results {
        if result.is_ok() {
            assert_eq!(bytes, payload);
        } else {
            assert!(bytes.is_empty());
        }
    }
    let mut reopened = vault(root.path());
    assert_eq!(
        reopened.consume(&b, &t.id, &operation, &mut vec![]),
        Err(Error::Phase)
    );
}

#[test]
fn flush_failure_preserves_the_input_fence_and_late_receipt() {
    struct FlushFailure(Vec<u8>);
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("private-canary"))
        }
    }
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = captured(&mut v, &a, &b, "flush", b"private-canary");
    v.deliver(&b, &t.id).unwrap();
    v.begin_receive(&b, &t.id, "flush-receive").unwrap();
    let mut writer = FlushFailure(vec![]);
    assert_eq!(
        v.consume(&b, &t.id, "flush-receive", &mut writer),
        Err(Error::Delivery)
    );
    assert_eq!(writer.0, b"private-canary");
    let current = v.status(&b, &t.id).unwrap();
    assert!(current.receiver.input_started && current.receiver.interrupted);
    assert_eq!(
        v.consume(&b, &t.id, "flush-receive", &mut vec![]),
        Err(Error::Phase)
    );
    assert_eq!(
        v.finish_receive(&t.id, "flush-receive", native())
            .unwrap()
            .receiver
            .receipt,
        Some(native())
    );
}

#[test]
fn failed_storage_reservation_and_erase_roll_back_atomically() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    v.db.execute_batch("CREATE TRIGGER fail_inbox BEFORE INSERT ON payloads WHEN NEW.id % 2 = 1 BEGIN SELECT RAISE(ABORT, 'private-canary'); END;").unwrap();
    assert_eq!(v.prepare(&a, &b, "allocation", 100), Err(Error::Storage));
    for table in ["transfers", "payloads"] {
        let count: u32 =
            v.db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
        assert_eq!(count, 0);
    }
    v.db.execute_batch("DROP TRIGGER fail_inbox;").unwrap();
    let t = captured(&mut v, &a, &b, "erase", b"private-canary");
    v.db.execute_batch("CREATE TRIGGER fail_erase BEFORE DELETE ON payloads BEGIN SELECT RAISE(ABORT, 'private-canary'); END;").unwrap();
    assert_eq!(v.release(&a, &t.id), Err(Error::Storage));
    assert_eq!(v.status(&a, &t.id).unwrap(), t);
    let capacity: u32 =
        v.db.query_row(
            "SELECT capacity FROM transfers WHERE handle=?1",
            [&t.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(capacity, t.maximum_bytes * 2);
    v.deliver(&b, &t.id).unwrap();
    v.db.execute_batch("DROP TRIGGER fail_erase;").unwrap();
    v.release(&a, &t.id).unwrap();
}

#[test]
fn storage_close_reports_unresolved_native_work_separately() {
    let root = tempdir();
    let mut v = vault(root.path());
    let (a, b) = grants();
    let t = v.prepare(&a, &b, "close-active", 100).unwrap();
    v.begin_capture(&a, &t.id, "close-send").unwrap();
    let cleanup = v.close().unwrap();
    assert!(cleanup.admission_closed && cleanup.storage_cleanup_verified);
    assert_eq!(cleanup.native_operations_without_receipts, 1);
    assert_eq!(cleanup.remaining_payload_bytes, 0);
    assert_eq!(cleanup.retained_transfer_records, 1);
    assert_eq!(
        v.finish_source(&t.id, "close-send", native())
            .unwrap()
            .capture,
        CapturePhase::Released
    );
    assert_eq!(v.close().unwrap().native_operations_without_receipts, 0);
    assert_eq!(v.prepare(&a, &b, "closed", 1), Err(Error::Phase));
}

#[test]
fn transient_collection_failure_can_resume_custody_without_another_native_export() {
    let root = tempdir();
    let (a, b) = grants();
    let mut v = vault(root.path());
    let t = v.prepare(&a, &b, "retry-custody", 100).unwrap();
    v.begin_capture(&a, &t.id, "original-send").unwrap();
    v.finish_source(&t.id, "original-send", native()).unwrap();
    // Source collection failed before any reader reached the vault. Its native
    // receipt/handle remain persisted while the controller retries collection.
    drop(v);
    let mut v = vault(root.path());
    assert_eq!(v.begin_capture(&a, &t.id, "second-send"), Err(Error::Phase));
    v.finish_source(&t.id, "original-send", native()).unwrap();
    let body = b"original-private-payload";
    let completed = v
        .capture(
            &a,
            &t.id,
            "original-send",
            &mut Cursor::new(body),
            produced(body, native()),
        )
        .unwrap();
    assert_eq!(completed.capture, CapturePhase::Ready);
    assert_eq!(
        completed.source.operation_id.as_deref(),
        Some("original-send")
    );
    assert_eq!(completed.source.receipt, Some(native()));
}

#[test]
fn handoff_rebinds_only_the_recipient_without_resetting_native_fences() {
    let root = tempdir();
    let (a, mut original) = grants();
    original.principal = a.principal.clone();
    original.authority = a.authority.clone();
    let mut child = original.clone();
    child.principal = "bob".into();
    child.authority = "child".into();
    let payload = b"private-handoff-fixture";
    let mut v = vault(root.path());
    let t = captured(&mut v, &a, &original, "handoff", payload);
    let handed = v.handoff(&a, &child, &t.id).unwrap();
    assert_eq!(handed.source, t.source);
    assert_eq!(handed.sha256, t.sha256);
    assert_eq!(handed.recipient.as_ref().unwrap().principal, "bob");
    assert_eq!(v.handoff(&a, &child, &t.id).unwrap(), handed);
    assert!(v.status(&original, &t.id).is_err());
    assert!(v.deliver(&original, &t.id).is_err());
    let mut stranger = child.clone();
    stranger.principal = "mallory".into();
    assert!(v.handoff(&a, &stranger, &t.id).is_err());
    assert!(v.status(&stranger, &t.id).is_err());
    drop(v);
    let mut v = vault(root.path());
    v.deliver(&child, &t.id).unwrap();
    v.begin_receive(&child, &t.id, "receive").unwrap();
    let mut bytes = Vec::new();
    v.consume(&child, &t.id, "receive", &mut bytes).unwrap();
    assert_eq!(bytes, payload);
    assert!(v.begin_receive(&child, &t.id, "duplicate").is_err());
    assert!(v.release(&a, &t.id).is_err());
    v.finish_receive(&t.id, "receive", native()).unwrap();
    v.release(&a, &t.id).unwrap();
    assert!(v.handoff(&a, &child, &t.id).is_err());
}

#[test]
fn handoff_requires_completed_capture_and_fixed_destination() {
    let root = tempdir();
    let (a, original) = grants();
    let mut child = original.clone();
    child.authority = "child".into();
    let mut v = vault(root.path());
    let t = v.prepare(&a, &original, "pending", 32).unwrap();
    assert!(v.handoff(&a, &child, &t.id).is_err());
    let t = captured(&mut v, &a, &original, "ready", b"fixture");
    let mut wrong = child.clone();
    wrong.wallet = "other".into();
    assert!(v.handoff(&a, &wrong, &t.id).is_err());
    wrong = child.clone();
    wrong.workspace = "other".into();
    assert!(v.handoff(&a, &wrong, &t.id).is_err());
    assert!(v.handoff(&original, &child, &t.id).is_err());
    v.deliver(&original, &t.id).unwrap();
    assert!(v.handoff(&a, &child, &t.id).is_err());
}

#[test]
fn stale_recipient_grant_is_rechecked_inside_the_admission_transaction() {
    let root = tempdir();
    let (a, original) = grants();
    let mut child = original.clone();
    child.authority = "child".into();
    let mut v = vault(root.path());
    let t = captured(&mut v, &a, &original, "stale", b"fixture");
    let (_, before) = v.authorized(&original, &t.id, Some(true)).unwrap();
    v.handoff(&a, &child, &t.id).unwrap();
    let tx =
        v.db.transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
    assert!(admission(&tx, &original, &before).is_err());
    assert!(admission(&tx, &child, &before).is_ok());
}

#[test]
fn legacy_custody_keeps_bytes_handles_and_prepare_replay_after_session_removal() {
    let root = tempdir();
    let (mut a, mut b) = grants();
    let mut v = vault(root.path());
    let original = captured(&mut v, &a, &b, "original", b"retained-private-payload");
    v.db.pragma_update(None, "user_version", 0).unwrap();
    drop(v);
    a.authority = "owner".into();
    b.authority = "owner".into();
    let mut reopened = vault(root.path());
    let replay = reopened
        .prepare(&a, &b, "original", original.maximum_bytes)
        .unwrap();
    assert_eq!(replay.id, original.id);
    assert_eq!(
        reopened.status(&a, &original.id).unwrap().sha256,
        original.sha256
    );
    reopened.deliver(&b, &original.id).unwrap();
}
