//! Real Linux process lifecycle contracts; no component runtimes or live funds.
#![cfg(all(target_os = "linux", feature = "contract-tests"))]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn fixture(args: &[&str]) -> Value {
    json!(
        std::iter::once(env!("CARGO_BIN_EXE_proofstorm-exec-fixture"))
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
    )
}
struct Reply {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
fn invoke(args: &[&str], input: &[u8]) -> Reply {
    let mut stdin = tempfile::tempfile().unwrap();
    stdin.write_all(input).unwrap();
    stdin.rewind().unwrap();
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let runner = std::env::var_os("PROOFSTORM_NATIVE_RUNNER")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_proofstorm-exec").into());
    let mut child = Command::new(runner)
        .args(args)
        .stdin(Stdio::from(stdin))
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("supervisor control call exceeded deadline");
        }
        sleep(Duration::from_millis(10));
    };
    stdout.rewind().unwrap();
    stderr.rewind().unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out).unwrap();
    stderr.read_to_end(&mut err).unwrap();
    Reply {
        status,
        stdout: out,
        stderr: err,
    }
}
struct Harness {
    root: tempfile::TempDir,
    handles: Vec<PathBuf>,
}
impl Harness {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            handles: Vec::new(),
        }
    }
    fn start(&mut self, argv: Value, mut config: Value, input: Option<&[u8]>) -> PathBuf {
        let directory = self.root.path().join(self.handles.len().to_string());
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(input) = input {
            assert!(
                invoke(
                    &[
                        "input",
                        directory.to_str().unwrap(),
                        &input.len().to_string(),
                        &digest(input)
                    ],
                    input
                )
                .status
                .success()
            );
        }
        self.handles.push(directory.clone());
        config["argv"] = argv;
        if config.get("script").is_none() {
            config["script"] = json!("");
        }
        if config.get("timeout_seconds").is_none() {
            config["timeout_seconds"] = json!(2);
        }
        if config.get("output").is_none() {
            config["output"] = json!({"mode":"private"});
        }
        let reply = invoke(
            &["start", directory.to_str().unwrap()],
            &serde_json::to_vec(&config).unwrap(),
        );
        assert!(reply.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&reply.stdout).unwrap(),
            json!({"started":true})
        );
        directory
    }
    fn status(directory: &Path) -> Value {
        let reply = invoke(&["status", directory.to_str().unwrap()], b"");
        assert!(reply.status.success());
        serde_json::from_slice(&reply.stdout).unwrap()
    }
    fn receipt(directory: &Path) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let value = Self::status(directory);
            if value["running"] != true {
                assert_eq!(value["cleanup_verified"], true, "{value}");
                return value;
            }
            assert!(Instant::now() < deadline, "cleanup receipt deadline");
            sleep(Duration::from_millis(30));
        }
    }
    fn cancel(directory: &Path) {
        assert!(
            invoke(&["cancel", directory.to_str().unwrap()], b"")
                .status
                .success()
        );
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        for path in &self.handles {
            let _ = invoke(&["cancel", path.to_str().unwrap()], b"");
            // On a failing assertion still allow the owned supervisor to clean up.
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let reply = invoke(&["status", path.to_str().unwrap()], b"");
                if serde_json::from_slice::<Value>(&reply.stdout)
                    .is_ok_and(|value| value["running"] != true)
                {
                    break;
                }
                sleep(Duration::from_millis(30));
            }
        }
    }
}

#[test]
fn large_private_manifest_stdin_and_retirement() {
    let mut h = Harness::new();
    let body = "private-transfer-canary-".repeat(24_000).into_bytes();
    let source = h.start(
        fixture(&["repeat", "private-transfer-canary-", "24000", "", "0"]),
        json!({"private_io":{"kind":"capture","maximum_bytes":600000,"format":"bytes"}}),
        None,
    );
    let receipt = Harness::receipt(&source);
    assert_eq!(
        receipt["payload_manifest"],
        json!({"bytes":body.len(),"sha256":digest(&body)})
    );
    assert_eq!(receipt["stdout"], "");
    assert_eq!(receipt["stderr"], "");
    assert!(!receipt.to_string().contains("private-transfer-canary"));
    assert_eq!(
        invoke(&["payload", source.to_str().unwrap()], b"").stdout,
        body
    );
    let consumer = h.start(fixture(&["stdin","private-transfer-canary-","24000"]),json!({"private_io":{"kind":"consume","bytes":body.len(),"sha256":digest(&body),"input":{"kind":"stdin"}}}),Some(&body));
    assert_eq!(Harness::receipt(&consumer)["exit_code"], 0);
    for path in [source, consumer] {
        assert!(
            invoke(&["retire", path.to_str().unwrap()], b"")
                .status
                .success()
        );
        for name in ["input", "payload", "stdout"] {
            assert!(!path.join(name).exists());
        }
    }
}

