//! Transfer an immutable workspace snapshot into a run without taking ownership of the task.
use super::Cells;
use crate::Error;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, ResourceExt,
    api::{AttachParams, ListParams},
};
use proofstorm_core::{
    Capability, CellInstance, ExperimentPhase, digest_json,
    workspace::{
        WORKSPACE_RUNNER, WorkspaceRequest,
        control::ControlCall,
        evidence::{
            CaptureRequest, CaptureSelection, MAX_CAPTURE_BYTES, TaskCapture, WorkspaceEvidence,
            WorkspaceEvidenceContent,
        },
    },
};
use proofstorm_kube::{CellAction, ProofstormCell, ProofstormCellAction, instance_namespace};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::io::AsyncReadExt;

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCaptureRequest {
    pub name: String,
    pub component: String,
    pub run_id: String,
    /// Reuse unchanged to retrieve the same capture. A new ID takes a new observation.
    pub request_id: String,
    pub selection: CaptureSelection,
}

impl WorkspaceCaptureRequest {
    #[must_use]
    pub fn capture_id(&self, workspace: &str, principal: &str) -> String {
        format!(
            "ws-evidence-{}",
            &digest_json(&(workspace, principal, &self.request_id))[7..47]
        )
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WorkspaceCaptureReceipt {
    pub capture_id: String,
    pub run_id: String,
    pub digest: String,
    pub byte_length: u32,
    pub task_id: String,
    pub task_phase: String,
    pub file_count: usize,
    pub controller_record_count: usize,
}

impl From<&WorkspaceEvidence> for WorkspaceCaptureReceipt {
    fn from(evidence: &WorkspaceEvidence) -> Self {
        let content = &evidence.content;
        Self {
            capture_id: content.capture_id.clone(),
            run_id: content.run_id.clone(),
            digest: evidence.digest.clone(),
            byte_length: evidence.byte_length,
            task_id: content.snapshot.selection.task_id.clone(),
            task_phase: content.snapshot.task["phase"]
                .as_str()
                .unwrap_or("unknown")
                .into(),
            file_count: content.snapshot.files.len(),
            controller_record_count: content.controller_actions.len(),
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::problem("workspace_capture_refused", message)
}

impl Cells {
    pub async fn workspace_capture(
        &self,
        request: &WorkspaceCaptureRequest,
    ) -> Result<WorkspaceCaptureReceipt, Error> {
        let client = self.runtime.client.clone();
        self.capture_with(request, move |pod, args| {
            let client = client.clone();
            async move { transfer(client, pod, args).await }
        })
        .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep admission, transfer, identity fences and durable attachment in one ordered flow"
    )]
    async fn capture_with<F, Fut>(
        &self,
        request: &WorkspaceCaptureRequest,
        mut download: F,
    ) -> Result<WorkspaceCaptureReceipt, Error>
    where
        F: FnMut(Pod, Vec<String>) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, Error>>,
    {
        self.authorize(&[
            Capability::ComponentExecLive,
            Capability::ArtifactRead,
            Capability::ExperimentRead,
        ])?;
        request.selection.validate().map_err(invalid)?;
        if request.request_id.is_empty() || request.request_id.len() > 256 {
            return Err(invalid("capture request_id must contain 1..256 bytes"));
        }
        let capture_id = request.capture_id(&self.workspace, &self.principal);
        let request_digest = digest_json(request);
        if let Some(previous) = self.store.workspace_capture(
            &self.workspace,
            &self.principal,
            &capture_id,
            &request_digest,
        )? {
            return Ok((&previous).into());
        }
        self.runtime_available()?;
        let _access = self
            .runtime_access()
            .map_err(|error| invalid(error.to_string()))?;
        let instance_id = self.store.resolve_cell_reference_for(
            &self.workspace,
            &self.principal,
            &request.name,
            Capability::ComponentExecLive,
        )?;
        let (instance, revision) = self.store.operation_context(
            &self.workspace,
            &self.principal,
            &instance_id,
            Capability::ComponentExecLive,
        )?;
        let run = self
            .store
            .experiment(&self.workspace, &self.principal, &request.run_id)?;
        if run.phase != ExperimentPhase::Active || run.instance_id != instance.id {
            return Err(invalid("select an open run belonging to this cell"));
        }
        if !revision.cell.components.iter().any(|component| {
            component.id == request.component && component.implementation == "workspace"
        }) {
            return Err(invalid("select a workspace component"));
        }
        let cells = Api::<ProofstormCell>::namespaced(
            self.runtime.client.clone(),
            &self.runtime.control_namespace,
        );
        let cell = cells.get(&instance.resource_name).await?;
        verify_cell(&cell, &instance)?;
        let pods = Api::<Pod>::namespaced(
            self.runtime.client.clone(),
            &instance_namespace(&instance.instance_key),
        );
        let mut candidates = pods
            .list(&ListParams::default().labels(&format!(
                "proofstorm.dev/instance={},proofstorm.dev/component={}",
                instance.instance_key, request.component
            )))
            .await?
            .items;
        candidates.retain(|pod| {
            pod.metadata.deletion_timestamp.is_none()
                && pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running")
        });
        let [pod] = candidates.as_slice() else {
            return Err(invalid("workspace must have exactly one running pod"));
        };
        let pod_uid = pod
            .uid()
            .ok_or_else(|| invalid("workspace pod identity missing"))?;
        let capture = CaptureRequest {
            capture_id: capture_id.clone(),
            request_digest: request_digest.clone(),
            selection: request.selection.clone(),
        };
        let args = vec![
            WORKSPACE_RUNNER.into(),
            "workspace".into(),
            "capture".into(),
            serde_json::to_string(&capture).map_err(|_| invalid("capture encoding failed"))?,
        ];
        let bytes = download(pod.clone(), args).await?;
        if bytes.len() > MAX_CAPTURE_BYTES {
            return Err(invalid("capture transfer exceeds byte limit"));
        }
        let snapshot: TaskCapture = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("invalid workspace capture response"))?;
        snapshot.validate().map_err(invalid)?;
        if snapshot.capture_id != capture_id
            || snapshot.selection != request.selection
            || snapshot.capture_request_digest != request_digest
        {
            return Err(invalid(
                "capture response does not match the requested selection",
            ));
        }
        let actions = self
            .capture_controller_actions(&instance, &request.component, &snapshot)
            .await?;
        let current = cells.get(&instance.resource_name).await?;
        verify_cell(&current, &instance)?;
        if current.uid() != cell.uid()
            || pods.get(&pod.name_any()).await?.uid().as_deref() != Some(&pod_uid)
        {
            return Err(invalid(
                "cell or workspace was replaced during capture; retry the same request",
            ));
        }
        let evidence = WorkspaceEvidence::new(WorkspaceEvidenceContent {
            capture_id: capture_id.clone(),
            run_id: request.run_id.clone(),
            principal_id: self.principal.clone(),
            instance_id: instance.id,
            instance_key: instance.instance_key,
            revision_digest: revision.digest,
            component: request.component.clone(),
            workspace_pod_uid: pod_uid,
            snapshot,
            controller_observed_at_unix: super::now(),
            controller_actions: actions,
        })
        .map_err(invalid)?;
        let recorded = self.store.record_workspace_capture(
            &self.workspace,
            &self.principal,
            &request_digest,
            &evidence,
        )?;
        // The local receipt is durable before the transfer copy is removed. A lost
        // acknowledgement retries from the store, never from changing live files.
        let release = serde_json::to_string(&WorkspaceRequest::ReleaseCapture { capture_id })
            .map_err(|_| invalid("capture release encoding failed"))?;
        let _ = download(
            pod.clone(),
            vec![
                WORKSPACE_RUNNER.into(),
                "workspace".into(),
                "request".into(),
                release,
            ],
        )
        .await;
        Ok((&recorded).into())
    }

    async fn capture_controller_actions(
        &self,
        instance: &CellInstance,
        component: &str,
        snapshot: &TaskCapture,
    ) -> Result<Vec<Value>, Error> {
        let Some(owner) = snapshot.task["control_owner"].as_str() else {
            return Ok(vec![]);
        };
        let api = Api::<ProofstormCellAction>::namespaced(
            self.runtime.client.clone(),
            &self.runtime.control_namespace,
        );
        let mut params = ListParams::default()
            .labels(&format!(
                "proofstorm.dev/instance={}",
                instance.instance_key
            ))
            .limit(200);
        let parent = loop {
            let page = api.list(&params).await?;
            if let Some(parent) = page
                .items
                .into_iter()
                .find(|action| action.spec.operation_id == owner)
            {
                break parent;
            }
            match page.metadata.continue_.filter(|token| !token.is_empty()) {
                Some(token) => params = params.continue_token(&token),
                None => {
                    return Err(invalid(
                        "task's controller owner is unavailable; capture before removing its records",
                    ));
                }
            }
        };
        let CellAction::ComponentExecLive(start) = &parent.spec.action else {
            return Err(invalid("task controller owner is not a start action"));
        };
        if !same_instance(&parent, instance)
            || start.component != component
            || proofstorm_core::workspace::control::start_request(&start.script, &start.argv)
                .as_ref()
                != Some(&snapshot.request)
        {
            return Err(invalid(
                "task controller owner does not match the captured task",
            ));
        }
        let label = digest_json(&(parent.metadata.uid.as_deref(), &parent.spec.operation_id))
            [7..47]
            .to_owned();
        params = ListParams::default()
            .labels(&format!("proofstorm.dev/workspace-owner={label}"))
            .limit(200);
        let mut children = BTreeMap::new();
        loop {
            let page = api.list(&params).await?;
            for child in page.items {
                if children.len() >= 4096 {
                    return Err(invalid("too many controller task records"));
                }
                children.insert(child.name_any(), child);
            }
            match page.metadata.continue_.filter(|token| !token.is_empty()) {
                Some(token) => params = params.continue_token(&token),
                None => break,
            }
        }
        controller_records(instance, snapshot, &parent, &label, &children)
    }
}

fn same_instance(action: &ProofstormCellAction, instance: &CellInstance) -> bool {
    action.spec.workspace_id == instance.workspace_id
        && action.spec.instance_id == instance.id
        && action.spec.instance_key == instance.instance_key
        && action.spec.cell_name == instance.resource_name
}

fn verify_cell(cell: &ProofstormCell, instance: &CellInstance) -> Result<(), Error> {
    proofstorm_kube::require_open_cell(cell).map_err(|error| invalid(error.to_string()))?;
    if cell.spec.instance_id != instance.id
        || cell.spec.instance_key != instance.instance_key
        || cell.spec.workspace_id != instance.workspace_id
        || cell.spec.revision_digest != instance.revision_digest
    {
        return Err(invalid(
            "cell incarnation or revision changed during capture",
        ));
    }
    Ok(())
}

fn controller_records(
    instance: &CellInstance,
    snapshot: &TaskCapture,
    parent: &ProofstormCellAction,
    label: &str,
    children: &BTreeMap<String, ProofstormCellAction>,
) -> Result<Vec<Value>, Error> {
    let mut records = vec![observation(parent)];
    for file in snapshot
        .files
        .iter()
        .filter(|file| file.path.starts_with("control/"))
    {
        let record: Value = serde_json::from_slice(&file.bytes().map_err(invalid)?)
            .map_err(|_| invalid("invalid task control record"))?;
        let call: ControlCall = serde_json::from_value(record["call"].clone())
            .map_err(|_| invalid("invalid captured task call"))?;
        snapshot
            .request
            .control
            .as_ref()
            .ok_or_else(|| invalid("task control grant missing"))?
            .permits(&call)
            .map_err(invalid)?;
        if record["digest"] != digest_json(&call) {
            return Err(invalid("task call digest mismatch"));
        }
        if record["claimed"] != true {
            continue;
        }
        let name = format!("ws-call-{}", &digest_json(&(label, &call.call_id))[7..47]);
        if let Some(child) = children.get(&name) {
            if !same_instance(child, instance)
                || child.spec.request_digest != digest_json(&call)
                || child.spec.principal_id != parent.spec.principal_id
                || child.annotations().get("proofstorm.dev/workspace-parent")
                    != Some(&parent.name_any())
            {
                return Err(invalid("controller child does not match the captured call"));
            }
            records.push(observation(child));
        } else {
            records.push(json!({"action_id":name,"call_id":call.call_id,"observation":"missing","outcome":"unknown"}));
        }
    }
    Ok(records)
}

fn observation(action: &ProofstormCellAction) -> Value {
    json!({"action_id":action.name_any(),"uid":action.uid(),"spec":action.spec,"status":action.status,"annotations":action.annotations()})
}

async fn transfer(client: kube::Client, pod: Pod, args: Vec<String>) -> Result<Vec<u8>, Error> {
    let run = async {
        let namespace = pod
            .namespace()
            .ok_or_else(|| invalid("workspace pod namespace missing"))?;
        let pods = Api::<Pod>::namespaced(client, &namespace);
        let mut process = pods
            .exec(
                &pod.name_any(),
                args,
                &AttachParams::default()
                    .container("component")
                    .stdout(true)
                    .stderr(true),
            )
            .await?;
        let stdout = process
            .stdout()
            .ok_or_else(|| invalid("capture stdout missing"))?;
        let stderr = process
            .stderr()
            .ok_or_else(|| invalid("capture stderr missing"))?;
        let status = process
            .take_status()
            .ok_or_else(|| invalid("capture status missing"))?;
        let (output, errors, status) = tokio::join!(
            read_stream(stdout, MAX_CAPTURE_BYTES),
            read_stream(stderr, 8192),
            status
        );
        let output = output?;
        let _ = errors?;
        process
            .join()
            .await
            .map_err(|_| invalid("capture transport interrupted"))?;
        if status.as_ref().and_then(|status| status.status.as_deref()) != Some("Success") {
            return Err(invalid(
                "workspace capture failed; check selection, file stability and size limits, then retry the same request",
            ));
        }
        Ok(output)
    };
    tokio::time::timeout(std::time::Duration::from_secs(45), run)
        .await
        .map_err(|_| invalid("capture timed out; retry the same request"))?
}

async fn read_stream(
    stream: impl tokio::io::AsyncRead + Unpin,
    maximum: usize,
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    stream
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| invalid("capture transport interrupted"))?;
    if bytes.len() > maximum {
        return Err(invalid("capture transport exceeds byte limit"));
    }
    Ok(bytes)
}
