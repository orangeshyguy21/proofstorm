use k8s_openapi::api::batch::v1::Job;
use proofstorm_core::CandidateInput;
use serde_json::json;

#[cfg(test)]
#[path = "candidate_recipe_tests.rs"]
mod recipe_tests;
use thiserror::Error;

use crate::ProofstormCandidateBuild;

pub const CANDIDATE_BUILD_LABEL: &str = "proofstorm.dev/candidate-build";
pub const CANDIDATE_CANCEL_ANNOTATION: &str = "proofstorm.dev/cancel-token";
/// Secret key holding a bundle source's bytes; see [`candidate_bundle_secret`].
pub const CANDIDATE_BUNDLE_KEY: &str = "source.bundle";

/// The Secret, owned by the candidate build, that carries a bundle source.
/// Only the `source` init container mounts it.
#[must_use]
pub fn candidate_bundle_secret(resource_name: &str) -> String {
    format!("{resource_name}-bundle")
}

use crate::images::{BUILDKIT_IMAGE, GIT_IMAGE};

#[derive(Debug, Error)]
pub enum CandidateBuildRenderError {
    #[error("candidate build resource name is missing")]
    MissingName,
    #[error("candidate build field {0} contains unsupported shell characters")]
    UnsafeField(&'static str),
    #[error("candidate build provenance is inconsistent or unsupported")]
    InvalidProvenance,
    #[error("candidate build Job serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Render the durable, controller-owned `BuildKit` Job for one frozen source SHA.
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
    let prepare = if let Some(provenance) = &build.spec.provenance {
        if provenance.validate().is_err()
            || provenance.profile.dockerfile != build.spec.dockerfile
            || !provenance.profile.platforms.contains(&provenance.platform)
        {
            return Err(CandidateBuildRenderError::InvalidProvenance);
        }
        provenance.profile.prepare.clone()
    } else {
        match build.spec.implementation.as_str() {
            // Nutshell's default image installs every Lightning backend. Historical
            // PR locks can reference removed platform wheels even when Proofstorm
            // only needs the LND adapter. This deterministic profile removes that
            // unused package from the resolved lock without changing candidate
            // source code or the runtime backend under test.
            "nutshell" | "nutshell-wallet" => {
                let mut script = "sed -i '/RUN poetry install --without dev --no-root/i RUN poetry remove breez-sdk-spark --lock && pip install --no-cache-dir breez-sdk-spark==0.17.0' /workspace/Dockerfile".to_owned();
                script.push_str("\ncat >> /workspace/Dockerfile <<'PROOFSTORM_MANAGEMENT_EOF'\n");
                script.push_str(include_str!(
                    "../drivers/candidate_nutshell_management.Dockerfile"
                ));
                script.push_str("\nPROOFSTORM_MANAGEMENT_EOF\n");
                script
            }
            "cdk" => format!(
                "cat > /tmp/management-prefix <<'PROOFSTORM_MANAGEMENT_EOF'\n{}\nPROOFSTORM_MANAGEMENT_EOF\ncat /tmp/management-prefix /workspace/{dockerfile} > /tmp/management-dockerfile\ncat >> /tmp/management-dockerfile <<'PROOFSTORM_MANAGEMENT_EOF'\nCOPY --from=proofstorm-management-client /src/target/release/cdk-mint-cli /usr/local/bin/cdk-mint-cli\nRUN cdk-mint-cli --version\nPROOFSTORM_MANAGEMENT_EOF\nmv /tmp/management-dockerfile /workspace/{dockerfile}",
                include_str!("../drivers/candidate_cdk_management.Dockerfile"),
                dockerfile = build.spec.dockerfile,
            ),
            _ => "true".to_owned(),
        }
    };
    let bundle = match build
        .spec
        .provenance
        .as_ref()
        .map(|p| (p, &p.requested_source))
    {
        Some((
            provenance,
            source @ CandidateInput::Bundle {
                commit,
                prerequisites,
                ..
            },
        )) => {
            if source.normalized(&provenance.profile.repository).as_ref() != Ok(source)
                || *commit != build.spec.commit_sha
            {
                return Err(CandidateBuildRenderError::InvalidProvenance);
            }
            Some(prerequisites)
        }
        _ => None,
    };
    // Bundle sources fetch only their public prerequisites; the private commits
    // come from the mounted bundle, which git verifies before checkout.
    let fetch_commit = bundle.map_or_else(
        || format!("git fetch --depth=1 origin '{}'", build.spec.commit_sha),
        |prerequisites| {
            let mut steps = Vec::new();
            if !prerequisites.is_empty() {
                let shas = prerequisites.iter().map(|sha| format!("'{sha}'"));
                steps.push(format!(
                    "git fetch --depth=1 origin {}",
                    shas.collect::<Vec<_>>().join(" ")
                ));
            }
            steps.push(format!(
                "git bundle verify --quiet /bundle/{CANDIDATE_BUNDLE_KEY}"
            ));
            steps.push(format!(
                "git fetch /bundle/{CANDIDATE_BUNDLE_KEY} '{}'",
                build.spec.commit_sha
            ));
            steps.join(" && ")
        },
    );
    let fetch = format!(
        "git init /workspace && cd /workspace && git remote add origin '{}' && {fetch_commit} && git checkout --detach FETCH_HEAD && test \"$(git rev-parse HEAD)\" = '{}' && {prepare}",
        build.spec.repository, build.spec.commit_sha
    );
    let mut source_mounts = vec![json!({"name": "workspace", "mountPath": "/workspace"})];
    let mut volumes = vec![
        json!({"name": "workspace", "emptyDir": {}}),
        json!({"name": "buildkit-state", "emptyDir": {}}),
    ];
    if bundle.is_some() {
        source_mounts.push(json!({"name": "bundle", "mountPath": "/bundle", "readOnly": true}));
        volumes.push(json!({"name": "bundle", "secret": {
            "secretName": candidate_bundle_secret(resource_name),
            "defaultMode": 0o444
        }}));
    }
    let platform = build
        .spec
        .provenance
        .as_ref()
        .map_or(String::new(), |p| format!(" --opt platform={}", p.platform));
    let build_command = format!(
        "buildctl-daemonless.sh build --frontend dockerfile.v0 --local context=/workspace --local dockerfile=/workspace --opt 'filename={}' --output 'type=image,name={destination},push=true,registry.insecure=true' --metadata-file /tmp/build-metadata.json && digest=$(sed -n 's/.*\"containerimage.digest\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' /tmp/build-metadata.json) && test -n \"$digest\" && printf '{{\"digest\":\"%s\"}}' \"$digest\" > /dev/termination-log",
        build.spec.dockerfile
    );
    let build_command = build_command.replacen(
        "build --frontend",
        &format!("build{platform} --frontend"),
        1,
    );
    let labels = json!({
        "app.kubernetes.io/name": "proofstorm-candidate-builder",
        CANDIDATE_BUILD_LABEL: resource_name,
    });
    let provenance = build.spec.provenance.as_ref();
    let source_image = provenance.map_or(GIT_IMAGE, |p| p.source_image.as_str());
    let builder_image = provenance.map_or(BUILDKIT_IMAGE, |p| p.builder_image.as_str());
    if !shell_safe(source_image) || !shell_safe(builder_image) {
        return Err(CandidateBuildRenderError::InvalidProvenance);
    }
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
            "activeDeadlineSeconds": provenance.map_or(1800, |p| p.profile.deadline_seconds),
            "template": {
                "metadata": {
                    "labels": labels,
                    "annotations": {
                        "container.apparmor.security.beta.kubernetes.io/buildkit": "unconfined"
                    }
                },
                "spec": {
                    "restartPolicy": "Never",
                    "automountServiceAccountToken": false,
                    "securityContext": {
                        "fsGroup": 1000,
                        "fsGroupChangePolicy": "OnRootMismatch"
                    },
                    "initContainers": [{
                        "name": "source",
                        "image": source_image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["/bin/sh", "-ec", fetch],
                        "volumeMounts": source_mounts,
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
                        "image": builder_image,
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
                            "limits": {"cpu": format!("{}m",provenance.map_or(2000, |p| p.profile.cpu_limit_millicores)), "memory": format!("{}Mi",provenance.map_or(4096, |p| p.profile.memory_limit_mib))}
                        }
                    }],
                    "volumes": volumes
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

    fn assert_historical_budget(build: &ProofstormCandidateBuild) {
        let mut historical = build.clone();
        let saved = historical.spec.provenance.as_mut().unwrap();
        saved.profile.version = 1;
        saved.profile.catalog_implementations.clear();
        saved.profile.cpu_limit_millicores = 2000;
        saved.profile.deadline_seconds = 1800;
        saved.profile_digest = saved.profile.digest();
        let job = serde_json::to_value(render_candidate_build_job(&historical).unwrap()).unwrap();
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 1800);
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["cpu"],
            "2000m"
        );
        let saved = historical.spec.provenance.as_mut().unwrap();
        saved.profile.cpu_limit_millicores =
            proofstorm_core::CANDIDATE_BUILD_MAX_CPU_MILLICORES + 1;
        saved.profile_digest = saved.profile.digest();
        assert!(saved.validate().is_err());
        saved.profile.cpu_limit_millicores = 2000;
        saved.profile.deadline_seconds = proofstorm_core::CANDIDATE_BUILD_MAX_DEADLINE_SECONDS + 1;
        saved.profile_digest = saved.profile.digest();
        assert!(saved.validate().is_err());
    }

