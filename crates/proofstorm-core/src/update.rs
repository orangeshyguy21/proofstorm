//! Deterministic, state-preserving edits of an existing lab.
use crate::{PublishedRevision, digest_json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabUpdateTarget {
    pub instance_id: String,
    pub expected_generation: u64,
    #[serde(default)]
    pub delete_data: bool,
    /// Explicitly purge data retained by earlier component removals.
    #[serde(default)]
    pub delete_retained: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabChanges {
    pub added: Vec<String>,
    pub unchanged: Vec<String>,
    pub restarted: Vec<String>,
    pub removed: Vec<String>,
    pub deleted_data: Vec<String>,
    pub connections_changed: bool,
    pub policy_changed: bool,
    pub unsupported: Vec<String>,
    /// Image references are catalog-validated, not a claim of a successful node pull.
    pub required_images: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabUpdatePlan {
    #[serde(default)]
    pub instance_key: String,
    pub target: LabUpdateTarget,
    pub base_revision: String,
    pub target_revision: String,
    pub target_lock: String,
    pub changes: LabChanges,
    pub digest: String,
}

impl LabUpdatePlan {
    pub fn bind_instance(&mut self, key: &str) {
        self.instance_key = key.into();
        self.digest.clear();
        self.digest = digest_json(self);
    }

    /// Build a deterministic edit from two published configurations.
    ///
    /// # Errors
    /// Returns an error if a revision has an incomplete lock.
    pub fn new(
        target: LabUpdateTarget,
        old: &PublishedRevision,
        new: &PublishedRevision,
    ) -> Result<Self, String> {
        let mut changes = LabChanges::default();
        for component in &new.lab.components {
            let next = locked(new, &component.id)?;
            changes.required_images.push(next.image.clone());
            if let Some(previous) = old.lab.components.iter().find(|c| c.id == component.id) {
                let prior = locked(old, &component.id)?;
                if prior.image != next.image
                    || prior.catalog_id != next.catalog_id
                    || previous.implementation != component.implementation
                    || previous.kind != component.kind
                {
                    changes.unsupported.push(format!(
                        "{}: backend/image replacement requires a validated state migration",
                        component.id
                    ));
                }
                // These settings can change persistent state identity or immutable storage fields.
                for key in [
                    "storage",
                    "storage_backend",
                    "database",
                    "network",
                    "wallet_name",
                ] {
                    if previous.config.get(key) != component.config.get(key) {
                        changes.unsupported.push(format!(
                            "{}: changing {key} requires an explicit state migration",
                            component.id
                        ));
                    }
                }
                if prior.rollout_digest == next.rollout_digest {
                    changes.unchanged.push(component.id.clone());
                } else {
                    changes.restarted.push(component.id.clone());
                }
            } else {
                changes.added.push(component.id.clone());
            }
        }
        changes.removed = old
            .lab
            .components
            .iter()
            .filter(|c| !new.lab.components.iter().any(|n| n.id == c.id))
            .map(|c| c.id.clone())
            .collect();
        changes.deleted_data.clone_from(&target.delete_retained);
        if target.delete_data {
            changes.deleted_data.extend(changes.removed.clone());
        }
        for component in &old.lab.components {
            if !new.lab.components.iter().any(|c| c.id == component.id) {
                continue;
            }
            let databases = |revision: &PublishedRevision| {
                revision
                    .lab
                    .links
                    .iter()
                    .filter(|l| {
                        l.from == component.id && l.kind == crate::LinkKind::DatabaseBackend
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            if databases(old) != databases(new) {
                changes.unsupported.push(format!(
                    "{}: database reconnection requires a validated data migration",
                    component.id
                ));
            }
        }
        changes.policy_changed = old.lab.policy != new.lab.policy;
        changes.connections_changed = old.lab.links != new.lab.links;
        for list in [
            &mut changes.added,
            &mut changes.unchanged,
            &mut changes.restarted,
            &mut changes.removed,
            &mut changes.deleted_data,
            &mut changes.unsupported,
            &mut changes.required_images,
        ] {
            list.sort();
            list.dedup();
        }
        let mut plan = Self {
            instance_key: String::new(),
            target,
            base_revision: old.digest.clone(),
            target_revision: new.digest.clone(),
            target_lock: new.lock.digest.clone(),
            changes,
            digest: String::new(),
        };
        plan.digest = digest_json(&plan);
        Ok(plan)
    }
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.base_revision == self.target_revision && self.changes.deleted_data.is_empty()
    }

    #[must_use]
    pub fn affected(&self) -> BTreeSet<String> {
        self.changes
            .restarted
            .iter()
            .chain(&self.changes.removed)
            .cloned()
            .collect()
    }
}

fn locked<'a>(
    revision: &'a PublishedRevision,
    component: &str,
) -> Result<&'a crate::LockEntry, String> {
    revision
        .lock
        .entries
        .iter()
        .find(|e| e.component_id == component)
        .ok_or_else(|| format!("revision lock is missing component {component}"))
}
