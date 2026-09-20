use std::collections::BTreeMap;

use k8s_openapi::{
    api::core::v1::{
        Affinity, Capabilities, PodAffinity, PodAffinityTerm, PodSecurityContext, SeccompProfile,
        SecurityContext,
    },
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};

use crate::INSTANCE_LABEL;

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

pub(crate) fn instance_affinity(instance_key: &str) -> Affinity {
    Affinity {
        pod_affinity: Some(PodAffinity {
            required_during_scheduling_ignored_during_execution: Some(vec![PodAffinityTerm {
                label_selector: Some(LabelSelector {
                    match_labels: Some(BTreeMap::from([(
                        INSTANCE_LABEL.into(),
                        instance_key.into(),
                    )])),
                    ..Default::default()
                }),
                topology_key: "kubernetes.io/hostname".into(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}
