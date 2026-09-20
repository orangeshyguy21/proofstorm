//! Ordered edits to a candidate cell. Admission validates the completed candidate.
use crate::{CellPolicy, CellSpec, ComponentSpec, LinkSpec};

#[derive(Debug, Clone, PartialEq)]
pub enum CellPatch {
    AddComponent { component: ComponentSpec },
    UpdateComponent { component: ComponentSpec },
    RemoveComponent { id: String },
    AddLink { link: LinkSpec },
    RemoveLink { id: String },
    SetPolicy { policy: CellPolicy },
}

/// Build a candidate from 1..=100 ordered edits, then canonicalize its ordering.
///
/// Intermediate states may be invalid (for example, removing a component before
/// its links). The caller must validate the final topology, catalog and policy
/// before publishing or persisting the returned candidate.
///
/// # Errors
///
/// Returns an error for an empty or oversized batch, duplicate additions, or
/// updates/removals of absent IDs. No intermediate candidate is returned.
pub fn apply_cell_patch(mut cell: CellSpec, patch: Vec<CellPatch>) -> Result<CellSpec, String> {
    if !(1..=100).contains(&patch.len()) {
        return Err("patch must contain 1..=100 operations".into());
    }
    for change in patch {
        match change {
            CellPatch::AddComponent { component } => {
                if cell.components.iter().any(|item| item.id == component.id) {
                    return Err(format!("Component {:?} already exists", component.id));
                }
                cell.components.push(component);
            }
            CellPatch::UpdateComponent { component } => {
                let current = cell
                    .components
                    .iter_mut()
                    .find(|item| item.id == component.id)
                    .ok_or_else(|| format!("Component {:?} is absent", component.id))?;
                *current = component;
            }
            CellPatch::RemoveComponent { id } => {
                let index = cell
                    .components
                    .iter()
                    .position(|item| item.id == id)
                    .ok_or_else(|| format!("Component {id:?} is absent"))?;
                cell.components.remove(index);
            }
            CellPatch::AddLink { link } => {
                if cell.links.iter().any(|item| item.id == link.id) {
                    return Err(format!("Link {:?} already exists", link.id));
                }
                cell.links.push(link);
            }
            CellPatch::RemoveLink { id } => {
                let index = cell
                    .links
                    .iter()
                    .position(|item| item.id == id)
                    .ok_or_else(|| format!("Link {id:?} is absent"))?;
                cell.links.remove(index);
            }
            CellPatch::SetPolicy { policy } => cell.policy = policy,
        }
    }
    cell.components.sort_by(|a, b| a.id.cmp(&b.id));
    cell.links.sort();
    Ok(cell)
}

#[cfg(test)]
mod tests;
