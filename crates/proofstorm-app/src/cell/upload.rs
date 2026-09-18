//! Local file upload: only metadata enters MCP, actions and the activity journal.
use super::Cells;
use crate::Error;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, ResourceExt,
    api::{AttachParams, ListParams},
};
use proofstorm_core::{
    Capability, CellInstance, CellOperation, OperationKind, OperationPhase,
    workspace::{
        WORKSPACE_RUNNER, WorkspaceRequest,
        upload::{MAX_UPLOAD_BYTES, STAGING_CAPACITY_ERROR, UploadRequest},
    },
};
use proofstorm_kube::{ProofstormCell, instance_namespace};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceUploadRequest {
    pub name: String,
    pub component: String,
    /// Local regular file readable by the MCP server. Relative paths use its working directory.
    pub source_path: String,
    /// Destination relative to /workspace, usually src/script.py.
    pub path: String,
    /// Reuse only for the same destination, file bytes and executable permission.
    pub request_id: String,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::problem("workspace_upload_refused", message)
}

fn read_source(path: &Path) -> Result<(Vec<u8>, bool), Error> {
    // Nonblocking open prevents special files such as FIFOs from hanging a tool call.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid("source file is not readable by the MCP server"))?;
    let metadata = file
        .metadata()
        .map_err(|_| invalid("source metadata unavailable"))?;
    if !metadata.is_file() {
        return Err(invalid("upload source must be a regular file"));
    }
    if metadata.len() > MAX_UPLOAD_BYTES {
        return Err(invalid("workspace upload exceeds 16 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_UPLOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("source file read failed"))?;
    if bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(invalid("workspace upload exceeds 16 MiB"));
    }
    Ok((bytes, metadata.permissions().mode() & 0o111 != 0))
}

impl Cells {
    pub async fn workspace_upload(
        &self,
        request: &WorkspaceUploadRequest,
    ) -> Result<CellOperation, Error> {
        let client = self.runtime.client.clone();
        self.upload_with(request, move |pod, manifest, bytes| {
            transfer(client, pod, manifest, Some(bytes))
        })
        .await
    }

    /// Identify recorded upload operations without requiring a live runtime.
    #[must_use]
    pub fn is_workspace_upload(operation: &CellOperation) -> bool {
        upload_manifest(operation).is_some()
    }

    /// Retire terminal staging, including on cancellation retries. The helper's
    /// durable receipt also fences writers that were already in flight.
    pub async fn finish_workspace_upload(&self, operation: &CellOperation) -> Result<(), Error> {
        if !matches!(
            operation.phase,
            OperationPhase::Succeeded | OperationPhase::Failed | OperationPhase::Cancelled
        ) {
            return Ok(());
        }
        let Some((component, manifest)) = upload_manifest(operation) else {
            return Ok(());
        };
        // Cancellation authority is sufficient to clean the authorized operation;
        // it does not require renewed authority to execute its original command.
        let (instance, _) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &operation.instance_id,
            &operation.id,
            Capability::ActionCancel,
        )?;
        self.runtime_available()?;
        let _access = self
            .runtime_access()
            .map_err(|error| invalid(error.to_string()))?;
        let pods = Api::<Pod>::namespaced(
            self.runtime.client.clone(),
            &instance_namespace(&instance.instance_key),
        );
        let candidates = pods
            .list(&ListParams::default().labels(&format!(
                "proofstorm.dev/instance={},proofstorm.dev/component={component}",
                instance.instance_key,
            )))
            .await?;
        let mut cleaned = false;
        for pod in candidates.items.into_iter().filter(|pod| {
            pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .as_ref()
                    .and_then(|status| status.phase.as_deref())
                    == Some("Running")
        }) {
            transfer(self.runtime.client.clone(), pod, manifest.clone(), None).await?;
            cleaned = true;
        }
        if !cleaned {
            return Err(invalid(
                "upload staging cleanup needs a running workspace; retry operation_cancel when it is ready",
            ));
        }
        Ok(())
    }

    async fn upload_with<F, Fut>(
        &self,
        request: &WorkspaceUploadRequest,
        transfer: F,
    ) -> Result<CellOperation, Error>
    where
        F: FnOnce(Pod, UploadRequest, Vec<u8>) -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
        self.authorize(&[Capability::ComponentExecLive, Capability::ArtifactRead])?;
        proofstorm_core::workspace::validate_path(&request.path).map_err(invalid)?;
        if request.source_path.is_empty() || request.source_path.len() > 4096 {
            return Err(invalid("source_path must contain 1..4096 bytes"));
        }
        let (bytes, executable) = read_source(Path::new(&request.source_path))?;
        let manifest = UploadRequest {
            upload_id: proofstorm_core::digest_json(&(
                &self.workspace,
                &self.principal,
                &request.request_id,
            ))[7..]
                .into(),
            path: request.path.clone(),
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            executable,
        };
        self.workspace_request_prepared(
            &request.name,
            &request.component,
            &WorkspaceRequest::Upload(manifest.clone()),
            &request.request_id,
            move |instance| async move {
                self.runtime_available()?;
                let _access = self
                    .runtime_access()
                    .map_err(|error| invalid(error.to_string()))?;
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
                let candidates = pods
                    .list(&ListParams::default().labels(&format!(
                        "proofstorm.dev/instance={},proofstorm.dev/component={}",
                        instance.instance_key, request.component
                    )))
                    .await?
                    .items
                    .into_iter()
                    .filter(|pod| {
                        pod.metadata.deletion_timestamp.is_none()
                            && pod
                                .status
                                .as_ref()
                                .and_then(|status| status.phase.as_deref())
                                == Some("Running")
                    })
                    .collect::<Vec<_>>();
                let [pod] = candidates.as_slice() else {
                    return Err(invalid("workspace must have exactly one running pod"));
                };
                let uid = pod
                    .uid()
                    .ok_or_else(|| invalid("workspace pod identity missing"))?;
                transfer(pod.clone(), manifest, bytes).await?;
                let current = cells.get(&instance.resource_name).await?;
                verify_cell(&current, &instance)?;
                if current.uid() != cell.uid()
                    || pods.get(&pod.name_any()).await?.uid() != Some(uid)
                {
                    return Err(invalid(
                        "workspace was replaced during upload; retry the same request",
                    ));
                }
                Ok(())
            },
        )
        .await
    }
}

