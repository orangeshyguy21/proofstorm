use k8s_openapi::api::batch::v1::Job;
use serde_json::json;
use thiserror::Error;

use crate::ProofstormCandidateBuild;

pub const CANDIDATE_BUILD_LABEL: &str = "proofstorm.dev/candidate-build";
pub const CANDIDATE_CANCEL_ANNOTATION: &str = "proofstorm.dev/cancel-token";

const GIT_IMAGE: &str =
    "docker.io/alpine/git@sha256:c0280cf9572316299b08544065d3bf35db65043d5e3963982ec50647d2746e26";
const BUILDKIT_IMAGE: &str = "docker.io/moby/buildkit@sha256:a02f6571999693089dc928e9bbb64836c21703b214195d8637c011f1a7025ef6";

#[derive(Debug, Error)]
pub enum CandidateBuildRenderError {
    #[error("candidate build resource name is missing")]
    MissingName,
    #[error("candidate build field {0} contains unsupported shell characters")]
    UnsafeField(&'static str),
    #[error("candidate build Job serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Render the durable, controller-owned `BuildKit` Job for one frozen PR SHA.
///
/// The Git init container verifies the fetched object before `BuildKit` sees the
/// source tree. The build container pushes straight to the local cluster
/// registry and exposes only the resulting digest through its termination log.
///
/// # Errors
///
/// Returns an error for missing or shell-unsafe immutable inputs, or if the
/// typed Kubernetes Job cannot be serialized.
#[allow(
    clippy::too_many_lines,
    reason = "the complete security and resource contract for the one build Job stays visible together"
)]
pub fn render_candidate_build_job(
    build: &ProofstormCandidateBuild,
) -> Result<Job, CandidateBuildRenderError> {
    let resource_name = build
        .metadata
        .name
        .as_deref()
        .ok_or(CandidateBuildRenderError::MissingName)?;
    for (name, value) in [
        ("resource_name", resource_name),
        ("repository", build.spec.repository.as_str()),
        ("commit_sha", build.spec.commit_sha.as_str()),
        ("image_repository", build.spec.image_repository.as_str()),
        ("candidate_id", build.spec.candidate_id.as_str()),
        ("dockerfile", build.spec.dockerfile.as_str()),
        ("implementation", build.spec.implementation.as_str()),
    ] {
        if !shell_safe(value) {
            return Err(CandidateBuildRenderError::UnsafeField(name));
        }
    }
    let destination = format!(
        "{}:{}",
        build.spec.image_repository, build.spec.candidate_id
    );
    let prepare = match build.spec.implementation.as_str() {
        // Nutshell's default image installs every Lightning backend. Historical
        // PR locks can reference removed platform wheels even when Proofstorm
        // only needs the LND adapter. This deterministic profile removes that
        // unused package from the resolved lock without changing candidate
        // source code or the runtime backend under test.
        "nutshell" | "nutshell-wallet" => {
            "sed -i '/RUN poetry install --without dev --no-root/i RUN poetry remove breez-sdk-spark --lock && pip install --no-cache-dir breez-sdk-spark==0.17.0' /workspace/Dockerfile"
        }
        _ => "true",
    };
    let fetch = format!(
        "git init /workspace && cd /workspace && git remote add origin '{}' && git fetch --depth=1 origin '{}' && git checkout --detach FETCH_HEAD && test \"$(git rev-parse HEAD)\" = '{}' && {prepare}",
        build.spec.repository, build.spec.commit_sha, build.spec.commit_sha
    );
    let build_command = format!(
        "buildctl-daemonless.sh build --frontend dockerfile.v0 --local context=/workspace --local dockerfile=/workspace --opt 'filename={}' --output 'type=image,name={destination},push=true,registry.insecure=true' --metadata-file /tmp/build-metadata.json && digest=$(sed -n 's/.*\"containerimage.digest\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' /tmp/build-metadata.json) && test -n \"$digest\" && printf '{{\"digest\":\"%s\"}}' \"$digest\" > /dev/termination-log",
        build.spec.dockerfile
    );
    let labels = json!({
        "app.kubernetes.io/name": "proofstorm-candidate-builder",
        CANDIDATE_BUILD_LABEL: resource_name,
    });
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": format!("{resource_name}-build"),
            "labels": labels,
            "ownerReferences": [{
                "apiVersion": "proofstorm.dev/v1alpha1",
                "kind": "ProofstormCandidateBuild",
                "name": resource_name,
                "uid": build.metadata.uid,
                "controller": true,
                "blockOwnerDeletion": true
            }]
        },
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": 1800,
            "ttlSecondsAfterFinished": 600,
            "template": {
                "metadata": {
                    "labels": labels,
                    "annotations": {
                        "container.apparmor.security.beta.kubernetes.io/buildkit": "unconfined"
                    }
                },
                "spec": {
                    "restartPolicy": "Never",
                    "securityContext": {
                        "fsGroup": 1000,
                        "fsGroupChangePolicy": "OnRootMismatch"
                    },
                    "initContainers": [{
                        "name": "source",
                        "image": GIT_IMAGE,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["/bin/sh", "-ec", fetch],
                        "volumeMounts": [{"name": "workspace", "mountPath": "/workspace"}],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "capabilities": {"drop": ["ALL"]}
                        },
                        "resources": {
                            "requests": {"cpu": "25m", "memory": "32Mi"},
                            "limits": {"cpu": "500m", "memory": "256Mi"}
                        }
                    }],
                    "containers": [{
                        "name": "buildkit",
                        "image": BUILDKIT_IMAGE,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["/bin/sh", "-ec", build_command],
                        "env": [{
                            "name": "BUILDKITD_FLAGS",
                            "value": "--oci-worker-no-process-sandbox"
                        }],
                        "volumeMounts": [
                            {"name": "workspace", "mountPath": "/workspace"},
                            {"name": "buildkit-state", "mountPath": "/home/user/.local/share/buildkit"}
                        ],
                        "securityContext": {
                            "runAsUser": 1000,
                            "runAsGroup": 1000,
                            "seccompProfile": {"type": "Unconfined"}
                        },
                        "resources": {
                            "requests": {"cpu": "250m", "memory": "512Mi"},
                            "limits": {"cpu": "2", "memory": "4Gi"}
                        }
                    }],
                    "volumes": [
                        {"name": "workspace", "emptyDir": {}},
                        {"name": "buildkit-state", "emptyDir": {}}
                    ]
                }
            }
        }
    }))
    .map_err(CandidateBuildRenderError::from)
}

