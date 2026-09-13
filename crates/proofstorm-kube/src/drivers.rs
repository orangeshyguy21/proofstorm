//! Install one shared native helper into component/action Pods.
use k8s_openapi::api::core::v1::PodSpec;
use serde_json::json;

pub use proofstorm_driver::BINARY as DRIVER_PATH;
pub const DRIVER_IMAGE: &str = "proofstorm-controller:driver-image-required";

/// Install the controller's static helper before component processes start.
/// # Errors
/// Rejects an invalid generated container/volume shape.
pub fn install(pod: &mut PodSpec) -> Result<(), serde_json::Error> {
    let volumes = pod.volumes.get_or_insert_default();
    volumes.push(serde_json::from_value(
        json!({"name":"proofstorm-driver","emptyDir":{}}),
    )?);
    for container in &mut pod.containers {
        container
            .volume_mounts
            .get_or_insert_default()
            .push(serde_json::from_value(json!({
                "name":"proofstorm-driver","mountPath":"/opt/proofstorm","readOnly":true
            }))?);
    }
    pod.init_containers.get_or_insert_default().insert(0,serde_json::from_value(json!({
        "name":"proofstorm-driver", "image":DRIVER_IMAGE, "imagePullPolicy":"IfNotPresent",
        "command":["/usr/local/lib/proofstorm-driver","install"],
        "securityContext":{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"readOnlyRootFilesystem":true,"runAsNonRoot":true},
        "resources":{"requests":{"cpu":"10m","memory":"16Mi"},"limits":{"cpu":"200m","memory":"128Mi"}},
        "volumeMounts":[{"name":"proofstorm-driver","mountPath":"/opt/proofstorm"}]
    }))?);
    Ok(())
}

/// Bind the generated helper placeholder to this controller's verified image.
pub fn bind_image(pod: &mut PodSpec, image: &str) {
    for container in pod.init_containers.iter_mut().flatten() {
        if container.name == "proofstorm-driver" && container.image.as_deref() == Some(DRIVER_IMAGE)
        {
            container.image = Some(image.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_is_installed_first_and_is_read_only_in_workload_containers() {
        let mut pod: PodSpec = serde_json::from_value(json!({
            "containers":[{"name":"component","image":"component:locked"},{"name":"observer","image":"observer:locked"}],
            "initContainers":[{"name":"credentials","image":"credentials:locked"}],
            "volumes":[{"name":"data","emptyDir":{}}]
        })).unwrap();
        install(&mut pod).unwrap();
        for container in &pod.containers {
            let mounts = container.volume_mounts.as_ref().unwrap();
            assert_eq!(mounts.len(), 1);
            assert_eq!(mounts[0].mount_path, "/opt/proofstorm");
            assert_eq!(mounts[0].read_only, Some(true));
        }
        bind_image(&mut pod, "controller@sha256:verified");
        let init = pod.init_containers.as_ref().unwrap();
        assert_eq!(init[0].name, "proofstorm-driver");
        assert_eq!(init[0].image.as_deref(), Some("controller@sha256:verified"));
        assert_eq!(init[1].image.as_deref(), Some("credentials:locked"));
        let security = init[0].security_context.as_ref().unwrap();
        assert_eq!(security.run_as_non_root, Some(true));
        assert_eq!(security.allow_privilege_escalation, Some(false));
        assert_eq!(security.read_only_root_filesystem, Some(true));
        // Binding is exact; it cannot replace a pre-existing non-placeholder image.
        bind_image(&mut pod, "unrelated");
        assert_eq!(
            pod.init_containers.as_ref().unwrap()[0].image.as_deref(),
            Some("controller@sha256:verified")
        );
    }
}
