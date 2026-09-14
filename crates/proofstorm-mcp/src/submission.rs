//! One immutable preview path for complete specifications and bounded stable-ID edits.
use crate::{
    AddLinkInput, CellInput, ErrorData, ProofstormMcp, coded_invalid_request, store_error,
};
use proofstorm_core::{
    CellPolicy, CellSpec, CellUpdateTarget, ComponentSpec, LinkSpec, digest_json,
};
use proofstorm_store::{CellPreview, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmissionRequest {
    pub name: String,
    /// Stable ID for this submission. Reuse the entire request for exact retries.
    pub request_id: String,
    /// Supply exactly one of cell, patch, or plan. Files are read inside the MCP working directory.
    #[serde(default)]
    pub cell: Option<CellInput>,
    /// Up to 100 ordered operations, validated atomically against the fenced desired revision.
    #[serde(default)]
    pub patch: Option<Vec<CellPatch>>,
    /// Immutable reference returned by `cell_plan`. Only `cell_up` accepts this form.
    #[serde(default)]
    pub plan: Option<PlanReference>,
    /// Required with `expected_instance_key` when editing; omitted for creation.
    #[serde(default)]
    pub expected_generation: Option<u64>,
    #[serde(default)]
    pub expected_instance_key: Option<String>,
    /// Explicitly delete data of removed components. Default preserves it.
    #[serde(default)]
    pub delete_data: bool,
    #[serde(default)]
    pub delete_retained: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanReference {
    pub id: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum CellPatch {
    AddComponent { component: ComponentSpec },
    UpdateComponent { component: ComponentSpec },
    RemoveComponent { id: String },
    AddLink { link: AddLinkInput },
    RemoveLink { id: String },
    SetPolicy { policy: CellPolicy },
}

impl ProofstormMcp {
    #[allow(
        clippy::too_many_lines,
        reason = "keep shared preview validation, retry lookup and immutable admission in one auditable path"
    )]
    pub(super) fn prepare_submission(
        &self,
        request: SubmissionRequest,
        applying: bool,
    ) -> Result<CellPreview, ErrorData> {
        self.authorize_all(
            proofstorm_core::mcp::tool(if applying { "cell_up" } else { "cell_plan" })
                .expect("public tool")
                .capabilities,
        )?;
        if request.request_id.is_empty()
            || request.request_id.len() > 128
            || request.name.len() > 63
        {
            return Err(invalid(
                "name must fit 63 bytes and request_id must contain 1..=128 bytes",
            ));
        }
        if usize::from(request.cell.is_some())
            + usize::from(request.patch.is_some())
            + usize::from(request.plan.is_some())
            != 1
        {
            return Err(invalid("Supply exactly one of cell, patch or plan"));
        }
        if let Some(reference) = &request.plan {
            if !applying
                || request.expected_generation.is_some()
                || request.expected_instance_key.is_some()
                || request.delete_data
                || !request.delete_retained.is_empty()
            {
                return Err(invalid(
                    "A bound plan is applied by cell_up using its saved preconditions; omit edit options",
                ));
            }
            let preview = self
                .store
                .cell_preview(&self.workspace, &self.principal, &reference.id)
                .map_err(store_error)?
                .ok_or_else(|| invalid("Plan not found for this actor"))?;
            if preview.name != request.name || digest_json(&preview) != reference.digest {
                return Err(coded_invalid_request(
                    "cell_plan_digest_mismatch",
                    "Plan name or digest differs from the reviewed record",
                ));
            }
            return Ok(preview);
        }
        let input = request
            .cell
            .map(CellSpec::try_from)
            .transpose()
            .map_err(invalid)?;
        let request_digest = digest_json(&(
            &request.name,
            &input,
            &request.patch,
            request.expected_generation,
            &request.expected_instance_key,
            request.delete_data,
            &request.delete_retained,
        ));
        let id = format!(
            "preview-{}",
            &digest_json(&(&self.workspace, &self.principal, &request.request_id))[7..39]
        );
        if let Some(preview) = self
            .store
            .cell_preview(&self.workspace, &self.principal, &id)
            .map_err(store_error)?
        {
            if preview.request_digest != request_digest {
                return Err(coded_invalid_request(
                    "idempotency_conflict",
                    "request_id was used for different input; use a new ID for new work",
                ));
            }
            return Ok(preview);
        }
        let target = match self
            .store
            .resolve_cell(&self.workspace, &self.principal, &request.name)
        {
            Ok(handle) => Some(
                self.store
                    .instance(&self.workspace, &self.principal, &handle.instance_id)
                    .map_err(store_error)?,
            ),
            Err(StoreError::NotFound { .. }) => None,
            Err(error) => return Err(store_error(error)),
        };
        if let Some(instance) = &target {
            if request.expected_generation != Some(instance.generation)
                || request.expected_instance_key.as_deref() != Some(instance.instance_key.as_str())
            {
                return Err(coded_invalid_request(
                    "cell_update_conflict",
                    "Copy desired_generation and instance_key from cell_inspect before preparing an edit",
                ));
            }
        } else if request.expected_generation.is_some()
            || request.expected_instance_key.is_some()
            || request.patch.is_some()
            || request.delete_data
            || !request.delete_retained.is_empty()
        {
            return Err(coded_invalid_request(
                "cell_update_conflict",
                "The requested existing cell is absent; nothing accepted",
            ));
        }
        let mut cell = if let Some(input) = input {
            input
        } else {
            let instance = target.as_ref().expect("patch requires existing cell");
            self.store
                .revision(&self.workspace, &self.principal, &instance.revision_digest)
                .map_err(store_error)?
                .cell
        };
        // The external name is authoritative, including when copying a configuration.
        cell.name.clone_from(&request.name);
        if let Some(patch) = &request.patch {
            apply_patch(&mut cell, patch)?;
        }
        let catalog = self
            .store
            .effective_catalog(&self.workspace, &self.principal)
            .map_err(store_error)?;
        let validation = crate::cell_validation_result_with_catalog(&cell, &catalog, 0);
        if !validation.valid {
            return Err(ErrorData::invalid_request(
                "Cell failed preflight; nothing accepted",
                Some(json!({"code":"cell_plan_invalid","validation":validation})),
            ));
        }
        self.store
            .create_draft(
                &self.workspace,
                &self.principal,
                &id,
                &cell,
                &format!("{id}:draft"),
            )
            .map_err(store_error)?;
        let revision = self
            .store
            .publish(
                &self.workspace,
                &self.principal,
                &id,
                1,
                &format!("{id}:publish"),
            )
            .map_err(store_error)?;
        let update = target
            .map(|instance| {
                self.store.plan_update(
                    &self.workspace,
                    &self.principal,
                    CellUpdateTarget {
                        instance_id: instance.id,
                        expected_generation: instance.generation,
                        delete_data: request.delete_data,
                        delete_retained: request.delete_retained,
                    },
                    &revision,
                )
            })
            .transpose()
            .map_err(store_error)?;
        let preview = CellPreview {
            id,
            request_digest,
            name: request.name,
            cell: revision.cell,
            revision_digest: revision.digest,
            lock_digest: revision.lock.digest,
            update,
        };
        self.store
            .save_cell_preview(&self.workspace, &self.principal, &preview)
            .map_err(store_error)?;
        Ok(preview)
    }
}

fn invalid(message: impl Into<String>) -> ErrorData {
    coded_invalid_request("invalid_cell_input", message)
}

fn apply_patch(cell: &mut CellSpec, patch: &[CellPatch]) -> Result<(), ErrorData> {
    if !(1..=100).contains(&patch.len()) {
        return Err(invalid("patch must contain 1..=100 operations"));
    }
    for change in patch {
        match change {
            CellPatch::AddComponent { component } => {
                if cell.components.iter().any(|item| item.id == component.id) {
                    return Err(invalid(format!(
                        "Component {:?} already exists",
                        component.id
                    )));
                }
                cell.components.push(component.clone());
            }
            CellPatch::UpdateComponent { component } => {
                let current = cell
                    .components
                    .iter_mut()
                    .find(|item| item.id == component.id)
                    .ok_or_else(|| invalid(format!("Component {:?} is absent", component.id)))?;
                *current = component.clone();
            }
            CellPatch::RemoveComponent { id } => {
                let index = cell
                    .components
                    .iter()
                    .position(|item| item.id == *id)
                    .ok_or_else(|| invalid(format!("Component {id:?} is absent")))?;
                cell.components.remove(index);
            }
            CellPatch::AddLink { link } => {
                let link = LinkSpec::try_from(link.clone()).map_err(invalid)?;
                if cell.links.iter().any(|item| item.id == link.id) {
                    return Err(invalid(format!("Link {:?} already exists", link.id)));
                }
                cell.links.push(link);
            }
            CellPatch::RemoveLink { id } => {
                let index = cell
                    .links
                    .iter()
                    .position(|item| item.id == *id)
                    .ok_or_else(|| invalid(format!("Link {id:?} is absent")))?;
                cell.links.remove(index);
            }
            CellPatch::SetPolicy { policy } => cell.policy = policy.clone(),
        }
    }
    cell.components.sort_by(|a, b| a.id.cmp(&b.id));
    cell.links.sort();
    Ok(())
}

pub(super) fn receipt(preview: &CellPreview) -> Result<crate::CallToolResult, ErrorData> {
    let topology = crate::topology_summary(&preview.cell);
    let changes=preview.update.as_ref().map(|plan|json!({
        "added":plan.changes.added.len(),"removed":plan.changes.removed.len(),
        "restarted":plan.changes.restarted.len(),"unchanged":plan.changes.unchanged.len(),
        "delete_data":plan.target.delete_data,"delete_retained_count":plan.target.delete_retained.len(),
        "supported":plan.changes.unsupported.is_empty(),
    }));
    crate::developer_result(
        json!({"plan":{"id":preview.id,"digest":digest_json(preview)},"name":preview.name,
        "component_count":topology.component_count,"link_count":topology.link_count,"changes":changes,
        "backend_link_count":topology.backend_link_count,"bound_backend_link_count":topology.bound_backend_link_count,"topology_digest":topology.topology_digest,"warnings":topology.warnings,
        "expected_generation":preview.update.as_ref().map(|p|p.target.expected_generation),
        "expected_instance_key":preview.update.as_ref().map(|p|&p.instance_key),
        "revision_digest":preview.revision_digest,"lock_digest":preview.lock_digest,
        "cell_digest":digest_json(&preview.cell),"next_tool":"cell_up","read_tool":"cell_read"}),
    )
}
