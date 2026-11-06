use k8s_openapi::api::core::v1::{
    Capabilities, PodSecurityContext, SeccompProfile, SecurityContext,
};

// Cell pods carry no node affinity. A pod that mounts another component's
// volume is placed on that volume's node by Kubernetes itself, so sharers stay
// together and everything else may spread across nodes.

pub(crate) fn pod_security(user: i64) -> PodSecurityContext {
    PodSecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(user),
        run_as_group: Some(user),
        fs_group: Some(user),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) fn container_security() -> SecurityContext {
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".into()]),
            ..Default::default()
        }),
        ..Default::default()
    }
}
