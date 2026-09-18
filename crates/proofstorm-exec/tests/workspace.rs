//! Real Linux workspace tasks, independent of Kubernetes or application funds.
#![cfg(all(target_os = "linux", feature = "contract-tests"))]
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

struct Workspace {
    directory: tempfile::TempDir,
    manager: Child,
    socket: PathBuf,
}

#[test]
fn capture_helper_preserves_running_tasks_and_retries_the_frozen_transfer_after_restart() {
    use proofstorm_core::workspace::evidence::TaskCapture;
    let mut workspace = Workspace::new();
    fs::write(workspace.directory.path().join("src/run.sh"), "sleep 60").unwrap();
    workspace.task(json!({"action":"start","task_id":"capture","argv":["sh","run.sh"]}));
    fs::write(
        workspace.directory.path().join("output/capture/result.bin"),
        [0, 255, 3],
    )
    .unwrap();
    fs::write(
        workspace
            .directory
            .path()
            .join(".proofstorm/tasks/capture/source/generated.cache"),
        "runtime output",
    )
    .unwrap();
    let request = json!({"capture_id":"frozen","request_digest":proofstorm_core::digest_json(&"capture"),"selection":{"task_id":"capture","output_paths":["result.bin"]}});
    let download = |workspace: &Workspace| {
        let result = Command::new(env!("CARGO_BIN_EXE_proofstorm-exec"))
            .args(["workspace", "capture", &request.to_string()])
            .env("PROOFSTORM_WORKSPACE_ROOT", workspace.directory.path())
            .env("PROOFSTORM_WORKSPACE_SOCKET", &workspace.socket)
            .output()
            .unwrap();
        assert!(result.status.success(), "{:?}", result.stderr);
        result.stdout
    };
    let first = download(&workspace);
    let capture: TaskCapture = serde_json::from_slice(&first).unwrap();
    capture.validate().unwrap();
    assert!(
        !capture
            .files
            .iter()
            .any(|file| file.path.ends_with("generated.cache"))
    );
    assert_eq!(capture.task["phase"], "running");
    assert_eq!(
        workspace.task(json!({"action":"status","task_id":"capture"}))["phase"],
        "running"
    );
    fs::write(
        workspace.directory.path().join("output/capture/result.bin"),
        "new result",
    )
    .unwrap();
    workspace.stop_manager();
    workspace.manager = Workspace::spawn(&workspace.directory, &workspace.socket);
    workspace.ready();
    assert_eq!(download(&workspace), first);
    let release =
        workspace.request(json!({"kind":"release_capture","request":{"capture_id":"frozen"}}));
    assert_eq!(release["released"], true);
    assert!(
        !workspace
            .directory
            .path()
            .join(".proofstorm/captures/frozen.json")
            .exists()
    );
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "protocol fixtures pass inline JSON values to keep control requests readable"
)]
impl Workspace {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let manager = Self::spawn(&directory, &socket);
        let workspace = Self {
            directory,
            manager,
            socket,
        };
        workspace.ready();
        workspace
    }

    fn spawn(directory: &tempfile::TempDir, socket: &PathBuf) -> Child {
        Command::new(env!("CARGO_BIN_EXE_proofstorm-exec"))
            .args(["workspace", "serve"])
            .env("PROOFSTORM_WORKSPACE_ROOT", directory.path())
            .env("PROOFSTORM_WORKSPACE_SOCKET", socket)
            .env("PROOFSTORM_RUNTIME_IMAGE", "test@sha256:runtime")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }

    fn ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.socket.exists() {
            assert!(Instant::now() < deadline, "workspace never became ready");
            sleep(Duration::from_millis(10));
        }
        assert_eq!(self.request(json!({"kind":"ping"}))["ready"], true);
    }

    fn request(&self, request: Value) -> Value {
        let mut socket = UnixStream::connect(&self.socket).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        socket.read_to_end(&mut reply).unwrap();
        serde_json::from_slice(&reply).unwrap()
    }

    fn task(&self, request: Value) -> Value {
        self.request(json!({"kind":"task","request":request}))
    }

    fn bridge(&self, request: Value) -> Value {
        self.request(json!({"kind":"bridge","request":request}))
    }

    fn wait(&self, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let status = self.task(json!({"action":"status","task_id":id}));
            if !matches!(
                status["phase"].as_str(),
                Some("running" | "starting" | "stopping")
            ) {
                return status;
            }
            assert!(Instant::now() < deadline, "task did not finish: {status}");
            sleep(Duration::from_millis(25));
        }
    }

    fn stop_manager(&mut self) {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(self.manager.id()).unwrap()),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        assert!(self.manager.wait().unwrap().success());
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.manager.try_wait().ok().flatten().is_none() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(self.manager.id()).unwrap()),
                nix::sys::signal::Signal::SIGTERM,
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if self.manager.try_wait().ok().flatten().is_some() {
                    return;
                }
                sleep(Duration::from_millis(25));
            }
            let _ = self.manager.kill();
            let _ = self.manager.wait();
        }
    }
}

