//! Explicit network faults and bounded reachability observations.
use super::{
    Cells,
    submission::{OperationAdmission, invalid_operation},
};
use crate::Error;
use proofstorm_core::{Capability, CellOperation, OperationKind, OperationPhase};
use proofstorm_kube::{
    CellAction, NetworkHealAction, NetworkPartitionAction, ReachabilityOracleAction,
    component_ports,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkPartitionRequest {
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
    pub from_component: String,
    pub to_component: String,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkHealRequest {
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
    pub partition_operation_id: String,
    #[serde(skip)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkProbeRequest {
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
    pub from_component: String,
    pub to_component: String,
    /// Logical destination service, such as `http`, `rpc`, or `p2p`.
    pub service: String,
    #[serde(default = "default_probe_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_probe_attempts")]
    pub attempts: u32,
    #[serde(skip)]
    pub idempotency_key: String,
}

const fn default_probe_timeout_seconds() -> u32 {
    2
}

const fn default_probe_attempts() -> u32 {
    3
}

impl NetworkProbeRequest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.from_component == self.to_component {
            return Err(invalid_operation(
                "from_component and to_component must differ",
            ));
        }
        if !(1..=5).contains(&self.timeout_seconds) {
            return Err(invalid_operation("timeout_seconds must be between 1 and 5"));
        }
        if !(1..=5).contains(&self.attempts) {
            return Err(invalid_operation("attempts must be between 1 and 5"));
        }
        Ok(())
    }
}

impl Cells {
    pub async fn network_partition(
        &self,
        request: NetworkPartitionRequest,
    ) -> Result<CellOperation, Error> {
        self.authorize(&[Capability::NetworkPartition])?;
        if request.from_component == request.to_component {
            return Err(invalid_operation("partition endpoints must be distinct"));
        }
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::NetworkPartition,
        )?;
        for component in [&request.from_component, &request.to_component] {
            if !revision
                .cell
                .components
                .iter()
                .any(|item| item.id == *component)
            {
                return Err(invalid_operation(&format!(
                    "component {component:?} is not part of this cell revision"
                )));
            }
        }
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::NetworkPartition,
                capability: Capability::NetworkPartition,
            },
            &request,
        )?;
        self.submit_action(
            &instance,
            operation,
            CellAction::NetworkPartition(NetworkPartitionAction {
                from_component: request.from_component,
                to_component: request.to_component,
            }),
        )
        .await
    }
    pub async fn network_heal(&self, request: NetworkHealRequest) -> Result<CellOperation, Error> {
        self.authorize(&[Capability::NetworkHeal])?;
        let mut request = request;
        request.experiment_id = self.store.operation_run_id(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.experiment_id,
            Capability::NetworkHeal,
        )?;
        let (instance, _) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::NetworkHeal,
        )?;
        let partition = self.store.operation(
            &self.workspace,
            &self.principal,
            &request.partition_operation_id,
        )?;
        if partition.kind != OperationKind::NetworkPartition
            || partition.instance_id != request.instance_id
            || partition.experiment_id != request.experiment_id
            || partition.phase != OperationPhase::Succeeded
        {
            return Err(invalid_operation(
                "partition operation must be succeeded and belong to the same instance and experiment",
            ));
        }
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::NetworkHeal,
                capability: Capability::NetworkHeal,
            },
            &request,
        )?;
        self.submit_action(
            &instance,
            operation,
            CellAction::NetworkHeal(NetworkHealAction {
                partition_operation_id: request.partition_operation_id,
            }),
        )
        .await
    }
    pub async fn network_probe(
        &self,
        request: NetworkProbeRequest,
    ) -> Result<CellOperation, Error> {
        self.authorize(&[Capability::OracleRun])?;
        request.validate()?;
        let (instance, revision) = self.store.operation_context_for(
            &self.workspace,
            &self.principal,
            &request.instance_id,
            &request.operation_id,
            Capability::OracleRun,
        )?;
        if !revision
            .cell
            .components
            .iter()
            .any(|component| component.id == request.from_component)
        {
            return Err(invalid_operation(&format!(
                "component {:?} is not part of this cell revision",
                request.from_component
            )));
        }
        let destination = revision
            .cell
            .components
            .iter()
            .find(|component| component.id == request.to_component)
            .ok_or_else(|| {
                invalid_operation(&format!(
                    "component {:?} is not part of this cell revision",
                    request.to_component
                ))
            })?;
        if !component_ports(destination).contains_key(&request.service) {
            return Err(invalid_operation(&format!(
                "component {:?} does not advertise logical service {:?}",
                request.to_component, request.service
            )));
        }
        let operation = self.admit_action(
            &instance,
            OperationAdmission {
                experiment_id: &request.experiment_id,
                session_id: &request.session_id,
                operation_id: &request.operation_id,
                idempotency_key: &request.idempotency_key,
                kind: OperationKind::ReachabilityOracle,
                capability: Capability::OracleRun,
            },
            &request,
        )?;
        self.submit_action(
            &instance,
            operation,
            CellAction::ReachabilityOracle(ReachabilityOracleAction {
                from_component: request.from_component,
                to_component: request.to_component,
                service: request.service,
                timeout_seconds: request.timeout_seconds,
                attempts: request.attempts,
            }),
        )
        .await
    }
}
