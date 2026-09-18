//! Durable outbound mailbox. A claim is a no-replay fence, not proof of execution.
use super::{Result, exchange};
use proofstorm_core::workspace::control::{BridgeRequest, ControlCall};
use serde_json::{Value, json};

fn send(request: &BridgeRequest) -> Result<Value> {
    request.validate()?;
    let reply = exchange(&json!({"kind":"bridge","request":request}))?;
    if let Some(error) = reply.get("error") {
        return Err(format!("control request refused: {error}").into());
    }
    Ok(reply)
}

pub(super) fn call(encoded: &str) -> Result<()> {
    let call: ControlCall = serde_json::from_str(encoded)?;
    let task_id = std::env::var("PROOFSTORM_TASK_ID")?;
    let owner = std::env::var("PROOFSTORM_CONTROL_OWNER")?;
    let timeout = call
        .command
        .as_ref()
        .map_or(300, |command| command.timeout_seconds)
        .saturating_add(60);
    let native = call.command.is_some();
    let call_id = call.call_id.clone();
    send(&BridgeRequest::Submit {
        task_id: task_id.clone(),
        owner: owner.clone(),
        call,
    })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(u64::from(timeout));
    loop {
        let result = send(&BridgeRequest::Result {
            task_id: task_id.clone(),
            owner: owner.clone(),
            call_id: call_id.clone(),
        })?;
        if let Some(receipt) = result.get("receipt") {
            println!("{receipt}");
            if receipt["phase"] != "Succeeded" || (native && receipt["artifact"]["exit_code"] != 0)
            {
                return Err("control call did not complete successfully; inspect its receipt before retrying".into());
            }
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            println!(
                "{}",
                json!({"call_id":call_id,"phase":"unknown","message":"wait expired; retry this exact call ID to collect its outcome; a new ID could repeat effects"})
            );
            return Err("control call outcome not yet known".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[cfg(target_os = "linux")]
impl super::manager::Manager {
    #[allow(
        clippy::too_many_lines,
        reason = "keep durable mailbox transitions and their validation together"
    )]
    pub(super) fn bridge(&mut self, request: BridgeRequest) -> Result<Value> {
        use super::{read_json, write_json};
        request.validate()?;
        if let BridgeRequest::Start { start, owner } = &request {
            if start.control.is_none() {
                return Err("control scope missing".into());
            }
            return self.start(start, Some(owner));
        }
        let (task_id, owner) = match &request {
            BridgeRequest::Submit { task_id, owner, .. }
            | BridgeRequest::Poll { task_id, owner }
            | BridgeRequest::Close { task_id, owner }
            | BridgeRequest::Claim { task_id, owner, .. }
            | BridgeRequest::Complete { task_id, owner, .. }
            | BridgeRequest::Result { task_id, owner, .. }
            | BridgeRequest::Cleanup { task_id, owner, .. } => (task_id, owner),
            BridgeRequest::Start { .. } => unreachable!(),
        };
        if matches!(&request, BridgeRequest::Poll { .. })
            && !self.tasks.join(task_id).join("state.json").is_file()
        {
            return Ok(json!({"state":{"phase":"absent"},"pending":null}));
        }
        let state = self.status(task_id)?;
        if state["control_owner"].as_str() != Some(owner) {
            if matches!(&request, BridgeRequest::Poll { .. }) {
                return Ok(json!({"state":state,"pending":null}));
            }
            return Err("control owner mismatch".into());
        }
        let start: proofstorm_core::workspace::TaskStart =
            serde_json::from_value(read_json(&self.tasks.join(task_id).join("task.json"))?)?;
        let scope = start.control.ok_or("control scope missing")?;
        let active = state["phase"] == "running";
        let directory = self.tasks.join(task_id).join("control");
        std::fs::create_dir_all(&directory)?;
        let mut paths = std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
        paths.sort();
        match request {
            BridgeRequest::Cleanup { pending_faults, .. } => {
                let mut state = state;
                state["control_cleanup"] = json!({"pending_faults":pending_faults,"observed_at_unix":super::now(),"task_phase":state["phase"]});
                write_json(&self.tasks.join(&start.task_id).join("state.json"), &state)?;
                Ok(json!({"recorded":true}))
            }
            BridgeRequest::Submit { call, .. } => {
                scope.permits(&call)?;
                let path = directory.join(format!("{}.json", call.call_id));
                if path.exists() {
                    let previous = read_json(&path)?;
                    if previous["digest"] != proofstorm_core::digest_json(&call) {
                        return Err("call_id already belongs to a different request".into());
                    }
                    return Ok(previous);
                }
                if !active {
                    return Err("task is no longer running".into());
                }
                if paths.len() >= scope.max_calls as usize {
                    return Err("task control call limit reached".into());
                }
                let record = json!({"call":call,"digest":proofstorm_core::digest_json(&call),"claimed":false});
                write_json(&path, &record)?;
                Ok(record)
            }
            BridgeRequest::Poll { .. } => {
                let mut next = Value::Null;
                for path in &paths {
                    let record = read_json(path)?;
                    if record.get("receipt").is_none() {
                        let claimed = record["claimed"] == true;
                        if next.is_null() || claimed {
                            next = record;
                        }
                        if claimed {
                            break;
                        }
                    }
                }
                Ok(json!({"state":state,"pending":next,"call_count":paths.len()}))
            }
            BridgeRequest::Close { .. } => {
                super::atomic_write(
                    &self.tasks.join(&start.task_id).join("cancel"),
                    b"control authority closed",
                )?;
                let mut state = state;
                if active {
                    state["phase"] = json!("stopping");
                    write_json(&self.tasks.join(&start.task_id).join("state.json"), &state)?;
                }
                Ok(json!({"closed":true}))
            }
            BridgeRequest::Claim { call_id, .. } => {
                if !active {
                    return Err("task is no longer running".into());
                }
                let path = directory.join(format!("{call_id}.json"));
                let mut record = read_json(&path)?;
                let fresh = record["claimed"] == false && record.get("receipt").is_none();
                record["claimed"] = json!(true);
                write_json(&path, &record)?;
                if fresh && record["call"]["operation"]["kind"] == "network_partition" {
                    // Persist cleanup responsibility before the controller may create a fault.
                    let mut state = state;
                    let count = state["control_cleanup"]["pending_faults"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_add(1);
                    state["control_cleanup"] = json!({"pending_faults":count,"observed_at_unix":super::now(),"task_phase":state["phase"]});
                    write_json(&self.tasks.join(&start.task_id).join("state.json"), &state)?;
                }
                Ok(json!({"fresh":fresh,"digest":record["digest"]}))
            }
            BridgeRequest::Complete {
                call_id, receipt, ..
            } => {
                let path = directory.join(format!("{call_id}.json"));
                let mut record = read_json(&path)?;
                if record.get("receipt").is_none() {
                    record["receipt"] = receipt;
                    write_json(&path, &record)?;
                }
                // Also expose the immutable result through ordinary workspace_file reads.
                let output = self
                    .root
                    .join("output")
                    .join(&start.task_id)
                    .join("control");
                std::fs::create_dir_all(&output)?;
                write_json(&output.join(format!("{call_id}.json")), &record)?;
                Ok(json!({"recorded":true}))
            }
            BridgeRequest::Result { call_id, .. } => {
                read_json(&directory.join(format!("{call_id}.json")))
            }
            BridgeRequest::Start { .. } => unreachable!(),
        }
    }
}
