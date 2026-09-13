//! Borrowed resource indexes shared by every component in one observation pass.
use std::collections::BTreeMap;

use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    core::v1::{PersistentVolumeClaim, Pod, Service},
    discovery::v1::EndpointSlice,
};

use crate::{COMPONENT_LABEL, ComponentObservationResources, probes::ProbeObservation};

/// Retain input order within each key, including duplicate names in supplied snapshots.
pub(crate) struct Lookup<'a, K>(BTreeMap<&'a str, Vec<&'a K>>);

impl<'a, K> Lookup<'a, K> {
    fn new(items: &'a [K], key: impl Fn(&'a K) -> Option<&'a str>) -> Self {
        let mut entries: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for item in items {
            if let Some(key) = key(item) {
                entries.entry(key).or_default().push(item);
            }
        }
        Self(entries)
    }

    pub fn get(&self, key: &str) -> Option<&'a K> {
        self.0.get(key)?.first().copied()
    }

    pub fn all(&self, key: &str) -> impl Iterator<Item = &'a K> + '_ {
        self.0.get(key).into_iter().flatten().copied()
    }
}

pub(crate) struct ObservationIndex<'a> {
    pub deployments: Lookup<'a, Deployment>,
    pub stateful_sets: Lookup<'a, StatefulSet>,
    pub claims: Lookup<'a, PersistentVolumeClaim>,
    pub services: Lookup<'a, Service>,
    pub pods: Lookup<'a, Pod>,
    pub pods_by_uid: Lookup<'a, Pod>,
    pub endpoints: Lookup<'a, EndpointSlice>,
    pub protocol: &'a BTreeMap<String, ProbeObservation>,
}

impl<'a> ObservationIndex<'a> {
    pub fn new(resources: &ComponentObservationResources<'a>) -> Self {
        Self {
            deployments: Lookup::new(resources.deployments, |r| r.metadata.name.as_deref()),
            stateful_sets: Lookup::new(resources.stateful_sets, |r| r.metadata.name.as_deref()),
            claims: Lookup::new(resources.persistent_volume_claims, |r| {
                r.metadata.name.as_deref()
            }),
            services: Lookup::new(resources.services, |r| r.metadata.name.as_deref()),
            pods: Lookup::new(resources.pods, |r| {
                r.metadata
                    .labels
                    .as_ref()?
                    .get(COMPONENT_LABEL)
                    .map(String::as_str)
            }),
            pods_by_uid: Lookup::new(resources.pods, |r| r.metadata.uid.as_deref()),
            endpoints: Lookup::new(resources.endpoint_slices, |r| {
                r.metadata
                    .labels
                    .as_ref()?
                    .get("kubernetes.io/service-name")
                    .map(String::as_str)
            }),
            protocol: resources.protocol,
        }
    }
}