#[test]
fn private_argv_capture_failures_tampering_and_duplicate_input() {
    let mut h = Harness::new();
    let body = b"cashuBprivate_test_token";
    let path = h.start(fixture(&["argv","cashuBprivate_test_token","@proofstorm-private-input"]),json!({"private_io":{"kind":"consume","bytes":body.len(),"sha256":digest(body),"input":{"kind":"argv","index":3}}}),Some(body));
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["exit_code"], 0);
    assert!(!receipt.to_string().contains("cashuBprivate_test_token"));
    for (body, maximum, code) in [
        ("cashuBfirst\ncashuBsecond".into(), 100, "0"),
        (format!("cashuB{}", "x".repeat(100)), 10, "0"),
        ("cashuBabcdef".into(), 100, "3"),
    ] {
        let path = h.start(
            fixture(&["emit", &body, "", code, "0"]),
            json!({"private_io":{"kind":"capture","maximum_bytes":maximum,"format":"cashu_token"}}),
            None,
        );
        let receipt = Harness::receipt(&path);
        assert!(receipt.get("payload_manifest").is_none());
        assert!(receipt.get("payload_error").is_some());
        assert!(!path.join("payload").exists());
    }
    let path = h.start(
        json!(["printf", "cashuBabcdef"]),
        json!({"private_io":{"kind":"capture","maximum_bytes":100,"format":"cashu_token"}}),
        None,
    );
    Harness::receipt(&path);
    fs::write(path.join("payload"), b"cashuBtampered").unwrap();
    let reply = invoke(&["payload", path.to_str().unwrap()], b"");
    assert!(!reply.status.success());
    assert!(reply.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&reply.stderr).contains("tampered"));
    let target = h.root.path().join("input-target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let hash = digest(b"abc");
    let args = ["input", target.to_str().unwrap(), "3", &hash];
    assert!(invoke(&args, b"abc").status.success());
    assert!(!invoke(&args, b"abc").status.success());
    assert_eq!(fs::read(target.join("input")).unwrap(), b"abc");
}

