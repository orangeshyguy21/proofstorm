use super::*;
use std::os::unix::fs::symlink;

fn capture_fixture() -> (
    tempfile::TempDir,
    proofstorm_core::workspace::evidence::CaptureRequest,
    Value,
) {
    let root = tempfile::tempdir().unwrap();
    let task = root.path().join(".proofstorm/tasks/report");
    fs::create_dir_all(&task).unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::create_dir_all(root.path().join("output/report")).unwrap();
    fs::write(root.path().join("src/report.sh"), "echo report").unwrap();
    let source_digest = snapshot(&root.path().join("src"), &task.join("source")).unwrap();
    let start: proofstorm_core::workspace::TaskStart = serde_json::from_value(
        json!({"task_id":"report","argv":["sh","report.sh"],"env":{"TEST_INPUT":"captured"}}),
    )
    .unwrap();
    write_json(&task.join("task.json"), &json!(start)).unwrap();
    fs::write(
        root.path().join("output/report/result.bin"),
        [0, 255, 1, 128],
    )
    .unwrap();
    fs::write(task.join("stdout.log"), "current log").unwrap();
    fs::write(task.join("stdout.log.previous"), "previous log").unwrap();
    let state = json!({"task_id":"report","phase":"running","request_digest":proofstorm_core::digest_json(&start),"source_digest":source_digest});
    let request = serde_json::from_value(json!({"capture_id":"capture-1","request_digest":proofstorm_core::digest_json(&"capture"),"selection":{"task_id":"report","output_paths":["result.bin"]}})).unwrap();
    (root, request, state)
}

#[test]
fn evidence_captures_freeze_binary_files_inputs_and_optional_rotated_logs() {
    use proofstorm_core::workspace::evidence::TaskCapture;
    let (root, mut request, state) = capture_fixture();
    capture::freeze(root.path(), &request, state.clone()).unwrap();
    let original = capture::read(root.path(), &request).unwrap();
    let frozen: TaskCapture = serde_json::from_slice(&original).unwrap();
    assert_eq!(frozen.task["phase"], "running");
    assert_eq!(frozen.request.env["TEST_INPUT"], "captured");
    assert_eq!(
        frozen
            .files
            .iter()
            .find(|f| f.path == "output/result.bin")
            .unwrap()
            .bytes()
            .unwrap(),
        [0, 255, 1, 128]
    );
    assert!(!frozen.files.iter().any(|f| f.path.starts_with("logs/")));
    fs::write(root.path().join("output/report/result.bin"), "changed").unwrap();
    capture::freeze(root.path(), &request, json!({"phase":"failed"})).unwrap();
    assert_eq!(capture::read(root.path(), &request).unwrap(), original);
    let original_request = request.request_digest.clone();
    request.request_digest = proofstorm_core::digest_json(&"different run or component");
    assert!(capture::freeze(root.path(), &request, state.clone()).is_err());
    request.request_digest = original_request;
    request.selection.include_logs = true;
    assert!(capture::freeze(root.path(), &request, state.clone()).is_err());
    request.capture_id = "capture-2".into();
    capture::freeze(root.path(), &request, state).unwrap();
    let next: TaskCapture =
        serde_json::from_slice(&capture::read(root.path(), &request).unwrap()).unwrap();
    assert_eq!(
        next.files
            .iter()
            .filter(|f| f.path.starts_with("logs/"))
            .count(),
        2
    );
    assert_eq!(
        next.files
            .iter()
            .find(|f| f.path == "output/result.bin")
            .unwrap()
            .bytes()
            .unwrap(),
        b"changed"
    );
    capture::release(root.path(), "capture-1").unwrap();
    capture::release(root.path(), "capture-1").unwrap();
}

#[test]
fn evidence_refuses_partial_selection_links_oversized_files_and_modified_source() {
    let (root, request, state) = capture_fixture();
    let output = root.path().join("output/report/result.bin");
    fs::remove_file(&output).unwrap();
    assert!(capture::freeze(root.path(), &request, state.clone()).is_err());
    symlink("/etc/passwd", &output).unwrap();
    assert!(capture::freeze(root.path(), &request, state.clone()).is_err());
    fs::remove_file(&output).unwrap();
    fs::File::create(&output)
        .unwrap()
        .set_len(proofstorm_core::workspace::evidence::MAX_CAPTURE_FILE_BYTES as u64 + 1)
        .unwrap();
    assert!(capture::freeze(root.path(), &request, state.clone()).is_err());
    fs::write(output, "small").unwrap();
    fs::write(
        root.path()
            .join(".proofstorm/tasks/report/source/report.sh"),
        "changed after start",
    )
    .unwrap();
    assert!(capture::freeze(root.path(), &request, state).is_err());
    assert!(
        !root
            .path()
            .join(".proofstorm/captures/capture-1.json")
            .exists()
    );
}

