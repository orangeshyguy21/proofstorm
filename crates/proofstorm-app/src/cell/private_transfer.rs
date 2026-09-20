//! Metadata-only private custody; native commands own export and import.
use super::{
    Cells,
    component_reference::component_image_any,
    submission::{OperationAdmission, invalid_operation},
};
use crate::Error;
use proofstorm_core::{Capability, CellOperation, ComponentKind, OperationKind, PublishedRevision};
use proofstorm_kube::CellAction;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Method-specific public input. Keep the flat Kubernetes action an internal wire type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "transferMethod", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateTransferInput {
    Prepare {
        component: String,
        #[serde(rename = "destinationComponent")]
        destination_component: String,
        /// Reserve before export: 1..=1048576 bytes; CDK recipients allow at most 65536.
        #[serde(rename = "maximumBytes")]
        #[schemars(range(min = 1, max = 1_048_576))]
        maximum_bytes: u32,
    },
    Handoff {
        component: String,
        reference: String,
        #[serde(rename = "recipientGrantId")]
        recipient_grant_id: String,
    },
    Status {
        component: String,
        reference: String,
    },
    Deliver {
        component: String,
        reference: String,
    },
    Release {
        component: String,
        reference: String,
    },
}

impl PrivateTransferInput {
    pub fn validate(&self) -> Result<(), Error> {
        self.action().map(|_| ())
    }

    pub(super) fn action(&self) -> Result<proofstorm_kube::PrivateTransferAction, Error> {
        use proofstorm_core::private_io::MAX_PRIVATE_BYTES;
        use proofstorm_kube::{PrivateTransferAction, TransferMethod};
        let recipient_grant_id = if let Self::Handoff {
            recipient_grant_id, ..
        } = self
        {
            if recipient_grant_id.trim().is_empty() {
                return Err(invalid_operation(
                    "handoff.recipientGrantId must be nonempty",
                ));
            }
            Some(recipient_grant_id.clone())
        } else {
            None
        };
        let (method, component, destination, reference, maximum) = match self {
            Self::Prepare {
                component,
                destination_component,
                maximum_bytes,
            } => {
                if !(1..=MAX_PRIVATE_BYTES).contains(maximum_bytes) {
                    return Err(invalid_operation(
                        "prepare.maximumBytes must be between 1 and 1048576; no operation was created",
                    ));
                }
                if component == destination_component {
                    return Err(invalid_operation(
                        "prepare.destinationComponent must differ from component; no operation was created",
                    ));
                }
                (
                    TransferMethod::Prepare,
                    component,
                    Some(destination_component.clone()),
                    None,
                    Some(*maximum_bytes),
                )
            }
            Self::Handoff {
                component,
                reference,
                ..
            } => (
                TransferMethod::Handoff,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
            Self::Status {
                component,
                reference,
            } => (
                TransferMethod::Status,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
            Self::Deliver {
                component,
                reference,
            } => (
                TransferMethod::Deliver,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
            Self::Release {
                component,
                reference,
            } => (
                TransferMethod::Release,
                component,
                None,
                Some(reference.clone()),
                None,
            ),
        };
        if component.trim().is_empty()
            || destination.as_ref().is_some_and(|id| id.trim().is_empty())
            || reference.as_ref().is_some_and(|id| id.trim().is_empty())
        {
            return Err(invalid_operation(
                "private transfer component, destinationComponent and reference must be nonempty when required; no operation was created",
            ));
        }
        Ok(PrivateTransferAction {
            recipient_grant_id,
            transfer_method: method,
            component: component.clone(),
            destination_component: destination,
            reference,
            maximum_bytes: maximum,
        })
    }
}

pub(super) fn validate_private_transfer_endpoints(
    transfer: &proofstorm_kube::PrivateTransferAction,
    revision: &PublishedRevision,
) -> Result<(), Error> {
    for id in std::iter::once(&transfer.component).chain(transfer.destination_component.iter()) {
        component_image_any(revision, id, ComponentKind::Wallet)?;
    }
    if let Some(destination) = &transfer.destination_component {
        let is_cdk = revision
            .cell
            .components
            .iter()
            .any(|c| &c.id == destination && c.implementation == "cdk-cli-wallet");
        if is_cdk
            && transfer
                .maximum_bytes
                .is_some_and(|maximum| maximum > proofstorm_core::private_io::MAX_PRIVATE_ARG_BYTES)
        {
            return Err(invalid_operation(
                "prepare.maximumBytes must be at most 65536 for a CDK CLI recipient's native argv input; no operation was created",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateTransferRequest {
    #[serde(rename = "name")]
    pub instance_id: String,
    /// Optional; defaults to this actor's cell run.
    #[serde(default)]
    #[serde(rename = "run_id")]
    pub experiment_id: String,
    #[serde(default)]
    #[serde(skip)]
    pub session_id: String,
    #[serde(rename = "request_id")]
    pub operation_id: String,
    pub transfer: PrivateTransferInput,
    #[serde(skip)]
    pub idempotency_key: String,
}

impl Cells {
    pub async fn private_transfer(
        &self,
        request: PrivateTransferRequest,
    ) -> Result<CellOperation, Error> {
        self.authorize(&[Capability::ComponentExecLive])?;
        let transfer = request.transfer.action()?;

        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::ComponentExecLive,
        )?;
        validate_private_transfer_endpoints(&transfer, &revision)?;
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::PrivateTransfer,
                capability: Capability::ComponentExecLive,
            },
            &request,
        )?;
        self.submit_action(&instance, operation, CellAction::PrivateTransfer(transfer))
            .await
    }
}