    #[test]
    fn candidate_jobs_bind_saved_recipes_platforms_and_cleanup_to_evidence() {
        for entry in proofstorm_core::default_catalog()
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    proofstorm_core::ComponentKind::Mint | proofstorm_core::ComponentKind::Wallet
                )
            })
        {
            for platform in ["linux/arm64", "linux/amd64"] {
                let mut profile = proofstorm_core::candidate_build_profile(&entry.id).unwrap();
                // A saved profile remains authoritative after the shipped registry changes.
                profile.version = 17;
                profile.prepare.push_str("\nprintf saved-recipe-marker\n");
                let provenance = proofstorm_core::CandidateProvenance {
                    schema_version: 1,
                    input_digest: "input".into(),
                    requested_source: proofstorm_core::CandidateInput::Commit {
                        sha: Some("a".repeat(40)),
                        url: None,
                        repository: None,
                    },
                    platform: platform.into(),
                    source_image: GIT_IMAGE.into(),
                    builder_image: BUILDKIT_IMAGE.into(),
                    baseline_digest: proofstorm_core::digest_json(entry),
                    profile_digest: profile.digest(),
                    profile: profile.clone(),
                };
                let mut build = ProofstormCandidateBuild::new(
                    "candidate-test",
                    ProofstormCandidateBuildSpec {
                        workspace_id: "workspace".into(),
                        candidate_id: "test".into(),
                        principal_id: "agent".into(),
                        implementation: entry.id.clone(),
                        base_version: entry.version.clone(),
                        pull_request_url: String::new(),
                        repository: format!("https://github.com/{}.git", profile.repository),
                        commit_sha: "a".repeat(40),
                        version: "candidate-test".into(),
                        request_digest: "request".into(),
                        accepted_at_unix: 1,
                        provenance: Some(provenance),
                        image_repository: format!("registry:5000/candidates/{}", entry.id),
                        dockerfile: profile.dockerfile.clone(),
                    },
                );
                build.meta_mut().uid = Some("uid".into());
                let job =
                    serde_json::to_value(render_candidate_build_job(&build).unwrap()).unwrap();
                let pod = &job["spec"]["template"]["spec"];
                assert_eq!(pod["automountServiceAccountToken"], false);
                let fetch = pod["initContainers"][0]["command"][2].as_str().unwrap();
                assert!(fetch.contains(&profile.prepare));
                assert!(fetch.contains(&format!(
                    "test \"$(git rev-parse HEAD)\" = '{}'",
                    "a".repeat(40)
                )));
                assert!(
                    pod["containers"][0]["command"][2]
                        .as_str()
                        .unwrap()
                        .contains(&format!("--opt platform={platform}"))
                );
                assert_eq!(pod["containers"][0]["image"], BUILDKIT_IMAGE);
                assert_eq!(
                    job["spec"]["activeDeadlineSeconds"],
                    profile.deadline_seconds
                );
                assert_eq!(
                    pod["containers"][0]["resources"]["limits"]["cpu"],
                    format!("{}m", profile.cpu_limit_millicores)
                );
                assert_eq!(
                    pod["containers"][0]["resources"]["limits"]["memory"],
                    "4096Mi"
                );
                assert!(
                    job["spec"].get("ttlSecondsAfterFinished").is_none(),
                    "cleanup must wait for archived diagnostics"
                );
                assert_historical_budget(&build);
                build
                    .spec
                    .provenance
                    .as_mut()
                    .unwrap()
                    .profile
                    .prepare
                    .push_str("tampered");
                assert!(matches!(
                    render_candidate_build_job(&build),
                    Err(CandidateBuildRenderError::InvalidProvenance)
                ));
            }
        }
    }

    #[test]
    fn bundle_sources_reach_only_the_source_container_and_never_fetch_the_tip() {
        let entry = proofstorm_core::default_catalog()
            .entries
            .iter()
            .find(|entry| entry.id == "cdk")
            .unwrap();
        let profile = proofstorm_core::candidate_build_profile("cdk").unwrap();
        let (commit, base) = ("c".repeat(40), "b".repeat(40));
        let render = |source: proofstorm_core::CandidateInput, commit_sha: &str| {
            let mut build = ProofstormCandidateBuild::new(
                "candidate-test",
                ProofstormCandidateBuildSpec {
                    workspace_id: "workspace".into(),
                    candidate_id: "test".into(),
                    principal_id: "agent".into(),
                    implementation: "cdk".into(),
                    base_version: entry.version.clone(),
                    pull_request_url: String::new(),
                    repository: format!("https://github.com/{}.git", profile.repository),
                    commit_sha: commit_sha.into(),
                    version: "candidate-test".into(),
                    request_digest: "request".into(),
                    accepted_at_unix: 1,
                    provenance: Some(proofstorm_core::CandidateProvenance {
                        schema_version: 1,
                        input_digest: "input".into(),
                        requested_source: source,
                        platform: "linux/arm64".into(),
                        source_image: GIT_IMAGE.into(),
                        builder_image: BUILDKIT_IMAGE.into(),
                        baseline_digest: proofstorm_core::digest_json(entry),
                        profile_digest: profile.digest(),
                        profile: profile.clone(),
                    }),
                    image_repository: "registry:5000/candidates/cdk".into(),
                    dockerfile: profile.dockerfile.clone(),
                },
            );
            build.meta_mut().uid = Some("uid".into());
            render_candidate_build_job(&build).map(|job| serde_json::to_value(job).unwrap())
        };
        let bundle = |prerequisites: Vec<String>| proofstorm_core::CandidateInput::Bundle {
            path: None,
            commit: commit.clone(),
            sha256: Some("d".repeat(64)),
            prerequisites,
        };
        let job = render(bundle(vec![base.clone()]), &commit).unwrap();
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(
            pod["initContainers"][0]["volumeMounts"][1],
            json!({"name": "bundle", "mountPath": "/bundle", "readOnly": true})
        );
        assert!(
            !pod["containers"][0]["volumeMounts"]
                .to_string()
                .contains("bundle"),
            "BuildKit must never see the bundle"
        );
        assert_eq!(
            pod["volumes"][2]["secret"]["secretName"],
            candidate_bundle_secret("candidate-test")
        );
        let fetch = pod["initContainers"][0]["command"][2].as_str().unwrap();
        assert!(fetch.contains(&format!("git fetch --depth=1 origin '{base}' && ")));
        assert!(fetch.contains(&format!(
            "git bundle verify --quiet /bundle/source.bundle && git fetch /bundle/source.bundle '{commit}'"
        )));
        assert!(!fetch.contains(&format!("origin '{commit}'")));
        assert!(fetch.contains(&format!("test \"$(git rev-parse HEAD)\" = '{commit}'")));

        // A complete bundle never contacts GitHub for source.
        let job = render(bundle(Vec::new()), &commit).unwrap();
        let fetch = job["spec"]["template"]["spec"]["initContainers"][0]["command"][2]
            .as_str()
            .unwrap();
        assert!(!fetch.contains("git fetch --depth=1 origin"));

        let public = proofstorm_core::CandidateInput::Commit {
            sha: Some(commit.clone()),
            url: None,
            repository: None,
        };
        let job = render(public, &commit).unwrap();
        assert!(!job.to_string().contains("bundle"));

        for (source, commit_sha) in [
            (bundle(vec![base.clone()]), base.clone()),
            (bundle(vec![base.to_ascii_uppercase()]), commit.clone()),
        ] {
            assert!(matches!(
                render(source, &commit_sha),
                Err(CandidateBuildRenderError::InvalidProvenance)
            ));
        }
    }

    #[test]
    fn candidate_job_freezes_source_and_pushes_to_local_registry() {
        let mut build = ProofstormCandidateBuild::new(
            "candidate-nutshell-1095",
            ProofstormCandidateBuildSpec {
                provenance: None,
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
        let legacy = serde_json::to_value(&job).unwrap();
        assert_eq!(legacy["spec"]["activeDeadlineSeconds"], 1800);
        assert_eq!(
            legacy["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["cpu"],
            "2000m"
        );
        let encoded = serde_json::to_string(&job).expect("encode Job");
        assert!(encoded.contains("git fetch --depth=1 origin"));
        assert!(encoded.contains(&build.spec.commit_sha));
        assert!(encoded.contains("registry.insecure=true"));
        assert!(encoded.contains("containerimage.digest"));
        assert!(encoded.contains("[[:space:]]*:[[:space:]]*"));
        assert!(encoded.contains("breez-sdk-spark==0.17.0"));
        assert!(!encoded.contains(&build.spec.pull_request_url));
        assert!(encoded.contains("mint-cli --help"));
        {
            build.spec.implementation = "cdk".into();
            let job = render_candidate_build_job(&build).expect("CDK candidate");
            let encoded = serde_json::to_string(&job).unwrap();
            assert!(encoded.contains("cargo build --locked --release --bin cdk-mint-cli"));
            assert!(encoded.contains("COPY --from=proofstorm-management-client"));
            assert!(
                !encoded.contains("releases/download"),
                "candidate client must use candidate source"
            );
        }
    }
}