#[test]
fn snapshots_freeze_code_and_preserve_executable_modes() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("src");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("run.sh"), "echo original").unwrap();
    fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let captured = root.path().join("captured");
    let digest = snapshot(&source, &captured).unwrap();
    fs::write(source.join("run.sh"), "echo changed").unwrap();
    assert_eq!(
        fs::read_to_string(captured.join("run.sh")).unwrap(),
        "echo original"
    );
    assert_eq!(
        fs::metadata(captured.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert_ne!(
        snapshot(&source, &root.path().join("second")).unwrap(),
        digest
    );
}

#[test]
fn snapshots_refuse_symlinks_and_oversized_sources() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("src");
    fs::create_dir(&source).unwrap();
    symlink("/etc/passwd", source.join("outside")).unwrap();
    assert!(snapshot(&source, &root.path().join("first")).is_err());
    fs::remove_file(source.join("outside")).unwrap();
    fs::File::create(source.join("large"))
        .unwrap()
        .set_len(MAX_SOURCE_BYTES + 1)
        .unwrap();
    assert!(snapshot(&source, &root.path().join("second")).is_err());
}

#[test]
fn file_access_is_atomic_bounded_and_refuses_traversal() {
    let root = tempfile::tempdir().unwrap();
    file_request(
        root.path(),
        &FileRequest::Write {
            path: "src/nested/script.sh".into(),
            content: "a".repeat(2500),
        },
    )
    .unwrap();
    let first = file_request(
        root.path(),
        &FileRequest::Read {
            path: "src/nested/script.sh".into(),
            offset: 0,
        },
    )
    .unwrap();
    assert_eq!(first["bytes"], 1024);
    assert_eq!(first["next_offset"], 1024);
    let last = file_request(
        root.path(),
        &FileRequest::Read {
            path: "src/nested/script.sh".into(),
            offset: 2048,
        },
    )
    .unwrap();
    assert_eq!(last["bytes"], 452);
    assert_eq!(last["next_offset"], Value::Null);
    symlink("/tmp", root.path().join("outside")).unwrap();
    for path in [
        "../escape",
        "/tmp/escape",
        ".proofstorm/request",
        "outside/escape",
    ] {
        assert!(
            file_request(
                root.path(),
                &FileRequest::Write {
                    path: path.into(),
                    content: "no".into()
                }
            )
            .is_err()
        );
    }
    file_request(
        root.path(),
        &FileRequest::Write {
            path: "src/nested/script.sh".into(),
            content: "replacement".into(),
        },
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("src/nested/script.sh")).unwrap(),
        "replacement"
    );
}

#[test]
fn directory_pages_do_not_drop_entries() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    for index in 0..20 {
        fs::write(root.path().join(format!("src/{index:02}")), "").unwrap();
    }
    let first = file_request(
        root.path(),
        &FileRequest::List {
            path: "src".into(),
            after: None,
        },
    )
    .unwrap();
    let second = file_request(
        root.path(),
        &FileRequest::List {
            path: "src".into(),
            after: Some(first["next_after"].as_str().unwrap().into()),
        },
    )
    .unwrap();
    assert_eq!(first["entries"].as_array().unwrap().len(), 16);
    assert_eq!(second["entries"].as_array().unwrap().len(), 4);
    assert_eq!(second["next_after"], Value::Null);
}

#[test]
fn file_read_pages_preserve_utf8_across_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let text = format!("{}🦀tail", "a".repeat(1022));
    file_request(
        root.path(),
        &FileRequest::Write {
            path: "src/unicode".into(),
            content: text.clone(),
        },
    )
    .unwrap();
    let first = file_request(
        root.path(),
        &FileRequest::Read {
            path: "src/unicode".into(),
            offset: 0,
        },
    )
    .unwrap();
    assert_eq!(first["next_offset"], 1022);
    let second = file_request(
        root.path(),
        &FileRequest::Read {
            path: "src/unicode".into(),
            offset: 1022,
        },
    )
    .unwrap();
    assert_eq!(
        format!(
            "{}{}",
            first["content"].as_str().unwrap(),
            second["content"].as_str().unwrap()
        ),
        text
    );
}
