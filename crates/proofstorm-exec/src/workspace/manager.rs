use super::{
    Result, atomic_write, bounded_read, file_request, local_path, now, read_json, snapshot,
    socket_path, write_json,
};
use nix::{
    fcntl::{Flock, FlockArg},
    sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask},
};
use proofstorm_core::workspace::{
    MAX_ACTIVE_TASKS, MAX_TASKS, TaskRequest, TaskStart, WorkspaceRequest, wire,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) struct Manager {
    pub(super) root: PathBuf,
    pub(super) tasks: PathBuf,
    children: BTreeMap<String, Child>,
}

pub(super) fn serve(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join(".proofstorm/tasks"))?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("data"))?;
    fs::create_dir_all(root.join("output"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".proofstorm/lock"))?;
    let _lock = Flock::lock(lock, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| error)?;
    let mut signals = SigSet::empty();
    signals.add(Signal::SIGTERM);
    signals.add(Signal::SIGINT);
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), None)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    thread::spawn(move || {
        if signals.wait().is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
    });
    let mut manager = Manager {
        root: root.to_path_buf(),
        tasks: root.join(".proofstorm/tasks"),
        children: BTreeMap::new(),
    };
    manager.recover()?;
    let socket = socket_path();
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    while !stopped.load(Ordering::SeqCst) {
        manager.reap()?;
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut bridge = false;
                let reply = (|| -> Result<Value> {
                    let bytes = bounded_read(
                        &mut stream,
                        proofstorm_core::workspace::control::MAX_BRIDGE_BYTES,
                    )?;
                    let value: Value = serde_json::from_slice(&bytes)?;
                    if value["kind"] == "bridge" {
                        bridge = true;
                        let request = serde_json::from_value(value["request"].clone())?;
                        return manager.bridge(request);
                    }
                    if bytes.len() > wire::MAX_MESSAGE_BYTES {
                        return Err("workspace request too large".into());
                    }
                    let request = wire::decode(value)?;
                    manager.request(request)
                })()
                .unwrap_or_else(|error| json!({"error":error.to_string()}));
                let mut bytes = serde_json::to_vec(&reply)?;
                if bytes.len() as u64 > proofstorm_core::workspace::control::MAX_BRIDGE_BYTES
                    || (!bridge
                        && serde_json::to_vec(&String::from_utf8_lossy(&bytes))?.len() > 11000)
                {
                    bytes =
                        br#"{"error":"workspace response exceeds control output budget"}"#.to_vec();
                }
                // A lost client does not cancel the accepted task or stop the manager.
                let _ = stream.write_all(&bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    }
    manager.shutdown()?;
    fs::remove_file(socket)?;
    Ok(())
}