#[test]
fn private_output_and_allowlisted_projection() {
    let mut h = Harness::new();
    let body = json!({"status":"SUCCEEDED","value_sat":"700","payment_preimage":"private-preimage-canary"}).to_string();
    let path = h.start(fixture(&["emit", &body, "", "0", "0"]), json!({}), None);
    let receipt = Harness::receipt(&path);
    assert!(!receipt.to_string().contains("private-preimage-canary"));
    assert_eq!(receipt["stdout"], "");
    assert!(
        fs::read_to_string(path.join("stdout"))
            .unwrap()
            .contains("private-preimage-canary")
    );
    assert_eq!(
        fs::metadata(path.join("stdout"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let output = json!({"mode":"json_fields","fields":["status","value_sat"]});
    let path = h.start(
        fixture(&["emit", &body, "", "0", "0"]),
        json!({"output":output}),
        None,
    );
    let receipt = Harness::receipt(&path);
    assert_eq!(
        receipt["selected_output"],
        json!({"status":"SUCCEEDED","value_sat":"700"})
    );
    assert!(!receipt.to_string().contains("private-preimage-canary"));
    let path = h.start(
        json!(["printf", "PREIMAGE private-preimage-canary"]),
        json!({"output":output}),
        None,
    );
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["projection_succeeded"], false);
    assert!(!receipt.to_string().contains("private-preimage-canary"));
}

#[test]
fn invoice_relay_preserves_raw_privacy_and_failure() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../proofstorm-core/tests/fixtures/invoice-relay.json"
    ))
    .unwrap();
    let request = fixture["payment_request"].as_str().unwrap();
    let response = json!({"payment_request":request,"r_hash":fixture["r_hash"],"payment_preimage":"invoice-private-canary"}).to_string();
    let mut h = Harness::new();
    let path = h.start(
        crate::fixture(&["emit", &response, "stderr-private-canary", "0", "0"]),
        json!({"output":{"mode":"lnd_invoice"}}),
        None,
    );
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["projection_succeeded"], true);
    assert_eq!(receipt["selected_output"]["payment_request"], request);
    assert_eq!(
        receipt["selected_output"]["payment_hash"],
        fixture["r_hash"]
    );
    assert_eq!(receipt["selected_output"]["amount_msat"], 700_000);
    assert_eq!(receipt["stdout"], "");
    assert_eq!(receipt["stderr"], "");
    assert!(!receipt.to_string().contains("private-canary"));
    assert!(
        fs::read_to_string(path.join("stdout"))
            .unwrap()
            .contains("invoice-private-canary")
    );
    assert_eq!(
        fs::metadata(path.join("stdout"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for (body, mode, code, padding) in [
        (request.into(), "bolt11", 3, 0),
        ("private-canary".into(), "bolt11", 0, 0),
        (response.repeat(2), "lnd_invoice", 0, 0),
        (
            json!([request, fixture["r_hash"]]).to_string(),
            "lnd_invoice",
            0,
            0,
        ),
        (request.into(), "bolt11", 0, 20_000),
    ] {
        let path = h.start(
            crate::fixture(&[
                "emit",
                &body,
                "",
                &code.to_string(),
                "0",
                &padding.to_string(),
            ]),
            json!({"output":{"mode":mode}}),
            None,
        );
        let receipt = Harness::receipt(&path);
        assert_eq!(receipt["exit_code"], code);
        assert_eq!(receipt["projection_succeeded"], false);
        assert!(receipt.get("selected_output").is_none());
        assert_eq!(receipt["stdout"], "");
        assert_eq!(receipt["stderr"], "");
        assert!(!receipt.to_string().contains("private-canary"));
    }
    for cancel in [false, true] {
        let path = h.start(
            crate::fixture(&["emit", request, "", "0", "10000"]),
            json!({"timeout_seconds":1,"output":{"mode":"bolt11"}}),
            None,
        );
        if cancel {
            sleep(Duration::from_millis(100));
            Harness::cancel(&path);
        }
        let receipt = Harness::receipt(&path);
        assert_eq!(
            receipt[if cancel { "cancelled" } else { "timed_out" }],
            true
        );
        assert_eq!(receipt["exit_signal"], 15);
        assert_eq!(receipt["projection_succeeded"], false);
        assert!(receipt.get("selected_output").is_none());
    }
}

#[test]
fn timeout_reaps_descendants_that_escape_the_session() {
    let mut h = Harness::new();
    let pidfile = h.root.path().join("escaped-pid");
    let path = h.start(
        fixture(&["parent", pidfile.to_str().unwrap()]),
        json!({"timeout_seconds":1}),
        None,
    );
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["timed_out"], true);
    assert!(receipt["children_reaped"].as_u64().unwrap() >= 2);
    let pid = nix::unistd::Pid::from_raw(fs::read_to_string(pidfile).unwrap().parse().unwrap());
    assert_eq!(
        nix::sys::signal::kill(pid, None),
        Err(nix::errno::Errno::ESRCH)
    );
}

#[test]
fn cancellation_is_scoped_and_background_children_are_reaped() {
    let mut h = Harness::new();
    let one = h.start(json!(["sleep", "120"]), json!({"timeout_seconds":10}), None);
    let two = h.start(json!(["sleep", "120"]), json!({"timeout_seconds":10}), None);
    sleep(Duration::from_millis(150));
    Harness::cancel(&one);
    assert_eq!(Harness::receipt(&one)["cancelled"], true);
    assert_eq!(Harness::status(&two), json!({"running":true}));
    Harness::cancel(&two);
    assert_eq!(Harness::receipt(&two)["cancelled"], true);
    let path = h.start(json!(["sh", "-c", "exit 7"]), json!({}), None);
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["exit_code"], 7);
    assert_eq!(receipt["exit_scope"], "command");
    let path = h.start(json!([]), json!({"script":"false | true"}), None);
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["exit_code"], 0);
    assert_eq!(receipt["exit_scope"], "shell");
    let path = h.start(json!([]), json!({"script":"sleep 120 & exit 0"}), None);
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["exit_code"], 0);
    assert!(receipt["children_reaped"].as_u64().unwrap() >= 2);
}

#[test]
fn streams_are_drained_with_bounded_retention() {
    let mut h = Harness::new();
    let path = h.start(
        fixture(&["repeat", "x", "100000", "y", "100000"]),
        json!({}),
        None,
    );
    let receipt = Harness::receipt(&path);
    assert_eq!(receipt["output_truncated"], true);
    assert_eq!(receipt["streams_complete"], true);
    for stream in ["stdout", "stderr"] {
        assert_eq!(receipt["private_output"][stream]["bytes_observed"], 100_000);
        assert_eq!(receipt["private_output"][stream]["retained_bytes"], 16_384);
    }
}

#[test]
fn escaped_public_streams_fit_the_supervisor_status_transport() {
    let mut harness = Harness::new();
    let directory = harness.start(
        fixture(&["binary", "40000"]),
        json!({"output":{"mode":"public"}}),
        None,
    );
    let receipt = Harness::receipt(&directory);
    assert_eq!(receipt["exit_code"], 0);
    assert_eq!(receipt["cleanup_verified"], true);
    assert_eq!(receipt["streams_complete"], true);
    assert_eq!(receipt["output_truncated"], true);
    for stream in ["stdout", "stderr"] {
        assert_eq!(receipt["private_output"][stream]["bytes_observed"], 40000);
        assert_eq!(receipt["private_output"][stream]["retained_bytes"], 16384);
        let text = receipt[stream].as_str().unwrap();
        assert!(!text.is_empty() && text.bytes().all(|byte| byte == 0));
    }
    assert!(fs::metadata(directory.join("receipt.json")).unwrap().len() < 32 * 1024);
}
