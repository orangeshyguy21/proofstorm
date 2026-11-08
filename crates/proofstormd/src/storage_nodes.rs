//! Hold back a workload whose pod would mount volumes bound to different nodes.
//!
//! Cell pods may spread across nodes, and a local volume pins its pod to the
//! node it was created on. An edit can make a pod mount two volumes that
//! already live on different nodes, such as a mint re-linked to a Lightning
//! node elsewhere. No such pod can ever start, so applying the edit would only
//! take the running pod away. The controller keeps the old workload instead and
//! reports why.
use k8s_openapi::api::{
    apps::v1::StatefulSet,
    core::v1::{PersistentVolume, PersistentVolumeClaim, PodSpec},
};
use kube::{Api, Client};
use std::collections::BTreeSet;

/// Shown as the component's `pod_unschedulable` blocker. Node names stay out.
pub(super) const HELD_MESSAGE: &str = "Edit not applied: this pod's volumes are bound to different nodes. Revert the change, or remove the component with delete_data and add it back.";

const HOSTNAME: &str = "kubernetes.io/hostname";

/// The claims a pod mounts by name.
pub(super) fn pod_claims(pod: Option<&PodSpec>) -> BTreeSet<String> {
    pod.and_then(|pod| pod.volumes.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|volume| volume.persistent_volume_claim.as_ref())
        .map(|claim| claim.claim_name.clone())
        .collect()
}

/// The claims a `StatefulSet`'s single replica mounts, its claim templates' included.
pub(super) fn stateful_set_claims(stateful_set: &StatefulSet) -> BTreeSet<String> {
    let Some(spec) = stateful_set.spec.as_ref() else {
        return BTreeSet::new();
    };
    let name = stateful_set.metadata.name.as_deref().unwrap_or_default();
    let mut claims = pod_claims(spec.template.spec.as_ref());
    claims.extend(
        spec.volume_claim_templates
            .iter()
            .flatten()
            .filter_map(|template| template.metadata.name.as_deref())
            .map(|template| format!("{template}-{name}-0")),
    );
    claims
}

/// The nodes a volume is pinned to; empty for one any node can mount.
fn volume_nodes(volume: &PersistentVolume) -> BTreeSet<String> {
    volume
        .spec
        .as_ref()
        .and_then(|spec| spec.node_affinity.as_ref())
        .and_then(|affinity| affinity.required.as_ref())
        .into_iter()
        .flat_map(|selector| &selector.node_selector_terms)
        .flat_map(|term| term.match_expressions.iter().flatten())
        .filter(|expression| expression.key == HOSTNAME && expression.operator == "In")
        .flat_map(|expression| expression.values.iter().flatten().cloned())
        .collect()
}

/// Whether no single node holds every volume.
fn split(pinned: &[BTreeSet<String>]) -> bool {
    let mut pinned = pinned.iter().filter(|nodes| !nodes.is_empty());
    let Some(first) = pinned.next() else {
        return false;
    };
    pinned
        .try_fold(first.clone(), |common, nodes| {
            let common: BTreeSet<String> = common.intersection(nodes).cloned().collect();
            (!common.is_empty()).then_some(common)
        })
        .is_none()
}

/// The nodes of the bound volumes behind `claims`, when no single node holds
/// them all. A pod with fewer than two claims can't be split, so it costs no
/// reads.
pub(super) async fn split_nodes(
    client: &Client,
    namespace: &str,
    claims: &BTreeSet<String>,
) -> Result<Option<BTreeSet<String>>, kube::Error> {
    if claims.len() < 2 {
        return Ok(None);
    }
    let bound = Api::<PersistentVolumeClaim>::namespaced(client.clone(), namespace);
    let volumes = Api::<PersistentVolume>::all(client.clone());
    let mut pinned = Vec::new();
    for claim in claims {
        let Some(volume) = bound
            .get_opt(claim)
            .await?
            .and_then(|claim| claim.spec)
            .and_then(|spec| spec.volume_name)
            .filter(|volume| !volume.is_empty())
        else {
            continue;
        };
        if let Some(volume) = volumes.get_opt(&volume).await? {
            pinned.push(volume_nodes(&volume));
        }
    }
    Ok(split(&pinned).then(|| pinned.into_iter().flatten().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn volume(nodes: &[&str]) -> PersistentVolume {
        serde_json::from_value(json!({"metadata":{"name":"pv"},"spec":{"nodeAffinity":{"required":{
            "nodeSelectorTerms":[{"matchExpressions":[{"key":HOSTNAME,"operator":"In","values":nodes}]}]}}}}))
        .unwrap()
    }

    #[test]
    fn volumes_on_different_nodes_are_split_and_shared_or_unpinned_ones_are_not() {
        let pinned = |nodes: &[&[&str]]| {
            nodes
                .iter()
                .map(|nodes| volume_nodes(&volume(nodes)))
                .collect::<Vec<_>>()
        };
        assert!(split(&pinned(&[&["a"], &["b"]])));
        assert!(!split(&pinned(&[&["a"], &["a"]])));
        assert!(!split(&pinned(&[&["a", "b"], &["b"]])));
        // A volume any node can mount, and an unbound claim, pin nothing.
        assert!(!split(&[BTreeSet::new(), volume_nodes(&volume(&["a"]))]));
        assert!(!split(&[]));
        assert!(
            volume_nodes(&serde_json::from_value(json!({"metadata":{"name":"pv"}})).unwrap())
                .is_empty()
        );
    }

    #[test]
    fn a_stateful_set_mounts_its_template_claims_and_its_pod_claims() {
        let set: StatefulSet = serde_json::from_value(json!({"metadata":{"name":"mint"},"spec":{
            "selector":{},"serviceName":"mint",
            "template":{"spec":{"containers":[],"volumes":[
                {"name":"lnd","persistentVolumeClaim":{"claimName":"data-ln-b-0"}},
                {"name":"config","configMap":{"name":"mint"}}]}},
            "volumeClaimTemplates":[{"metadata":{"name":"data"}}]}}))
        .unwrap();
        assert_eq!(
            stateful_set_claims(&set),
            BTreeSet::from(["data-ln-b-0".to_owned(), "data-mint-0".to_owned()])
        );
    }
}