#[test]
fn tasks_survive_control_disconnect_freeze_sources_and_deduplicate_starts() {
    let workspace = Workspace::new();
    fs::write(workspace.directory.path().join("src/value"), "original").unwrap();
    let start = json!({"action":"start","task_id":"miner","script":"sleep 0.3; cat value; while :; do echo tick; sleep 0.1; done"});
    let first = workspace.task(start.clone());
    assert_eq!(first["phase"], "running");
    fs::write(workspace.directory.path().join("src/value"), "changed").unwrap();
    assert_eq!(
        workspace.task(start.clone())["source_digest"],
        first["source_digest"]
    );
    let mut changed = start;
    changed["script"] = json!("echo duplicate");
    assert!(workspace.task(changed).get("error").is_some());
    sleep(Duration::from_millis(500));
    let logs = workspace.task(json!({"action":"logs","task_id":"miner"}));
    assert!(logs["tail"].as_str().unwrap().contains("original"));
    assert!(!logs["tail"].as_str().unwrap().contains("changed"));
    workspace.task(json!({"action":"stop","task_id":"miner"}));
    let result = workspace.wait("miner");
    assert_eq!(result["phase"], "cancelled");
    assert_eq!(result["cleanup_verified"], true);
    assert_eq!(
        workspace.task(json!({"action":"list"}))["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn bounded_native_control_cleanup_does_not_terminate_the_workspace_task() {
    let workspace = Workspace::new();
    let directory = workspace.directory.path().join("native-control");
    fs::create_dir(&directory).unwrap();
    let request = json!({"kind":"task","request":{"action":"start","task_id":"independent","script":"sleep 600"}});
    let command = json!({"argv":[env!("CARGO_BIN_EXE_proofstorm-exec"),"workspace","request",request.to_string()],"timeout_seconds":2,"output":{"mode":"public"}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_proofstorm-exec"))
        .arg("start")
        .arg(&directory)
        .env("PROOFSTORM_WORKSPACE_SOCKET", &workspace.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&command).unwrap())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !directory.join("receipt.json").exists() {
        assert!(Instant::now() < deadline, "control command did not finish");
        sleep(Duration::from_millis(10));
    }
    let receipt: Value =
        serde_json::from_slice(&fs::read(directory.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["exit_code"], 0);
    assert_eq!(receipt["cleanup_verified"], true);
    assert_eq!(
        workspace.task(json!({"action":"status","task_id":"independent"}))["phase"],
        "running"
    );
    workspace.task(json!({"action":"stop","task_id":"independent"}));
    assert_eq!(workspace.wait("independent")["cleanup_verified"], true);
}

#[test]
fn multiple_tasks_have_independent_deadlines_exit_codes_and_rotating_logs() {
    let workspace = Workspace::new();
    workspace.task(
        json!({"action":"start","task_id":"deadline","script":"sleep 60", "timeout_seconds":1}),
    );
    workspace
        .task(json!({"action":"start","task_id":"failed","script":"echo failure >&2; exit 7"}));
    workspace.task(
        json!({"action":"start","task_id":"logs","script":"head -c 900000 /dev/zero; echo latest"}),
    );
    assert_eq!(workspace.wait("deadline")["phase"], "timed_out");
    assert_eq!(workspace.wait("failed")["exit_code"], 7);
    assert_eq!(workspace.wait("logs")["phase"], "succeeded");
    let logs = workspace.task(json!({"action":"logs","task_id":"logs"}));
    assert!(logs["tail"].as_str().unwrap().ends_with("latest\n"));
    for name in ["stdout.log", "stdout.log.previous"] {
        assert!(
            fs::metadata(
                workspace
                    .directory
                    .path()
                    .join(".proofstorm/tasks/logs")
                    .join(name)
            )
            .unwrap()
            .len()
                <= 256 * 1024
        );
    }
    let failed = workspace.task(json!({"action":"logs","task_id":"failed","stream":"stderr"}));
    assert!(failed["tail"].as_str().unwrap().contains("failure"));
}

#[test]
fn restart_preserves_files_and_never_replays_uncertain_or_completed_tasks() {
    let mut workspace = Workspace::new();
    workspace.task(json!({"action":"start","task_id":"once","script":"echo once >> \"$PROOFSTORM_WORKSPACE/data/effects\""}));
    assert_eq!(workspace.wait("once")["phase"], "succeeded");
    workspace.stop_manager();
    let pending = workspace
        .directory
        .path()
        .join(".proofstorm/tasks/uncertain");
    fs::create_dir(&pending).unwrap();
    fs::write(
        pending.join("state.json"),
        serde_json::to_vec(
            &json!({"task_id":"uncertain","phase":"starting","request_digest":"original"}),
        )
        .unwrap(),
    )
    .unwrap();
    workspace.manager = Workspace::spawn(&workspace.directory, &workspace.socket);
    workspace.ready();
    assert_eq!(
        workspace.task(json!({"action":"status","task_id":"uncertain"}))["phase"],
        "interrupted"
    );
    assert_eq!(
        workspace.task(json!({"action":"status","task_id":"once"}))["phase"],
        "succeeded"
    );
    assert_eq!(
        fs::read_to_string(workspace.directory.path().join("data/effects")).unwrap(),
        "once\n"
    );
}

#[test]
fn shutdown_cleans_background_descendants_and_persists_terminal_state() {
    let mut workspace = Workspace::new();
    workspace.task(json!({"action":"start","task_id":"children","script":"sleep 60 & echo $! > \"$PROOFSTORM_OUTPUT/pid\"; wait"}));
    let pid_file = workspace.directory.path().join("output/children/pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pid_file.exists() {
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(&pid_file).unwrap();
    workspace.stop_manager();
    let state: Value = serde_json::from_slice(
        &fs::read(
            workspace
                .directory
                .path()
                .join(".proofstorm/tasks/children/state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["phase"], "cancelled");
    assert_eq!(state["cleanup_verified"], true);
    assert!(!PathBuf::from(format!("/proc/{}", pid.trim())).exists());
}

#[test]
fn control_calls_are_scoped_durable_and_never_reclaimed_after_restart() {
    let mut workspace = Workspace::new();
    let start = json!({"task_id":"flow","script":"sleep 60","control":{"components":["chain"],"max_calls":2,"max_timeout_seconds":10}});
    let mut plain = start.clone();
    plain["action"] = json!("start");
    assert!(workspace.task(plain)["error"].is_string());
    let bound = json!({"action":"start","owner":"original","start":start});
    assert_eq!(workspace.bridge(bound.clone())["control_owner"], "original");
    assert_eq!(
        workspace.bridge(json!({"action":"start","owner":"duplicate","start":start}))["control_owner"],
        "original"
    );
    let call = json!({"call_id":"step-1","component":"chain","command":{"script":"true","timeout_seconds":10}});
    let submit = json!({"action":"submit","task_id":"flow","owner":"original","call":call});
    let first = workspace.bridge(submit.clone());
    assert_eq!(first["claimed"], false);
    assert_eq!(workspace.bridge(submit.clone()), first);
    for (field, value) in [("script", json!("false")), ("timeout_seconds", json!(11))] {
        let mut changed = submit.clone();
        changed["call"]["command"][field] = value;
        assert!(workspace.bridge(changed)["error"].is_string());
    }
    let mut outside = submit.clone();
    outside["call"]["component"] = json!("wallet");
    assert!(workspace.bridge(outside)["error"].is_string());
    let claim = json!({"action":"claim","task_id":"flow","owner":"original","call_id":"step-1"});
    assert_eq!(workspace.bridge(claim.clone())["fresh"], true);
    assert_eq!(workspace.bridge(claim.clone())["fresh"], false);
    workspace.stop_manager();
    workspace.manager = Workspace::spawn(&workspace.directory, &workspace.socket);
    workspace.ready();
    assert_eq!(
        workspace.bridge(json!({"action":"poll","task_id":"flow","owner":"original"}))["pending"]["claimed"],
        true
    );
    assert!(workspace.bridge(claim)["error"].is_string());
    assert_eq!(workspace.bridge(submit)["claimed"], true);
    assert_ne!(workspace.bridge(bound)["phase"], "running");
    let completion = json!({"action":"complete","task_id":"flow","owner":"original","call_id":"step-1","receipt":{"phase":"Cancelled","artifact":{"cleanup_verified":true}}});
    assert_eq!(workspace.bridge(completion.clone())["recorded"], true);
    let mut overwrite = completion;
    overwrite["receipt"]["phase"] = json!("Succeeded");
    workspace.bridge(overwrite);
    let receipt = workspace
        .bridge(json!({"action":"result","task_id":"flow","owner":"original","call_id":"step-1"}));
    assert_eq!(receipt["receipt"]["phase"], "Cancelled");
    let persisted: Value = serde_json::from_slice(
        &fs::read(
            workspace
                .directory
                .path()
                .join("output/flow/control/step-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted, receipt);
}

#[test]
fn scripts_use_the_control_helper_and_stop_cleans_up_waiting_call_processes() {
    for cancel in [false, true] {
        let workspace = Workspace::new();
        let call = json!({"call_id":"height","component":"chain","command":{"script":"true","timeout_seconds":10,"output":{"mode":"public"}}});
        let start = json!({"task_id":"flow","script":"\"$PROOFSTORM_CONTROL\" workspace call \"$CALL\" > \"$PROOFSTORM_OUTPUT/result.json\"","env":{"CALL":call.to_string()},"control":{"components":["chain"],"max_calls":1}});
        assert_eq!(
            workspace.bridge(json!({"action":"start","owner":"original","start":start}))["phase"],
            "running"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let poll =
                workspace.bridge(json!({"action":"poll","owner":"original","task_id":"flow"}));
            if !poll["pending"].is_null() {
                assert_eq!(poll["pending"]["call"]["call_id"], "height");
                break;
            }
            assert!(Instant::now() < deadline, "helper did not submit");
            sleep(Duration::from_millis(25));
        }
        let extra = json!({"action":"submit","task_id":"flow","owner":"original","call":{"call_id":"overflow","component":"chain","command":{"script":"true","timeout_seconds":10}}});
        assert!(workspace.bridge(extra)["error"].is_string());
        if cancel {
            workspace.bridge(json!({"action":"close","owner":"original","task_id":"flow"}));
            let status = workspace.wait("flow");
            assert_eq!(status["phase"], "cancelled");
            assert_eq!(status["cleanup_verified"], true);
        } else {
            workspace.bridge(json!({"action":"complete","owner":"original","task_id":"flow","call_id":"height","receipt":{"phase":"Succeeded","artifact":{"exit_code":0,"stdout":"101"}}}));
            assert_eq!(workspace.wait("flow")["phase"], "succeeded");
            let receipt: Value = serde_json::from_slice(
                &fs::read(workspace.directory.path().join("output/flow/result.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(receipt["artifact"]["stdout"], "101");
        }
    }
}

#[test]
fn typed_controls_accept_non_native_receipts_and_keep_cleanup_observations_after_exit() {
    let workspace = Workspace::new();
    let call =
        json!({"call_id":"restart","operation":{"kind":"component_restart","component":"chain"}});
    let start = json!({"task_id":"typed","script":"\"$PROOFSTORM_CONTROL\" workspace call \"$CALL\" > \"$PROOFSTORM_OUTPUT/result.json\"","env":{"CALL":call.to_string()},"control":{"lifecycle":["chain"]}});
    assert_eq!(
        workspace.bridge(json!({"action":"start","owner":"original","start":start}))["phase"],
        "running"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let poll = workspace.bridge(json!({"action":"poll","task_id":"typed","owner":"original"}));
        if !poll["pending"].is_null() {
            assert_eq!(poll["pending"]["call"], call);
            break;
        }
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(25));
    }
    workspace.bridge(json!({"action":"complete","task_id":"typed","owner":"original","call_id":"restart","receipt":{"phase":"Succeeded","artifact":{"state":"running","restarted":true}}}));
    assert_eq!(workspace.wait("typed")["phase"], "succeeded");
    workspace.bridge(
        json!({"action":"cleanup","task_id":"typed","owner":"original","pending_faults":1}),
    );
    assert_eq!(
        workspace.task(json!({"action":"status","task_id":"typed"}))["control_cleanup"]["pending_faults"],
        1
    );
    workspace.bridge(
        json!({"action":"cleanup","task_id":"typed","owner":"original","pending_faults":0}),
    );
    let status = workspace.task(json!({"action":"status","task_id":"typed"}));
    assert_eq!(status["control_cleanup"]["pending_faults"], 0);
    assert!(
        status["control_cleanup"]["observed_at_unix"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn partition_claim_records_cleanup_responsibility_before_controller_dispatch() {
    let workspace = Workspace::new();
    workspace.bridge(json!({"action":"start","owner":"original","start":{"task_id":"fault","script":"sleep 60","control":{"network":[{"from_component":"chain","to_component":"scripts"}]}}}));
    workspace.bridge(json!({"action":"submit","owner":"original","task_id":"fault","call":{"call_id":"outage","operation":{"kind":"network_partition","from_component":"chain","to_component":"scripts","duration_seconds":30}}}));
    let claim = json!({"action":"claim","owner":"original","task_id":"fault","call_id":"outage"});
    assert_eq!(workspace.bridge(claim.clone())["fresh"], true);
    assert_eq!(workspace.bridge(claim)["fresh"], false);
    workspace.task(json!({"action":"stop","task_id":"fault"}));
    let status = workspace.wait("fault");
    assert_eq!(status["control_cleanup"]["pending_faults"], 1);
    assert_eq!(status["control_cleanup"]["task_phase"], "running");
    workspace.bridge(
        json!({"action":"cleanup","owner":"original","task_id":"fault","pending_faults":0}),
    );
    let status = workspace.task(json!({"action":"status","task_id":"fault"}));
    assert_eq!(status["control_cleanup"]["pending_faults"], 0);
    assert_eq!(status["control_cleanup"]["task_phase"], "cancelled");
}
