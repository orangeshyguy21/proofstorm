//! Bounded reads of complete desired configurations and immutable previews.
use crate::{
    CallToolResult, CellReadResponse, ErrorData, ProofstormMcp, coded_invalid_request,
    developer_result, read_query, store_error,
};
use proofstorm_core::{Capability, digest_json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellTarget {
    /// Cell name or canonical instance ID. Supply name or `plan_id`, never both.
    #[serde(default)]
    pub name: Option<String>,
    /// Immutable preview ID returned by `cell_plan`.
    #[serde(default)]
    pub plan_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellDocumentSection {
    #[default]
    Configuration,
    Plan,
    Lock,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellReadRequest {
    #[serde(flatten)]
    pub target: CellTarget,
    /// Configuration (default), complete immutable preview plan, or resolved image lock.
    #[serde(default)]
    pub document: CellDocumentSection,
    /// RFC 6901 path in the cell document, e.g. /components/0/config or /policy.
    #[serde(default)]
    pub pointer: String,
    /// Bind to `document_digest` from a prior read, or `cell_digest` from configuration search.
    #[serde(default)]
    pub expected_digest: Option<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Return immediate child paths and sizes; useful before selecting a large object.
    #[serde(default)]
    pub scan: bool,
}
fn default_limit() -> usize {
    100
}

impl ProofstormMcp {
    pub(super) fn read_cell_document(
        &self,
        request: &CellReadRequest,
    ) -> Result<CallToolResult, ErrorData> {
        let (document, revision_digest) = self.cell_document_snapshot(&request.target)?;
        let value = match request.document {
            CellDocumentSection::Configuration => json!(document.cell),
            CellDocumentSection::Plan => {
                let id = request.target.plan_id.as_ref().ok_or_else(|| {
                    coded_invalid_request("cell_read_target", "document=plan requires plan_id")
                })?;
                json!(
                    self.store
                        .cell_preview(&self.workspace, &self.principal, id)
                        .map_err(store_error)?
                        .ok_or_else(|| coded_invalid_request(
                            "cell_plan_missing",
                            "Preview not found"
                        ))?
                )
            }
            CellDocumentSection::Lock => {
                json!(
                    self.store
                        .revision(&self.workspace, &self.principal, &revision_digest)
                        .map_err(store_error)?
                        .lock
                )
            }
        };
        read_value(&document, &value, request)
    }
    pub(super) fn cell_document(&self, target: &CellTarget) -> Result<CellReadResponse, ErrorData> {
        self.cell_document_snapshot(target)
            .map(|(document, _)| document)
    }
    fn cell_document_snapshot(
        &self,
        target: &CellTarget,
    ) -> Result<(CellReadResponse, String), ErrorData> {
        self.authorize(Capability::CellRead)?;
        match (&target.name, &target.plan_id) {
            (Some(name), None) => {
                let id = self
                    .store
                    .resolve_cell_reference_for(
                        &self.workspace,
                        &self.principal,
                        name,
                        Capability::CellRead,
                    )
                    .map_err(store_error)?;
                let (instance, revision) = self
                    .store
                    .operation_context(&self.workspace, &self.principal, &id, Capability::CellRead)
                    .map_err(store_error)?;
                Ok((
                    CellReadResponse {
                        id: instance.id,
                        workspace_id: self.workspace.clone(),
                        version: instance.generation,
                        cell: revision.cell,
                    },
                    instance.revision_digest,
                ))
            }
            (None, Some(id)) => {
                let preview = self
                    .store
                    .cell_preview(&self.workspace, &self.principal, id)
                    .map_err(store_error)?
                    .ok_or_else(|| {
                        coded_invalid_request(
                            "cell_plan_missing",
                            "Preview not found for this actor",
                        )
                    })?;
                Ok((
                    CellReadResponse {
                        id: preview.id,
                        workspace_id: self.workspace.clone(),
                        version: 1,
                        cell: preview.cell,
                    },
                    preview.revision_digest,
                ))
            }
            _ => Err(coded_invalid_request(
                "cell_read_target",
                "Supply name or plan_id, not both",
            )),
        }
    }
}

#[cfg(test)]
pub(super) fn read(
    document: &CellReadResponse,
    request: &CellReadRequest,
) -> Result<CallToolResult, ErrorData> {
    let value = json!(document.cell);
    read_value(document, &value, request)
}

fn read_value(
    document: &CellReadResponse,
    value: &Value,
    request: &CellReadRequest,
) -> Result<CallToolResult, ErrorData> {
    crate::activity_search::validate_pointer(&request.pointer)?;
    if !(1..=4000).contains(&request.limit) {
        return Err(coded_invalid_request(
            "cell_read_limit",
            "limit must be 1..=4000",
        ));
    }
    let digest = if request.document == CellDocumentSection::Configuration {
        digest_json(&document.cell)
    } else {
        digest_json(&value)
    };
    if request
        .expected_digest
        .as_ref()
        .is_some_and(|expected| *expected != digest)
    {
        return Err(coded_invalid_request(
            "cell_read_changed",
            "Desired configuration changed; search again or omit expected_digest for a fresh observation",
        ));
    }
    let selected = value.pointer(&request.pointer).ok_or_else(|| {
        coded_invalid_request(
            "cell_read_pointer_missing",
            "Pointer is absent; scan its parent for available paths",
        )
    })?;
    let mut scan = request.scan;
    if selected.is_object()
        && read_query::wire_size(selected)? > crate::MAX_AGENT_RESPONSE_BYTES / 2
    {
        scan = true;
    }
    let total = match selected {
        Value::String(s) => Some(s.chars().count()),
        Value::Array(a) => Some(a.len()),
        Value::Object(o) if scan => Some(o.len()),
        _ => None,
    };
    if total.map_or(request.offset != 0, |n| request.offset > n) {
        return Err(coded_invalid_request(
            "cell_read_offset",
            "offset exceeds the selected value",
        ));
    }
    let mut length = total.map_or(0, |n| (n - request.offset).min(request.limit));
    loop {
        let (body, entries) = if scan {
            let pairs: Vec<(String, &Value)> = match selected {
                Value::Object(o) => o
                    .iter()
                    .skip(request.offset)
                    .take(length)
                    .map(|(key, value)| (key.clone(), value))
                    .collect(),
                Value::Array(a) => a
                    .iter()
                    .enumerate()
                    .skip(request.offset)
                    .take(length)
                    .map(|(index, value)| (index.to_string(), value))
                    .collect(),
                _ => {
                    return Err(coded_invalid_request(
                        "cell_read_scan",
                        "scan requires an object or array",
                    ));
                }
            };
            (Value::Null,json!(pairs.into_iter().map(|(key,value)|json!({"path":format!("{}/{}",request.pointer,key.replace('~',"~0").replace('/',"~1")),"bytes":value.to_string().len()})).collect::<Vec<_>>()))
        } else {
            (
                match selected {
                    Value::String(s) => json!(
                        s.chars()
                            .skip(request.offset)
                            .take(length)
                            .collect::<String>()
                    ),
                    Value::Array(a) => json!(&a[request.offset..request.offset + length]),
                    _ => selected.clone(),
                },
                Value::Null,
            )
        };
        let result = json!({"id":document.id,"version":document.version,"cell_digest":digest_json(&document.cell),"document_digest":digest,"document":request.document,"pointer":request.pointer,"value":body,"scan":scan,"entries":entries,
            "offset":request.offset,"next_offset":total.filter(|n|request.offset+length<*n).map(|_|request.offset+length),"total_length":total,
            "unit":if selected.is_string(){"characters"}else{"items"}});
        if read_query::wire_size(&result)? <= crate::MAX_AGENT_RESPONSE_BYTES {
            return developer_result(result);
        }
        if length <= 1 {
            return Err(coded_invalid_request(
                "cell_read_value_too_large",
                "Select a deeper pointer or use scan=true to locate the large array item's fields",
            ));
        }
        length /= 2;
    }
}