fn upload_manifest(operation: &CellOperation) -> Option<(String, UploadRequest)> {
    if operation.kind != OperationKind::ComponentExecLive {
        return None;
    }
    let argv: Vec<String> = serde_json::from_value(operation.request.get("argv")?.clone()).ok()?;
    let [runner, workspace, mode, encoded] = argv.as_slice() else {
        return None;
    };
    if runner != WORKSPACE_RUNNER || workspace != "workspace" || mode != "request" {
        return None;
    }
    let WorkspaceRequest::Upload(manifest) =
        proofstorm_core::workspace::wire::decode(serde_json::from_str(encoded).ok()?).ok()?
    else {
        return None;
    };
    Some((
        operation.request.get("component")?.as_str()?.into(),
        manifest,
    ))
}

fn verify_cell(cell: &ProofstormCell, instance: &CellInstance) -> Result<(), Error> {
    proofstorm_kube::require_open_cell(cell).map_err(|error| invalid(error.to_string()))?;
    if cell.spec.instance_id != instance.id
        || cell.spec.instance_key != instance.instance_key
        || cell.spec.workspace_id != instance.workspace_id
        || cell.spec.revision_digest != instance.revision_digest
    {
        return Err(invalid(
            "cell incarnation or revision changed during upload",
        ));
    }
    Ok(())
}

async fn transfer(
    client: kube::Client,
    pod: Pod,
    manifest: UploadRequest,
    bytes: Option<Vec<u8>>,
) -> Result<(), Error> {
    let run = async {
        let namespace = pod
            .namespace()
            .ok_or_else(|| invalid("workspace pod namespace missing"))?;
        let pods = Api::<Pod>::namespaced(client, &namespace);
        let args = vec![
            WORKSPACE_RUNNER.into(),
            "workspace".into(),
            if bytes.is_some() {
                "upload".into()
            } else {
                "upload-finish".into()
            },
            serde_json::to_string(&manifest)
                .map_err(|_| invalid("upload metadata encoding failed"))?,
        ];
        let mut process = pods
            .exec(
                &pod.name_any(),
                args,
                &AttachParams::default()
                    .container("component")
                    .stdin(bytes.is_some())
                    .stdout(true)
                    .stderr(true),
            )
            .await?;
        let mut input = if bytes.is_some() {
            Some(
                process
                    .stdin()
                    .ok_or_else(|| invalid("upload stdin missing"))?,
            )
        } else {
            None
        };
        let stdout = process
            .stdout()
            .ok_or_else(|| invalid("upload stdout missing"))?;
        let stderr = process
            .stderr()
            .ok_or_else(|| invalid("upload stderr missing"))?;
        let status = process
            .take_status()
            .ok_or_else(|| invalid("upload status missing"))?;
        let write = async {
            if let (Some(bytes), Some(input)) = (bytes, input.as_mut()) {
                input.write_all(&bytes).await?;
                input.shutdown().await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let (sent, output, errors, status) =
            tokio::join!(write, read_stream(stdout), read_stream(stderr), status);
        sent.map_err(|_| invalid("upload interrupted; retry the same request"))?;
        let output = output?;
        let errors = errors?;
        process
            .join()
            .await
            .map_err(|_| invalid("upload transport interrupted"))?;
        if status.as_ref().and_then(|status| status.status.as_deref()) != Some("Success") {
            if String::from_utf8_lossy(&errors).contains(STAGING_CAPACITY_ERROR) {
                return Err(invalid(STAGING_CAPACITY_ERROR));
            }
            return Err(invalid(
                "upload staging failed; destination unchanged; retry the same request",
            ));
        }
        let receipt: UploadRequest = serde_json::from_slice(&output)
            .map_err(|_| invalid("invalid upload staging receipt"))?;
        if receipt != manifest {
            return Err(invalid("upload staging receipt does not match the file"));
        }
        Ok(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), run)
        .await
        .map_err(|_| invalid("upload timed out; retry the same request"))?
}

async fn read_stream(stream: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    stream
        .take(8193)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| invalid("upload receipt interrupted"))?;
    if bytes.len() > 8192 {
        return Err(invalid("upload receipt too large"));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;