impl Manager {
    fn ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.tasks)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "invalid task directory")?;
            if !name.starts_with('.') {
                ids.push(name);
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn recover(&self) -> Result<()> {
        for id in self.ids()? {
            let directory = self.tasks.join(id);
            let mut state = read_json(&directory.join("state.json"))?;
            if matches!(
                state["phase"].as_str(),
                Some("running" | "starting" | "stopping")
            ) {
                if directory.join("receipt.json").is_file() {
                    update_receipt(&directory, &mut state)?;
                } else {
                    state["phase"] = json!("interrupted");
                    state["reason"] = json!(
                        "workspace_restarted; task was not replayed; reconcile application effects before starting a new task"
                    );
                    state["completed_at_unix"] = json!(now());
                    state["cleanup_verified"] = json!(false);
                    write_json(&directory.join("state.json"), &state)?;
                }
            }
        }
        Ok(())
    }

    fn request(&mut self, request: WorkspaceRequest) -> Result<Value> {
        match request {
            WorkspaceRequest::Capture(request) => {
                let state = self.status(&request.selection.task_id)?;
                super::capture::freeze(&self.root, &request, state)
            }
            WorkspaceRequest::ReleaseCapture { capture_id } => {
                super::capture::release(&self.root, &capture_id)
            }
            WorkspaceRequest::Ping => Ok(json!({"ready":true,"version":"proofstorm-workspace/v1"})),
            WorkspaceRequest::File(request) => file_request(&self.root, &request),
            WorkspaceRequest::Upload(request) => super::upload::commit(&self.root, &request),
            WorkspaceRequest::Task(TaskRequest::Start(start)) => {
                if start.control.is_some() {
                    return Err("controlled tasks must be started through the controller".into());
                }
                self.start(&start, None)
            }
            WorkspaceRequest::Task(TaskRequest::Status { task_id }) => self.status(&task_id),
            WorkspaceRequest::Task(TaskRequest::Stop { task_id }) => {
                let mut state = self.status(&task_id)?;
                if matches!(
                    state["phase"].as_str(),
                    Some("starting" | "running" | "stopping")
                ) {
                    atomic_write(&self.tasks.join(&task_id).join("cancel"), b"cancel")?;
                    state["phase"] = json!("stopping");
                    write_json(&self.tasks.join(&task_id).join("state.json"), &state)?;
                }
                Ok(state)
            }
            WorkspaceRequest::Task(TaskRequest::List { after }) => {
                let mut ids = self.ids()?;
                ids.retain(|id| after.as_ref().is_none_or(|after| id > after));
                let next = (ids.len() > 8).then(|| ids[7].clone());
                let tasks = ids
                    .iter()
                    .take(8)
                    .map(|id| self.status(id))
                    .collect::<Result<Vec<_>>>()?;
                Ok(json!({"tasks":tasks,"next_after":next}))
            }
            WorkspaceRequest::Task(TaskRequest::Logs { task_id, stream }) => {
                let state = self.status(&task_id)?;
                let mut tail = Vec::new();
                for suffix in [".previous", ""] {
                    let path = self
                        .tasks
                        .join(&task_id)
                        .join(format!("{}.log{suffix}", stream.name()));
                    match fs::File::open(path) {
                        Ok(mut file) => {
                            let length = file.metadata()?.len();
                            file.seek(SeekFrom::Start(length.saturating_sub(1024)))?;
                            file.take(1024).read_to_end(&mut tail)?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => return Err(error.into()),
                    }
                }
                let start = tail.len().saturating_sub(1024);
                Ok(
                    json!({"task_id":task_id,"phase":state["phase"],"stream":stream,"tail":String::from_utf8_lossy(&tail[start..]),"retention":"two rotating 256 KiB segments per stream; response is last 1024 bytes"}),
                )
            }
        }
    }

    pub(super) fn start(&mut self, start: &TaskStart, owner: Option<&str>) -> Result<Value> {
        start.validate()?;
        let request_digest = proofstorm_core::digest_json(start);
        let directory = self.tasks.join(&start.task_id);
        if directory.exists() {
            let state = self.status(&start.task_id)?;
            if state["request_digest"] != request_digest {
                return Err("task_id already belongs to a different request".into());
            }
            return Ok(state);
        }
        if self.ids()?.len() >= MAX_TASKS {
            return Err("workspace task history limit reached (128)".into());
        }
        if self.children.len() >= MAX_ACTIVE_TASKS {
            return Err("workspace concurrent task limit reached (16)".into());
        }
        let source = local_path(&self.root, &start.source, false)?;
        if !source.is_dir() {
            return Err("task source must be a directory".into());
        }
        let staging = self.tasks.join(format!(
            ".stage-{}-{}",
            start.task_id,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&staging)?;
        let prepared = (|| -> Result<Value> {
            let source_digest = snapshot(&source, &staging.join("source"))?;
            // The execution directory may gain caches or generated files. Keep
            // submitted code separately so later evidence still captures its inputs.
            snapshot(&staging.join("source"), &staging.join("inputs"))?;
            write_json(&staging.join("task.json"), &serde_json::to_value(start)?)?;
            let state = json!({"task_id":start.task_id,"request_digest":request_digest,"source_digest":source_digest,"runtime_image":std::env::var("PROOFSTORM_RUNTIME_IMAGE").unwrap_or_default(),"phase":"starting","created_at_unix":now(),"output_path":format!("output/{}",start.task_id)});
            let mut state = state;
            if let Some(owner) = owner {
                state["control_owner"] = json!(owner);
            }
            write_json(&staging.join("state.json"), &state)?;
            fs::create_dir_all(self.root.join("output").join(&start.task_id))?;
            fs::rename(&staging, &directory)?;
            fs::File::open(&self.tasks)?.sync_all()?;
            Ok(state)
        })();
        let mut state = match prepared {
            Ok(state) => state,
            Err(error) => {
                let _ = fs::remove_dir_all(staging);
                return Err(error);
            }
        };
        // Persist the claim before launching. Recovery never replays a starting/running task.
        let launched = Command::new(std::env::current_exe()?)
            .args(["workspace", "run"])
            .arg(&directory)
            .env("PROOFSTORM_WORKSPACE", &self.root)
            .env("PROOFSTORM_TASK_ID", &start.task_id)
            .env("PROOFSTORM_CONTROL_OWNER", owner.unwrap_or_default())
            .env("PROOFSTORM_CONTROL", std::env::current_exe()?)
            .env(
                "PROOFSTORM_OUTPUT",
                self.root.join("output").join(&start.task_id),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(child) = launched {
            self.children.insert(start.task_id.clone(), child);
            state["phase"] = json!("running");
            state["started_at_unix"] = json!(now());
        } else {
            state["phase"] = json!("failed");
            state["reason"] = json!("supervisor_start_failed");
            state["completed_at_unix"] = json!(now());
        }
        write_json(&directory.join("state.json"), &state)?;
        Ok(state)
    }

    pub(super) fn status(&self, id: &str) -> Result<Value> {
        let directory = self.tasks.join(id);
        let mut state = read_json(&directory.join("state.json"))?;
        if directory.join("receipt.json").is_file() {
            update_receipt(&directory, &mut state)?;
        }
        Ok(state)
    }

    fn reap(&mut self) -> Result<()> {
        let mut finished = Vec::new();
        for (id, child) in &mut self.children {
            if child.try_wait()?.is_some() {
                finished.push(id.clone());
            }
        }
        for id in finished {
            self.children.remove(&id);
            let directory = self.tasks.join(&id);
            let mut state = self.status(&id)?;
            if !directory.join("receipt.json").is_file() {
                state["phase"] = json!("interrupted");
                state["reason"] = json!(
                    "supervisor_exited_without_receipt; cleanup and application outcome are unknown"
                );
                state["cleanup_verified"] = json!(false);
                state["completed_at_unix"] = json!(now());
                write_json(&directory.join("state.json"), &state)?;
            }
        }
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        for id in self.children.keys() {
            atomic_write(&self.tasks.join(id).join("cancel"), b"shutdown")?;
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while !self.children.is_empty() && Instant::now() < deadline {
            self.reap()?;
            thread::sleep(Duration::from_millis(25));
        }
        // The container runtime ends the remaining namespace on process exit. Do not invent cleanup evidence.
        Ok(())
    }
}

fn update_receipt(directory: &Path, state: &mut Value) -> Result<()> {
    let receipt = read_json(&directory.join("receipt.json"))?;
    let clean = receipt["cleanup_verified"] == true;
    let phase = if !clean {
        "interrupted"
    } else if receipt["cancelled"] == true {
        "cancelled"
    } else if receipt["timed_out"] == true {
        "timed_out"
    } else if receipt["exit_code"] == 0 {
        "succeeded"
    } else {
        "failed"
    };
    if state["phase"] != phase {
        state["phase"] = json!(phase);
        state["completed_at_unix"] = json!(now());
        for field in [
            "exit_code",
            "exit_signal",
            "cleanup_verified",
            "timed_out",
            "cancelled",
        ] {
            state[field] = receipt[field].clone();
        }
        write_json(&directory.join("state.json"), state)?;
    }
    Ok(())
}