fn shell_safe(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':' | b'@')
        })
}

#[cfg(test)]
mod tests {
    use kube::Resource;

    use super::*;
    use crate::ProofstormCandidateBuildSpec;

    #[test]
    fn candidate_job_freezes_source_and_pushes_to_local_registry() {
        let mut build = ProofstormCandidateBuild::new(
            "candidate-nutshell-1095",
            ProofstormCandidateBuildSpec {
                workspace_id: "local".into(),
                candidate_id: "nutshell-1095-aabbccdd".into(),
                principal_id: "local".into(),
                implementation: "nutshell".into(),
                base_version: "0.20.0".into(),
                pull_request_url: "https://github.com/cashubtc/nutshell/pull/1095".into(),
                repository: "https://github.com/cashubtc/nutshell.git".into(),
                commit_sha: "aabbccddaabbccddaabbccddaabbccddaabbccdd".into(),
                version: "candidate-pr1095-aabbccdd".into(),
                request_digest: "sha256:request".into(),
                accepted_at_unix: 1,
                image_repository: "proofstorm-registry.localhost:5000/candidates/nutshell".into(),
                dockerfile: "Dockerfile".into(),
            },
        );
        build.meta_mut().uid = Some("uid-1".into());
        let job = render_candidate_build_job(&build).expect("render build Job");
        let encoded = serde_json::to_string(&job).expect("encode Job");
        assert!(encoded.contains("git fetch --depth=1 origin"));
        assert!(encoded.contains(&build.spec.commit_sha));
        assert!(encoded.contains("registry.insecure=true"));
        assert!(encoded.contains("containerimage.digest"));
        assert!(encoded.contains("[[:space:]]*:[[:space:]]*"));
        assert!(encoded.contains("breez-sdk-spark==0.17.0"));
        assert!(!encoded.contains(&build.spec.pull_request_url));
    }
}
