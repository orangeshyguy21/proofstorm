use k8s_openapi::api::batch::v1::Job;
use kube::ResourceExt;
use proofstorm_core::{
    Capability, ComponentConditionReason, ComponentConditionState, ComponentConditionType,
    ComponentKind, ComponentPlanContract, ComponentStatus, EffectiveComponentConfig,
    ExecutionStorageSource, OperationClass, ReadinessPrerequisite,
};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    AuthenticationConformanceAction, AuthenticationProtectedSpendAction,
    AuthenticationReplayAction, BootstrapLiquidityAction, ChannelCloseAction, ChannelOpenAction,
    ChannelPolicySetAction, ChannelRebalanceAction, ComponentForensicsAction,
    ConservationOracleAction, LabAction, PeerConnectAction, PeerDisconnectAction, ProofstormLab,
    ProofstormLabAction, ReachabilityOracleAction, WalletBalanceAction, WalletFundAction,
    WalletInitializeAction, WalletInvoiceAction, WalletMeltQuoteRefreshAction, WalletPayAction,
    WalletQuoteClaimAction, WalletRoundTripAction, component_ports, instance_namespace,
};

const REACHABILITY_PROBE_IMAGE: &str = "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662";

/// Fixed driver for the secret-bearing OIDC/CAT/BAT baseline. It writes only
/// an allowlisted result to the termination log.
const AUTHENTICATION_CONFORMANCE_DRIVER: &str =
    include_str!("../drivers/authentication_conformance.py");
const AUTHENTICATION_PROTECTED_SPEND_DRIVER: &str =
    include_str!("../drivers/authentication_protected_spend.py");
const AUTHENTICATION_REPLAY_DRIVER: &str = include_str!("../drivers/authentication_replay.py");

pub struct BootstrapJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub chain: &'a str,
    pub mint_lightning: &'a str,
    pub payer_lightning: &'a str,
    pub bitcoin_image: &'a str,
    pub lnd_image: &'a str,
    pub funding_sat: u64,
    pub channel_sat: u64,
    pub push_sat: u64,
}

pub struct WalletRoundTripJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub payer_lightning: &'a str,
    pub wallet_image: &'a str,
    pub lnd_image: &'a str,
    pub amount_sat: u64,
    pub tolerance_sat: u64,
}

pub struct ConservationOracleJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub wallet_image: &'a str,
    pub baseline_operation_id: &'a str,
    pub treatment_operation_id: &'a str,
    pub expected_sat: u64,
    pub tolerance_sat: u64,
}

pub struct PeerConnectJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub from_lightning: &'a str,
    pub to_lightning: &'a str,
    pub from_adapter: LightningAdapter,
    pub from_image: &'a str,
    pub to_adapter: LightningAdapter,
    pub to_image: &'a str,
}

pub struct PeerDisconnectJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub from_lightning: &'a str,
    pub to_lightning: &'a str,
    pub from_adapter: LightningAdapter,
    pub from_image: &'a str,
    pub to_adapter: LightningAdapter,
    pub to_image: &'a str,
}

pub struct ChannelOpenJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub chain: &'a str,
    pub from_lightning: &'a str,
    pub to_lightning: &'a str,
    pub bitcoin_image: &'a str,
    pub from_adapter: LightningAdapter,
    pub from_image: &'a str,
    pub to_adapter: LightningAdapter,
    pub to_image: &'a str,
    pub channel_sat: u64,
    pub push_sat: u64,
}

pub struct ChannelPolicySetJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub from_lightning: &'a str,
    pub to_lightning: &'a str,
    pub from_adapter: LightningAdapter,
    pub from_image: &'a str,
    pub to_adapter: LightningAdapter,
    pub to_image: &'a str,
    pub base_fee_msat: u64,
    pub fee_rate_ppm: u32,
}

pub struct ChannelCloseJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub chain: &'a str,
    pub from_lightning: &'a str,
    pub to_lightning: &'a str,
    pub channel_id: &'a str,
    pub bitcoin_image: &'a str,
    pub from_adapter: LightningAdapter,
    pub from_image: &'a str,
    pub to_adapter: LightningAdapter,
    pub to_image: &'a str,
    pub force: bool,
}

pub struct ChannelRebalanceJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub lightning: &'a str,
    pub lightning_image: &'a str,
    pub outgoing_channel_id: &'a str,
    pub incoming_channel_id: &'a str,
    pub amount_sat: u64,
    pub max_fee_sat: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightningAdapter {
    Lnd,
    Cln,
}

impl LightningAdapter {
    fn from_implementation(implementation: &str) -> Option<Self> {
        match implementation {
            "lnd" => Some(Self::Lnd),
            "cln" => Some(Self::Cln),
            _ => None,
        }
    }
}

pub struct WalletJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub wallet_image: &'a str,
}

pub struct WalletFundJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub payer_lightning: &'a str,
    pub wallet_image: &'a str,
    pub lightning_image: &'a str,
    pub amount_sat: u64,
}

const WALLET_QUOTE_DRIVER: &str = include_str!("../drivers/wallet_quote_driver.py");

pub struct WalletInvoiceJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub wallet_image: &'a str,
    pub amount_sat: u64,
    pub timeout_seconds: u32,
}

pub struct WalletPayJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub recipient_wallet: &'a str,
    pub recipient_mint: &'a str,
    pub mint_quote_id: &'a str,
    pub wallet_image: &'a str,
}

pub struct WalletQuoteClaimJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub mint_quote_id: &'a str,
    pub wallet_image: &'a str,
    pub timeout_seconds: u32,
}

pub struct WalletMeltQuoteRefreshJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub wallet: &'a str,
    pub mint: &'a str,
    pub melt_quote_id: &'a str,
    pub wallet_image: &'a str,
    pub timeout_seconds: u32,
}

pub struct AuthenticationConformanceJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub mint: &'a str,
    pub identity_provider: &'a str,
    pub mint_image: &'a str,
}

pub struct AuthenticationProtectedSpendJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub mint: &'a str,
    pub identity_provider: &'a str,
    pub mint_image: &'a str,
}

pub struct AuthenticationReplayJobSpec<'a> {
    pub resource_name: &'a str,
    pub instance_key: &'a str,
    pub mint: &'a str,
    pub identity_provider: &'a str,
    pub mint_image: &'a str,
    pub session_secret: &'a str,
    pub source_operation_id: &'a str,
}

#[derive(Debug, Error)]
pub enum ActionRenderError {
    #[error("action identity does not match referenced lab: {0}")]
    Identity(&'static str),
    #[error("action capability is invalid for its typed request")]
    Capability,
    #[error("typed action request is outside bounded policy: {0}")]
    Bounds(&'static str),
    #[error("component {component:?} must use installed {implementation:?} {kind:?} adapter")]
    Component {
        component: String,
        implementation: &'static str,
        kind: ComponentKind,
    },
    #[error("component {0:?} has no immutable lock entry")]
    MissingLock(String),
    #[error("component {0:?} is not present in the immutable lab revision")]
    UnknownComponent(String),
    #[error("component {component:?} does not advertise logical service {service:?}")]
    UnknownService { component: String, service: String },
    #[error("component {component:?} uses unsupported action adapter {adapter:?}")]
    UnsupportedAdapter { component: String, adapter: String },
    #[error("component plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("typed action rendered an invalid Kubernetes Job: {0}")]
    InvalidResource(#[from] serde_json::Error),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ActionAdmissionError {
    #[error("action identity is invalid: {0}")]
    Identity(&'static str),
    #[error("component plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("component {component:?} has no {operation:?} admission contract")]
    MissingContract {
        component: String,
        operation: OperationClass,
    },
    #[error("component {component:?} does not satisfy {prerequisite:?} for {operation:?}")]
    PrerequisiteUnsatisfied {
        component: String,
        operation: OperationClass,
        prerequisite: ReadinessPrerequisite,
        condition: Option<ComponentConditionType>,
        state: Option<ComponentConditionState>,
        reason: Option<ComponentConditionReason>,
    },
}

impl ActionAdmissionError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Identity(_) => "action_identity_invalid",
            Self::InvalidPlan(_) => "action_plan_invalid",
            Self::MissingContract { .. } => "action_admission_contract_missing",
            Self::PrerequisiteUnsatisfied { .. } => "action_prerequisite_unsatisfied",
        }
    }
}

/// Evaluate a typed action against backend-declared readiness prerequisites.
///
/// Immutable execution contexts and target descriptors remain usable when a
/// component protocol is unhealthy. Runtime conditions are required only when
/// the selected operation contract names their prerequisite.
///
/// # Errors
///
/// Returns a stable failure when identity, compiled admission contracts, or an
/// applicable runtime readiness prerequisite is unavailable.
pub fn evaluate_action_admission(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
) -> Result<(), ActionAdmissionError> {
    validate_action_identity(action, lab).map_err(|error| match error {
        ActionRenderError::Identity(field) => ActionAdmissionError::Identity(field),
        _ => ActionAdmissionError::InvalidPlan(error.to_string()),
    })?;
    let plans = crate::compile_component_plans(
        &lab.spec.instance_key,
        &lab.spec.revision_digest,
        &lab.spec.lab,
        &lab.spec.lock,
    )
    .map_err(|error| ActionAdmissionError::InvalidPlan(error.to_string()))?;
    let statuses = lab
        .status
        .as_ref()
        .filter(|status| status.observed_revision_digest == lab.spec.revision_digest)
        .map_or(&[][..], |status| status.components.as_slice());
    let protocol_lease_current = lab
        .annotations()
        .get(crate::PROTOCOL_PROBER_LEASE_ANNOTATION)
        .is_some_and(|lease| {
            lease != "inactive"
                && lab.status.as_ref().is_some_and(|status| {
                    status.observed_protocol_probe_lease.as_ref() == Some(lease)
                })
        });

    for (component, operation) in action_participants(&action.spec.action) {
        let plan = require_admission_plan(&plans, component, operation)?;
        let contract = plan
            .operation_admission
            .iter()
            .find(|contract| contract.operation == operation)
            .ok_or_else(|| ActionAdmissionError::MissingContract {
                component: component.to_owned(),
                operation,
            })?;
        for prerequisite in &contract.prerequisites {
            evaluate_prerequisite(
                plan,
                statuses,
                operation,
                *prerequisite,
                protocol_lease_current,
            )?;
        }
    }

    if let Some((executor, target)) = action_execution_target(&action.spec.action)
        && executor != target
    {
        let target = require_admission_plan(&plans, target, OperationClass::NativeExec)?;
        evaluate_prerequisite(
            target,
            statuses,
            OperationClass::NativeExec,
            ReadinessPrerequisite::TargetDescriptor,
            protocol_lease_current,
        )?;
    }
    Ok(())
}

fn require_admission_plan<'a>(
    plans: &'a [ComponentPlanContract],
    component: &str,
    operation: OperationClass,
) -> Result<&'a ComponentPlanContract, ActionAdmissionError> {
    plans
        .iter()
        .find(|plan| plan.component_id == component)
        .ok_or_else(|| ActionAdmissionError::PrerequisiteUnsatisfied {
            component: component.to_owned(),
            operation,
            prerequisite: ReadinessPrerequisite::AcceptedIdentity,
            condition: None,
            state: None,
            reason: None,
        })
}

fn evaluate_prerequisite(
    plan: &ComponentPlanContract,
    statuses: &[ComponentStatus],
    operation: OperationClass,
    prerequisite: ReadinessPrerequisite,
    protocol_lease_current: bool,
) -> Result<(), ActionAdmissionError> {
    let condition_type = match prerequisite {
        ReadinessPrerequisite::Storage => Some(ComponentConditionType::StorageReady),
        ReadinessPrerequisite::Dependencies => Some(ComponentConditionType::DependenciesReady),
        ReadinessPrerequisite::Protocol => Some(ComponentConditionType::ProtocolReady),
        ReadinessPrerequisite::AcceptedIdentity
        | ReadinessPrerequisite::ExecutionContext
        | ReadinessPrerequisite::TargetDescriptor
        | ReadinessPrerequisite::FaultIdentity => return Ok(()),
        ReadinessPrerequisite::WorkloadIdentity => {
            return evaluate_workload_identity(plan, statuses, operation);
        }
    };
    let condition_type = condition_type.expect("runtime prerequisite has a condition");
    if !plan.applicable_conditions.contains(&condition_type) {
        return Ok(());
    }
    if matches!(
        prerequisite,
        ReadinessPrerequisite::Dependencies | ReadinessPrerequisite::Protocol
    ) && !protocol_lease_current
    {
        return Err(unsatisfied(
            plan,
            operation,
            prerequisite,
            Some(condition_type),
            None,
        ));
    }
    let status = current_component_status(plan, statuses);
    let condition = status.and_then(|status| {
        status
            .conditions
            .iter()
            .find(|condition| condition.condition_type == condition_type)
    });
    if condition.is_some_and(|condition| condition.state == ComponentConditionState::True) {
        return Ok(());
    }
    Err(unsatisfied(
        plan,
        operation,
        prerequisite,
        Some(condition_type),
        condition,
    ))
}

fn evaluate_workload_identity(
    plan: &ComponentPlanContract,
    statuses: &[ComponentStatus],
    operation: OperationClass,
) -> Result<(), ActionAdmissionError> {
    let condition = current_component_status(plan, statuses).and_then(|status| {
        status
            .conditions
            .iter()
            .find(|condition| condition.condition_type == ComponentConditionType::WorkloadReady)
    });
    if condition.is_some_and(|condition| {
        !matches!(
            condition.reason,
            ComponentConditionReason::NotObserved | ComponentConditionReason::StaleRevision
        )
    }) {
        return Ok(());
    }
    Err(unsatisfied(
        plan,
        operation,
        ReadinessPrerequisite::WorkloadIdentity,
        Some(ComponentConditionType::WorkloadReady),
        condition,
    ))
}

fn current_component_status<'a>(
    plan: &ComponentPlanContract,
    statuses: &'a [ComponentStatus],
) -> Option<&'a ComponentStatus> {
    statuses.iter().find(|status| {
        status.id == plan.component_id
            && status.observed_revision_digest == plan.revision_digest
            && status.observed_rollout_digest == plan.rollout_digest
    })
}

fn unsatisfied(
    plan: &ComponentPlanContract,
    operation: OperationClass,
    prerequisite: ReadinessPrerequisite,
    condition_type: Option<ComponentConditionType>,
    condition: Option<&proofstorm_core::ComponentCondition>,
) -> ActionAdmissionError {
    ActionAdmissionError::PrerequisiteUnsatisfied {
        component: plan.component_id.clone(),
        operation,
        prerequisite,
        condition: condition_type,
        state: condition.map(|condition| condition.state),
        reason: condition.map(|condition| condition.reason),
    }
}

fn action_execution_target(action: &LabAction) -> Option<(&str, &str)> {
    match action {
        LabAction::ComponentForensics(request) => {
            Some((&request.component, &request.target_component))
        }
        LabAction::ReachabilityOracle(request) => {
            Some((&request.from_component, &request.to_component))
        }
        _ => None,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive action-to-participant contract is clearest in one match"
)]
fn action_participants(action: &LabAction) -> Vec<(&str, OperationClass)> {
    use OperationClass as Operation;
    match action {
        LabAction::NodeStart(request) => vec![(&request.component, Operation::Start)],
        LabAction::NodeStop(request) => vec![(&request.component, Operation::Stop)],
        LabAction::NodeRestart(request) | LabAction::ComponentRestart(request) => {
            vec![(&request.component, Operation::Restart)]
        }
        LabAction::BootstrapLiquidity(request) => vec![
            (&request.chain, Operation::PeerChannelMutation),
            (&request.mint_lightning, Operation::PeerChannelMutation),
            (&request.payer_lightning, Operation::PeerChannelMutation),
        ],
        LabAction::PeerConnect(request) => vec![
            (&request.from_lightning, Operation::PeerChannelMutation),
            (&request.to_lightning, Operation::PeerChannelMutation),
        ],
        LabAction::PeerDisconnect(request) => vec![
            (&request.from_lightning, Operation::PeerChannelMutation),
            (&request.to_lightning, Operation::PeerChannelMutation),
        ],
        LabAction::ChannelOpen(request) => vec![
            (&request.chain, Operation::PeerChannelMutation),
            (&request.from_lightning, Operation::PeerChannelMutation),
            (&request.to_lightning, Operation::PeerChannelMutation),
        ],
        LabAction::ChannelPolicySet(request) => vec![
            (&request.from_lightning, Operation::PeerChannelMutation),
            (&request.to_lightning, Operation::PeerChannelMutation),
        ],
        LabAction::ChannelClose(request) | LabAction::ChannelForceClose(request) => vec![
            (&request.chain, Operation::PeerChannelMutation),
            (&request.from_lightning, Operation::PeerChannelMutation),
            (&request.to_lightning, Operation::PeerChannelMutation),
        ],
        LabAction::ChannelRebalance(request) => {
            vec![(&request.lightning, Operation::PeerChannelMutation)]
        }
        LabAction::NetworkPartition(request) => vec![
            (&request.from_component, Operation::Inspect),
            (&request.to_component, Operation::Inspect),
        ],
        // Neither healing a fault nor reading a log has a component readiness
        // prerequisite. For a log that is deliberate: an unready,
        // crash-looping, or stopped component is when its log matters most.
        LabAction::NetworkHeal(_) | LabAction::ComponentLogs(_) => Vec::new(),
        LabAction::WalletInitialize(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
        ],
        LabAction::WalletBalance(request) => vec![
            (&request.wallet, Operation::Inspect),
            (&request.mint, Operation::Inspect),
        ],
        LabAction::WalletFund(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
            (&request.payer_lightning, Operation::WalletPayment),
        ],
        LabAction::WalletInvoice(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
        ],
        LabAction::WalletPay(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
            (&request.recipient_wallet, Operation::WalletPayment),
            (&request.recipient_mint, Operation::WalletPayment),
        ],
        LabAction::WalletQuoteClaim(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
        ],
        LabAction::WalletMeltQuoteRefresh(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
        ],
        LabAction::WalletRoundTrip(request) => vec![
            (&request.wallet, Operation::WalletPayment),
            (&request.mint, Operation::WalletPayment),
            (&request.payer_lightning, Operation::WalletPayment),
        ],
        LabAction::ConservationOracle(request) => vec![
            (&request.wallet, Operation::Inspect),
            (&request.mint, Operation::Inspect),
        ],
        LabAction::ReachabilityOracle(request) => {
            vec![(&request.from_component, Operation::NativeExec)]
        }
        LabAction::ComponentForensics(request) => {
            vec![(&request.component, Operation::NativeExec)]
        }
        LabAction::ComponentExecLive(request) => {
            vec![(&request.component, Operation::NativeExec)]
        }
        LabAction::AuthenticationConformance(request) => vec![
            (&request.mint, Operation::Authentication),
            (&request.identity_provider, Operation::Authentication),
        ],
        LabAction::AuthenticationProtectedSpend(request) => vec![
            (&request.mint, Operation::Authentication),
            (&request.identity_provider, Operation::Authentication),
        ],
        LabAction::AuthenticationReplay(request) => vec![
            (&request.mint, Operation::Authentication),
            (&request.identity_provider, Operation::Authentication),
        ],
    }
}

#[must_use]
pub const fn action_result_container(action: &LabAction) -> &'static str {
    match action {
        LabAction::NodeStart(_)
        | LabAction::NodeStop(_)
        | LabAction::NodeRestart(_)
        | LabAction::ComponentRestart(_)
        | LabAction::NetworkPartition(_)
        | LabAction::NetworkHeal(_)
        | LabAction::BootstrapLiquidity(_)
        | LabAction::PeerConnect(_)
        | LabAction::PeerDisconnect(_)
        | LabAction::ChannelOpen(_)
        | LabAction::ChannelPolicySet(_)
        | LabAction::ChannelClose(_)
        | LabAction::ChannelForceClose(_)
        | LabAction::ChannelRebalance(_) => "result",
        LabAction::WalletInitialize(_)
        | LabAction::WalletBalance(_)
        | LabAction::WalletFund(_)
        | LabAction::WalletInvoice(_)
        | LabAction::WalletPay(_)
        | LabAction::WalletQuoteClaim(_)
        | LabAction::WalletMeltQuoteRefresh(_)
        | LabAction::WalletRoundTrip(_) => "wallet",
        LabAction::ConservationOracle(_) | LabAction::ReachabilityOracle(_) => "oracle",
        LabAction::ComponentForensics(_) => "forensics",
        LabAction::AuthenticationConformance(_)
        | LabAction::AuthenticationProtectedSpend(_)
        | LabAction::AuthenticationReplay(_) => "authentication",
        // Never rendered as a Job; the controller reads the log itself.
        LabAction::ComponentLogs(_) | LabAction::ComponentExecLive(_) => "",
    }
}

/// Validate a typed action against its immutable lab and render its deterministic Job.
///
/// # Errors
///
/// Returns an error for identity drift, unsupported components, values outside
/// policy bounds, or an invalid internal Kubernetes resource.
pub fn render_lab_action_job(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
) -> Result<Job, ActionRenderError> {
    validate_action_identity(action, lab)?;
    let mut job = match &action.spec.action {
        LabAction::NodeStart(_)
        | LabAction::NodeStop(_)
        | LabAction::NodeRestart(_)
        | LabAction::ComponentRestart(_)
        | LabAction::ComponentExecLive(_)
        | LabAction::NetworkPartition(_)
        | LabAction::NetworkHeal(_) => {
            return Err(ActionRenderError::Bounds(
                "direct controller actions do not render Jobs",
            ));
        }
        LabAction::BootstrapLiquidity(request) => render_bootstrap_action(action, lab, request)?,
        LabAction::PeerConnect(request) => render_peer_connect_action(action, lab, request)?,
        LabAction::PeerDisconnect(request) => render_peer_disconnect_action(action, lab, request)?,
        LabAction::ChannelOpen(request) => render_channel_open_action(action, lab, request)?,
        LabAction::ChannelPolicySet(request) => {
            render_channel_policy_set_action(action, lab, request)?
        }
        LabAction::ChannelClose(request) => {
            render_channel_close_action(action, lab, request, false)?
        }
        LabAction::ChannelForceClose(request) => {
            render_channel_close_action(action, lab, request, true)?
        }
        LabAction::ChannelRebalance(request) => {
            render_channel_rebalance_action(action, lab, request)?
        }
        LabAction::WalletInitialize(request) => {
            render_wallet_initialize_action(action, lab, request)?
        }
        LabAction::WalletBalance(request) => render_wallet_balance_action(action, lab, request)?,
        LabAction::WalletFund(request) => render_wallet_fund_action(action, lab, request)?,
        LabAction::WalletInvoice(request) => render_wallet_invoice_action(action, lab, request)?,
        LabAction::WalletPay(request) => render_wallet_pay_action(action, lab, request)?,
        LabAction::WalletQuoteClaim(request) => {
            render_wallet_quote_claim_action(action, lab, request)?
        }
        LabAction::WalletMeltQuoteRefresh(request) => {
            render_wallet_melt_quote_refresh_action(action, lab, request)?
        }
        LabAction::WalletRoundTrip(request) => render_wallet_action(action, lab, request)?,
        LabAction::ConservationOracle(request) => render_oracle_action(action, lab, request)?,
        LabAction::ReachabilityOracle(request) => {
            render_reachability_oracle_action(action, lab, request)?
        }
        LabAction::ComponentForensics(request) => render_native_exec_action(action, lab, request)?,
        LabAction::AuthenticationConformance(request) => {
            render_authentication_conformance_action(action, lab, request)?
        }
        LabAction::AuthenticationProtectedSpend(request) => {
            render_authentication_protected_spend_action(action, lab, request)?
        }
        LabAction::AuthenticationReplay(request) => {
            render_authentication_replay_action(action, lab, request)?
        }
        LabAction::ComponentLogs(_) => {
            return Err(ActionRenderError::InvalidPlan(
                "component logs are fulfilled by the controller, not by a Job".to_owned(),
            ));
        }
    };
    mark_controller_owned(&mut job, &action.name_any());
    Ok(job)
}

fn render_authentication_conformance_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &AuthenticationConformanceAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::AuthenticationTest {
        return Err(ActionRenderError::Capability);
    }
    let mint_image = authentication_components(lab, &request.mint, &request.identity_provider)?;
    render_authentication_conformance_job(&AuthenticationConformanceJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        mint: &request.mint,
        identity_provider: &request.identity_provider,
        mint_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_authentication_protected_spend_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &AuthenticationProtectedSpendAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::AuthenticationTest {
        return Err(ActionRenderError::Capability);
    }
    let mint_image = authentication_components(lab, &request.mint, &request.identity_provider)?;
    render_authentication_protected_spend_job(&AuthenticationProtectedSpendJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        mint: &request.mint,
        identity_provider: &request.identity_provider,
        mint_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_authentication_replay_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &AuthenticationReplayAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::AuthenticationTest {
        return Err(ActionRenderError::Capability);
    }
    let mint_image = authentication_components(lab, &request.mint, &request.identity_provider)?;
    render_authentication_replay_job(&AuthenticationReplayJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        mint: &request.mint,
        identity_provider: &request.identity_provider,
        mint_image,
        session_secret: &request.session_secret,
        source_operation_id: &request.source_operation_id,
    })
    .map_err(ActionRenderError::from)
}

fn authentication_components<'a>(
    lab: &'a ProofstormLab,
    mint: &str,
    identity_provider: &str,
) -> Result<&'a str, ActionRenderError> {
    let mint_image = locked_component_image(lab, mint, ComponentKind::Mint, "nutshell")?;
    locked_component_image(
        lab,
        identity_provider,
        ComponentKind::IdentityProvider,
        "keycloak",
    )?;
    let links = lab
        .spec
        .lab
        .links
        .iter()
        .filter(|link| {
            link.kind == proofstorm_core::LinkKind::AuthenticationBackend
                && link.from == mint
                && link.to == identity_provider
                && matches!(
                    link.binding.as_ref(),
                    Some(proofstorm_core::DependencyBinding::Authentication {
                        protocol: proofstorm_core::AuthenticationProtocol::Oidc
                    })
                )
        })
        .count();
    if links != 1 {
        return Err(ActionRenderError::InvalidPlan(format!(
            "authentication conformance requires exactly one OIDC link from {mint:?} to {identity_provider:?}, found {links}"
        )));
    }
    Ok(mint_image)
}

fn render_native_exec_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ComponentForensicsAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::ComponentForensics {
        return Err(ActionRenderError::Capability);
    }
    if request.script.is_empty() || request.script.len() > 16 * 1024 {
        return Err(ActionRenderError::Bounds(
            "script must contain 1..=16384 UTF-8 bytes",
        ));
    }
    if !(1..=300).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 1..=300",
        ));
    }

    let plans = crate::compile_component_plans(
        &lab.spec.instance_key,
        &lab.spec.revision_digest,
        &lab.spec.lab,
        &lab.spec.lock,
    )
    .map_err(|error| ActionRenderError::InvalidPlan(error.to_string()))?;
    let component = plans
        .iter()
        .find(|plan| plan.component_id == request.component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.component.clone()))?;
    let target = plans
        .iter()
        .find(|plan| plan.component_id == request.target_component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.target_component.clone()))?;
    let context = native_exec_component_context(component)?;
    let mut environment = vec![
        ("PROOFSTORM_COMPONENT".to_owned(), request.component.clone()),
        (
            "PROOFSTORM_EXEC_COMPONENT".to_owned(),
            request.component.clone(),
        ),
        ("PROOFSTORM_SCRIPT".to_owned(), request.script.clone()),
    ];
    environment.extend(context.environment);
    environment.extend(native_exec_target_environment(
        &action.spec.instance_key,
        target,
    ));

    // A non-zero native exit is experiment data, not an infrastructure failure.
    // The controller reads the bounded pod log and this small exit metadata.
    let wrapper = "set +e; /bin/sh -c \"$PROOFSTORM_SCRIPT\"; code=$?; printf '{\"exit_code\":%s}' \"$code\" >/dev/termination-log; exit 0";
    let mut exec_container = container_with_env(
        "exec",
        &component.execution_context.image,
        wrapper,
        &context.mounts,
        environment,
    );
    exec_container["env"]
        .as_array_mut()
        .expect("native exec environment is an array")
        .extend(context.secret_environment);
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(),
        "affinity": instance_affinity(&action.spec.instance_key),
        "containers": [exec_container],
        "volumes": context.volumes,
    });
    let mut rendered = job(
        &action.name_any(),
        &instance_namespace(&action.spec.instance_key),
        &action.spec.instance_key,
        "native-exec",
        i64::from(request.timeout_seconds) + 10,
        &pod,
    )
    .map_err(ActionRenderError::from)?;
    let labels = rendered
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .and_then(|metadata| metadata.labels.as_mut())
        .expect("internally rendered exec pod has labels");
    // Native execution must observe the exact same network policy (including
    // active partitions) as its execution component. A distinct service target
    // does not change the caller's network identity or bypass a partition. It
    // must not match the wider policy used by portable controller-action jobs.
    labels.remove("proofstorm.dev/operation");
    labels.insert(
        "proofstorm.dev/network-identity".to_owned(),
        request.component.clone(),
    );
    Ok(rendered)
}

struct NativeExecComponentContext {
    mounts: Vec<Value>,
    volumes: Vec<Value>,
    environment: Vec<(String, String)>,
    secret_environment: Vec<Value>,
}

fn native_exec_component_context(
    plan: &ComponentPlanContract,
) -> Result<NativeExecComponentContext, ActionRenderError> {
    let mut context = NativeExecComponentContext {
        mounts: Vec::new(),
        volumes: Vec::new(),
        environment: plan
            .execution_context
            .environment
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    value.replace("{component_id}", &plan.component_id),
                )
            })
            .collect(),
        secret_environment: vec![],
    };
    if let EffectiveComponentConfig::Postgres(config) = &plan.effective_config {
        let secret_name = format!("{}-credentials", plan.component_id);
        context.environment.extend([
            ("PGHOST".into(), plan.component_id.clone()),
            ("PGPORT".into(), "5432".into()),
            ("PGUSER".into(), "proofstorm".into()),
            ("PGDATABASE".into(), config.database_name.clone()),
        ]);
        context.secret_environment.push(json!({
            "name": "PGPASSWORD",
            "valueFrom": {"secretKeyRef": {"name": secret_name, "key": "POSTGRES_PASSWORD"}}
        }));
    }
    for binding in &plan.execution_context.mounts {
        context
            .mounts
            .push(mount(&binding.name, &binding.mount_path, binding.read_only));
        let source = match binding.source {
            ExecutionStorageSource::StatefulData => {
                json!({"persistentVolumeClaim": {"claimName": format!("data-{}-0", plan.component_id)}})
            }
            ExecutionStorageSource::ComponentPersistentData => {
                json!({"persistentVolumeClaim": {"claimName": format!("{}-data", plan.component_id)}})
            }
            ExecutionStorageSource::ComponentConfig => {
                json!({"configMap": {"name": format!("{}-config", plan.component_id)}})
            }
            ExecutionStorageSource::LinkedStatefulData { ref link_id } => {
                let target = plan
                    .relevant_links
                    .iter()
                    .find(|link| link.id == *link_id && link.from == plan.component_id)
                    .ok_or_else(|| {
                        ActionRenderError::InvalidPlan(format!(
                            "component {:?} lacks resolved execution binding {link_id:?}",
                            plan.component_id,
                        ))
                    })?;
                let credentials = plan
                    .credentials
                    .iter()
                    .filter(|credential| credential.mount_name == binding.name)
                    .collect::<Vec<_>>();
                let [credential] = credentials.as_slice() else {
                    return Err(ActionRenderError::InvalidPlan(format!(
                        "component {:?} execution mount {:?} requires exactly one compiled credential, found {}",
                        plan.component_id,
                        binding.name,
                        credentials.len()
                    )));
                };
                if credential.source_component_id != target.to {
                    return Err(ActionRenderError::InvalidPlan(format!(
                        "component {:?} execution binding {link_id:?} target {:?} does not match credential source {:?}",
                        plan.component_id, target.to, credential.source_component_id
                    )));
                }
                json!({"persistentVolumeClaim": {"claimName": credential.claim_name}})
            }
        };
        let mut volume = json!({"name": binding.name});
        volume
            .as_object_mut()
            .expect("execution volume is an object")
            .extend(
                source
                    .as_object()
                    .expect("volume source is an object")
                    .clone(),
            );
        context.volumes.push(volume);
    }
    Ok(context)
}

fn native_exec_target_environment(
    instance_key: &str,
    target: &ComponentPlanContract,
) -> Vec<(String, String)> {
    let namespace = instance_namespace(instance_key);
    let ports = &target.target_descriptor.ports;
    let mut environment = vec![
        (
            "PROOFSTORM_TARGET_COMPONENT".to_owned(),
            target.component_id.clone(),
        ),
        (
            "PROOFSTORM_TARGET_KIND".to_owned(),
            serde_json::to_value(target.kind)
                .expect("component kind serializes")
                .as_str()
                .expect("component kind is a string")
                .to_owned(),
        ),
        (
            "PROOFSTORM_TARGET_IMPLEMENTATION".to_owned(),
            target.backend_id.clone(),
        ),
        (
            "PROOFSTORM_TARGET_HOST".to_owned(),
            target.component_id.clone(),
        ),
        (
            "PROOFSTORM_TARGET_FQDN".to_owned(),
            format!("{}.{namespace}.svc", target.component_id),
        ),
        (
            "PROOFSTORM_TARGET_SERVICES_JSON".to_owned(),
            serde_json::to_string(&ports).expect("component ports serialize"),
        ),
    ];
    environment.extend(ports.iter().map(|(name, port)| {
        (
            format!("PROOFSTORM_TARGET_PORT_{}", name.to_ascii_uppercase()),
            port.to_string(),
        )
    }));
    match target.kind {
        ComponentKind::Bitcoin => environment.extend([
            ("BITCOIN_RPC_HOST".to_owned(), target.component_id.clone()),
            (
                "BITCOIN_RPC_PORT".to_owned(),
                ports.get("rpc").copied().unwrap_or_default().to_string(),
            ),
            ("BITCOIN_RPC_USER".to_owned(), "proofstorm".to_owned()),
            (
                "BITCOIN_RPC_PASSWORD".to_owned(),
                "proofstorm-regtest-only".to_owned(),
            ),
        ]),
        ComponentKind::Lightning if target.backend_id == "lnd" => environment.extend([
            ("LND_RPC_HOST".to_owned(), target.component_id.clone()),
            (
                "LND_RPC_PORT".to_owned(),
                ports.get("rpc").copied().unwrap_or_default().to_string(),
            ),
        ]),
        ComponentKind::Lightning if target.backend_id == "cln" => {
            environment.push(("CLN_P2P_HOST".to_owned(), target.component_id.clone()));
        }
        ComponentKind::Mint => environment.push((
            "CASHU_MINT_URL".to_owned(),
            format!(
                "http://{}:{}",
                target.component_id,
                ports.get("http").copied().unwrap_or_default()
            ),
        )),
        ComponentKind::Database => environment.extend([
            (
                "PROOFSTORM_DATABASE_HOST".to_owned(),
                target.component_id.clone(),
            ),
            (
                "PROOFSTORM_DATABASE_PORT".to_owned(),
                ports
                    .get("postgres")
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            ),
        ]),
        _ => {}
    }
    environment
}

/// Render an idempotent controller-owned cleanup Job for private action state.
///
/// Most actions have no persistent private intermediary. Wallet invoices do:
/// their payment request must be removed from the wallet volume before a
/// cancellation can be reported as complete.
///
/// # Errors
///
/// Returns an error when the original action is invalid for its immutable lab
/// or the fixed cleanup resource contract cannot be rendered.
pub fn render_lab_action_cleanup_job(
    _action: &ProofstormLabAction,
    _lab: &ProofstormLab,
) -> Result<Option<Job>, ActionRenderError> {
    Ok(None)
}

fn render_peer_connect_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &PeerConnectAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::PeerConnect {
        return Err(ActionRenderError::Capability);
    }
    validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
    let (from_adapter, from_image) = locked_lightning(lab, &request.from_lightning)?;
    let (to_adapter, to_image) = locked_lightning(lab, &request.to_lightning)?;
    render_peer_connect_job(&PeerConnectJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        from_lightning: &request.from_lightning,
        to_lightning: &request.to_lightning,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_peer_disconnect_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &PeerDisconnectAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::PeerDisconnect {
        return Err(ActionRenderError::Capability);
    }
    validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
    let (from_adapter, from_image) = locked_lightning(lab, &request.from_lightning)?;
    let (to_adapter, to_image) = locked_lightning(lab, &request.to_lightning)?;
    render_peer_disconnect_job(&PeerDisconnectJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        from_lightning: &request.from_lightning,
        to_lightning: &request.to_lightning,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_channel_open_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ChannelOpenAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::ChannelOpen {
        return Err(ActionRenderError::Capability);
    }
    validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
    validate_channel_bounds(request.channel_sat, request.push_sat)?;
    let bitcoin_image =
        locked_component_image(lab, &request.chain, ComponentKind::Bitcoin, "bitcoin-core")?;
    let (from_adapter, from_image) = locked_lightning(lab, &request.from_lightning)?;
    let (to_adapter, to_image) = locked_lightning(lab, &request.to_lightning)?;
    render_channel_open_job(&ChannelOpenJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        chain: &request.chain,
        from_lightning: &request.from_lightning,
        to_lightning: &request.to_lightning,
        bitcoin_image,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
        channel_sat: request.channel_sat,
        push_sat: request.push_sat,
    })
    .map_err(ActionRenderError::from)
}

fn render_channel_policy_set_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ChannelPolicySetAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::ChannelOpen {
        return Err(ActionRenderError::Capability);
    }
    validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
    if request.base_fee_msat > 100_000_000 {
        return Err(ActionRenderError::Bounds(
            "base_fee_msat must be in 0..=100000000",
        ));
    }
    if request.fee_rate_ppm > 1_000_000 {
        return Err(ActionRenderError::Bounds(
            "fee_rate_ppm must be in 0..=1000000",
        ));
    }
    let (from_adapter, from_image) = locked_lightning(lab, &request.from_lightning)?;
    let (to_adapter, to_image) = locked_lightning(lab, &request.to_lightning)?;
    render_channel_policy_set_job(&ChannelPolicySetJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        from_lightning: &request.from_lightning,
        to_lightning: &request.to_lightning,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
        base_fee_msat: request.base_fee_msat,
        fee_rate_ppm: request.fee_rate_ppm,
    })
    .map_err(ActionRenderError::from)
}

fn render_channel_close_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ChannelCloseAction,
    force: bool,
) -> Result<Job, ActionRenderError> {
    let expected = if force {
        Capability::ChannelForceClose
    } else {
        Capability::ChannelClose
    };
    if action.spec.capability != expected {
        return Err(ActionRenderError::Capability);
    }
    validate_lightning_pair(&request.from_lightning, &request.to_lightning)?;
    validate_channel_id(&request.channel_id)?;
    let bitcoin_image =
        locked_component_image(lab, &request.chain, ComponentKind::Bitcoin, "bitcoin-core")?;
    let (from_adapter, from_image) = locked_lightning(lab, &request.from_lightning)?;
    let (to_adapter, to_image) = locked_lightning(lab, &request.to_lightning)?;
    render_channel_close_job(&ChannelCloseJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        chain: &request.chain,
        from_lightning: &request.from_lightning,
        to_lightning: &request.to_lightning,
        channel_id: &request.channel_id,
        bitcoin_image,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
        force,
    })
    .map_err(ActionRenderError::from)
}

fn render_channel_rebalance_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ChannelRebalanceAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::ChannelRebalance {
        return Err(ActionRenderError::Capability);
    }
    validate_channel_id(&request.outgoing_channel_id)?;
    validate_channel_id(&request.incoming_channel_id)?;
    validate_rebalance_bounds(request)?;
    let (adapter, lightning_image) = locked_lightning(lab, &request.lightning)?;
    if adapter != LightningAdapter::Lnd {
        return Err(ActionRenderError::UnsupportedAdapter {
            component: request.lightning.clone(),
            adapter: "cln".into(),
        });
    }
    render_channel_rebalance_job(&ChannelRebalanceJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        lightning: &request.lightning,
        lightning_image,
        outgoing_channel_id: &request.outgoing_channel_id,
        incoming_channel_id: &request.incoming_channel_id,
        amount_sat: request.amount_sat,
        max_fee_sat: request.max_fee_sat,
    })
    .map_err(ActionRenderError::from)
}

fn render_bootstrap_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &BootstrapLiquidityAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletFund {
        return Err(ActionRenderError::Capability);
    }
    validate_bootstrap_action(request)?;
    let bitcoin_image =
        locked_component_image(lab, &request.chain, ComponentKind::Bitcoin, "bitcoin-core")?;
    let mint_lnd_image = locked_component_image(
        lab,
        &request.mint_lightning,
        ComponentKind::Lightning,
        "lnd",
    )?;
    let payer_lnd_image = locked_component_image(
        lab,
        &request.payer_lightning,
        ComponentKind::Lightning,
        "lnd",
    )?;
    if mint_lnd_image != payer_lnd_image {
        return Err(ActionRenderError::Bounds(
            "Lightning components must resolve to the same pinned image",
        ));
    }
    render_bootstrap_job(&BootstrapJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        chain: &request.chain,
        mint_lightning: &request.mint_lightning,
        payer_lightning: &request.payer_lightning,
        bitcoin_image,
        lnd_image: mint_lnd_image,
        funding_sat: request.funding_sat,
        channel_sat: request.channel_sat,
        push_sat: request.push_sat,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_initialize_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletInitializeAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletCreate {
        return Err(ActionRenderError::Capability);
    }
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    render_wallet_initialize_job(&WalletJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        wallet_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_balance_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletBalanceAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletControl {
        return Err(ActionRenderError::Capability);
    }
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    render_wallet_balance_job(&WalletJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        wallet_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_fund_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletFundAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletFund {
        return Err(ActionRenderError::Capability);
    }
    validate_wallet_amount(request.amount_sat)?;
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    let lightning_image = locked_component_image(
        lab,
        &request.payer_lightning,
        ComponentKind::Lightning,
        "lnd",
    )?;
    render_wallet_fund_job(&WalletFundJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        payer_lightning: &request.payer_lightning,
        wallet_image,
        lightning_image,
        amount_sat: request.amount_sat,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_invoice_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletInvoiceAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletFund {
        return Err(ActionRenderError::Capability);
    }
    validate_wallet_amount(request.amount_sat)?;
    if !(30..=600).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 30..=600",
        ));
    }
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    render_wallet_invoice_job(&WalletInvoiceJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        wallet_image,
        amount_sat: request.amount_sat,
        timeout_seconds: request.timeout_seconds,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_pay_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletPayAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletControl {
        return Err(ActionRenderError::Capability);
    }
    validate_quote_id(&request.mint_quote_id)?;
    if request.wallet == request.recipient_wallet {
        return Err(ActionRenderError::Bounds(
            "payer and recipient wallets must differ",
        ));
    }
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    nutshell_wallet_image(lab, &request.recipient_wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    locked_component(lab, &request.recipient_mint, ComponentKind::Mint)?;
    render_wallet_pay_job(&WalletPayJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        recipient_wallet: &request.recipient_wallet,
        recipient_mint: &request.recipient_mint,
        mint_quote_id: &request.mint_quote_id,
        wallet_image,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_quote_claim_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletQuoteClaimAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletControl {
        return Err(ActionRenderError::Capability);
    }
    validate_quote_id(&request.mint_quote_id)?;
    if !(1..=120).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 1..=120",
        ));
    }
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    render_wallet_quote_claim_job(&WalletQuoteClaimJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        mint_quote_id: &request.mint_quote_id,
        wallet_image,
        timeout_seconds: request.timeout_seconds,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_melt_quote_refresh_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletMeltQuoteRefreshAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletControl {
        return Err(ActionRenderError::Capability);
    }
    validate_quote_id(&request.melt_quote_id)?;
    if !(1..=120).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 1..=120",
        ));
    }
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    render_wallet_melt_quote_refresh_job(&WalletMeltQuoteRefreshJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        melt_quote_id: &request.melt_quote_id,
        wallet_image,
        timeout_seconds: request.timeout_seconds,
    })
    .map_err(ActionRenderError::from)
}

fn render_wallet_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &WalletRoundTripAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::WalletControl {
        return Err(ActionRenderError::Capability);
    }
    validate_wallet_round_trip_action(request)?;
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    let lnd_image = locked_component_image(
        lab,
        &request.payer_lightning,
        ComponentKind::Lightning,
        "lnd",
    )?;
    render_wallet_round_trip_job(&WalletRoundTripJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        payer_lightning: &request.payer_lightning,
        wallet_image,
        lnd_image,
        amount_sat: request.amount_sat,
        tolerance_sat: request.tolerance_sat,
    })
    .map_err(ActionRenderError::from)
}

fn render_oracle_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ConservationOracleAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::OracleRun {
        return Err(ActionRenderError::Capability);
    }
    validate_conservation_oracle_action(request)?;
    let wallet_image = nutshell_wallet_image(lab, &request.wallet)?;
    locked_component(lab, &request.mint, ComponentKind::Mint)?;
    render_conservation_oracle_job(&ConservationOracleJobSpec {
        resource_name: &action.name_any(),
        instance_key: &action.spec.instance_key,
        wallet: &request.wallet,
        mint: &request.mint,
        wallet_image,
        baseline_operation_id: &request.baseline_operation_id,
        treatment_operation_id: &request.treatment_operation_id,
        expected_sat: request.expected_sat,
        tolerance_sat: request.tolerance_sat,
    })
    .map_err(ActionRenderError::from)
}

fn render_reachability_oracle_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    request: &ReachabilityOracleAction,
) -> Result<Job, ActionRenderError> {
    if action.spec.capability != Capability::OracleRun {
        return Err(ActionRenderError::Capability);
    }
    validate_reachability_oracle_action(request)?;
    let source = lab
        .spec
        .lab
        .components
        .iter()
        .find(|component| component.id == request.from_component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.from_component.clone()))?;
    let destination = lab
        .spec
        .lab
        .components
        .iter()
        .find(|component| component.id == request.to_component)
        .ok_or_else(|| ActionRenderError::UnknownComponent(request.to_component.clone()))?;
    let port = component_ports(destination)
        .get(&request.service)
        .copied()
        .ok_or_else(|| ActionRenderError::UnknownService {
            component: destination.id.clone(),
            service: request.service.clone(),
        })?;
    let deadline = i64::from(request.timeout_seconds * request.attempts + 15);
    let script = format!(
        "set -eu; reachable=false; completed=0; i=1; while test \"$i\" -le {attempts}; do completed=$i; if nc -z -w {timeout} {destination} {port}; then reachable=true; break; fi; i=$((i+1)); done; printf '{{\"from_component\":\"{source}\",\"to_component\":\"{destination}\",\"service\":\"{service}\",\"port\":{port},\"reachable\":%s,\"attempts\":%s,\"timeout_seconds\":{timeout}}}' \"$reachable\" \"$completed\" >/dev/termination-log",
        attempts = request.attempts,
        timeout = request.timeout_seconds,
        destination = destination.id,
        source = source.id,
        service = request.service,
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(&action.spec.instance_key),
        "containers": [container("oracle", REACHABILITY_PROBE_IMAGE, &script, &[])]
    });
    let mut rendered = job(
        &action.name_any(),
        &instance_namespace(&action.spec.instance_key),
        &action.spec.instance_key,
        "reachability-oracle",
        deadline,
        &pod,
    )?;
    let labels = rendered
        .spec
        .as_mut()
        .and_then(|spec| spec.template.metadata.as_mut())
        .and_then(|metadata| metadata.labels.as_mut())
        .expect("internally rendered probe pod has labels");
    // The Pod must not match the controller-action firewall exception. Giving it
    // the source component identity makes the lab's actual source policy govern
    // this observation, including any active partitions.
    labels.remove("proofstorm.dev/operation");
    labels.insert(
        "proofstorm.dev/network-identity".to_owned(),
        request.from_component.clone(),
    );
    Ok(rendered)
}

fn nutshell_wallet_image<'a>(
    lab: &'a ProofstormLab,
    wallet: &str,
) -> Result<&'a str, ActionRenderError> {
    let (adapter, image) = locked_component(lab, wallet, ComponentKind::Wallet)?;
    if adapter != "nutshell-wallet" {
        return Err(ActionRenderError::UnsupportedAdapter {
            component: wallet.to_owned(),
            adapter: adapter.to_owned(),
        });
    }
    Ok(image)
}

fn validate_action_identity(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
) -> Result<(), ActionRenderError> {
    for (matches, field) in [
        (action.spec.lab_name == lab.name_any(), "lab_name"),
        (
            action.spec.workspace_id == lab.spec.workspace_id,
            "workspace_id",
        ),
        (
            action.spec.instance_id == lab.spec.instance_id,
            "instance_id",
        ),
        (
            action.spec.instance_key == lab.spec.instance_key,
            "instance_key",
        ),
    ] {
        if !matches {
            return Err(ActionRenderError::Identity(field));
        }
    }
    Ok(())
}

fn validate_bootstrap_action(request: &BootstrapLiquidityAction) -> Result<(), ActionRenderError> {
    if !(1..=1_000_000_000).contains(&request.funding_sat) {
        return Err(ActionRenderError::Bounds(
            "funding_sat must be in 1..=1,000,000,000",
        ));
    }
    if !(20_000..=100_000_000).contains(&request.channel_sat) {
        return Err(ActionRenderError::Bounds(
            "channel_sat must be in 20,000..=100,000,000",
        ));
    }
    if request.channel_sat > request.funding_sat {
        return Err(ActionRenderError::Bounds(
            "channel_sat cannot exceed funding_sat",
        ));
    }
    if request.push_sat > request.channel_sat / 2 {
        return Err(ActionRenderError::Bounds(
            "push_sat cannot exceed half of channel_sat",
        ));
    }
    if request.mint_lightning == request.payer_lightning {
        return Err(ActionRenderError::Bounds(
            "mint and payer Lightning components must differ",
        ));
    }
    Ok(())
}

fn validate_lightning_pair(from: &str, to: &str) -> Result<(), ActionRenderError> {
    if from == to {
        return Err(ActionRenderError::Bounds(
            "from and to Lightning components must differ",
        ));
    }
    Ok(())
}

fn validate_channel_bounds(channel_sat: u64, push_sat: u64) -> Result<(), ActionRenderError> {
    if !(20_000..=100_000_000).contains(&channel_sat) {
        return Err(ActionRenderError::Bounds(
            "channel_sat must be in 20,000..=100,000,000",
        ));
    }
    if push_sat > channel_sat / 2 {
        return Err(ActionRenderError::Bounds(
            "push_sat cannot exceed half of channel_sat",
        ));
    }
    Ok(())
}

fn validate_channel_id(channel_id: &str) -> Result<(), ActionRenderError> {
    let digest = channel_id.strip_prefix("ch-").unwrap_or_default();
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ActionRenderError::Bounds(
            "channel_id must be an opaque ch- prefixed SHA-256 handle",
        ));
    }
    Ok(())
}

fn validate_rebalance_bounds(request: &ChannelRebalanceAction) -> Result<(), ActionRenderError> {
    if request.outgoing_channel_id == request.incoming_channel_id {
        return Err(ActionRenderError::Bounds(
            "outgoing and incoming channel handles must differ",
        ));
    }
    if !(1..=10_000_000).contains(&request.amount_sat) {
        return Err(ActionRenderError::Bounds(
            "amount_sat must be in 1..=10,000,000",
        ));
    }
    if request.max_fee_sat > request.amount_sat || request.max_fee_sat > 100_000 {
        return Err(ActionRenderError::Bounds(
            "max_fee_sat cannot exceed amount_sat or 100,000",
        ));
    }
    Ok(())
}

fn validate_wallet_round_trip_action(
    request: &WalletRoundTripAction,
) -> Result<(), ActionRenderError> {
    validate_wallet_amount(request.amount_sat)?;
    if request.tolerance_sat > request.amount_sat || request.tolerance_sat > 10_000 {
        return Err(ActionRenderError::Bounds(
            "tolerance_sat cannot exceed amount_sat or 10,000",
        ));
    }
    Ok(())
}

fn validate_wallet_amount(amount_sat: u64) -> Result<(), ActionRenderError> {
    if !(1..=500_000).contains(&amount_sat) {
        return Err(ActionRenderError::Bounds(
            "amount_sat must be in 1..=500,000",
        ));
    }
    Ok(())
}

fn validate_quote_id(value: &str) -> Result<(), ActionRenderError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || bytes[0] == b'-'
        || bytes[bytes.len() - 1] == b'-'
        || value.contains("--")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return Err(ActionRenderError::Bounds(
            "quote_id must be a lowercase kebab-case identifier of 1..=63 bytes",
        ));
    }
    Ok(())
}

fn validate_conservation_oracle_action(
    request: &ConservationOracleAction,
) -> Result<(), ActionRenderError> {
    if request.expected_sat > 100_000_000 {
        return Err(ActionRenderError::Bounds(
            "expected_sat cannot exceed 100,000,000",
        ));
    }
    if request.tolerance_sat > 10_000 {
        return Err(ActionRenderError::Bounds(
            "tolerance_sat cannot exceed 10,000",
        ));
    }
    Ok(())
}

fn validate_reachability_oracle_action(
    request: &ReachabilityOracleAction,
) -> Result<(), ActionRenderError> {
    if request.from_component == request.to_component {
        return Err(ActionRenderError::Bounds(
            "from_component and to_component must differ",
        ));
    }
    if !(1..=5).contains(&request.timeout_seconds) {
        return Err(ActionRenderError::Bounds(
            "timeout_seconds must be in 1..=5",
        ));
    }
    if !(1..=5).contains(&request.attempts) {
        return Err(ActionRenderError::Bounds("attempts must be in 1..=5"));
    }
    Ok(())
}

fn locked_component<'a>(
    lab: &'a ProofstormLab,
    id: &str,
    kind: ComponentKind,
) -> Result<(&'a str, &'a str), ActionRenderError> {
    let component = lab
        .spec
        .lab
        .components
        .iter()
        .find(|component| component.id == id && component.kind == kind)
        .ok_or_else(|| ActionRenderError::Component {
            component: id.to_owned(),
            implementation: "installed",
            kind,
        })?;
    let lock = lab
        .spec
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id && entry.catalog_id == component.implementation)
        .ok_or_else(|| ActionRenderError::MissingLock(id.to_owned()))?;
    Ok((&component.implementation, &lock.image))
}

fn locked_lightning<'a>(
    lab: &'a ProofstormLab,
    id: &str,
) -> Result<(LightningAdapter, &'a str), ActionRenderError> {
    let (implementation, image) = locked_component(lab, id, ComponentKind::Lightning)?;
    let adapter = LightningAdapter::from_implementation(implementation).ok_or_else(|| {
        ActionRenderError::UnsupportedAdapter {
            component: id.to_owned(),
            adapter: implementation.to_owned(),
        }
    })?;
    Ok((adapter, image))
}

fn locked_component_image<'a>(
    lab: &'a ProofstormLab,
    id: &str,
    kind: ComponentKind,
    implementation: &'static str,
) -> Result<&'a str, ActionRenderError> {
    let valid = lab.spec.lab.components.iter().any(|component| {
        component.id == id && component.kind == kind && component.implementation == implementation
    });
    if !valid {
        return Err(ActionRenderError::Component {
            component: id.to_owned(),
            implementation,
            kind,
        });
    }
    lab.spec
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id && entry.catalog_id == implementation)
        .map(|entry| entry.image.as_str())
        .ok_or_else(|| ActionRenderError::MissingLock(id.to_owned()))
}

fn mark_controller_owned(job: &mut Job, action_name: &str) {
    for labels in [
        job.metadata.labels.as_mut(),
        job.spec
            .as_mut()
            .and_then(|spec| spec.template.metadata.as_mut())
            .and_then(|metadata| metadata.labels.as_mut()),
    ]
    .into_iter()
    .flatten()
    {
        labels.insert(
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstormd".to_owned(),
        );
        labels.insert("proofstorm.dev/action".to_owned(), action_name.to_owned());
    }
}

/// Render the bounded chain funding and Lightning channel bootstrap job.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_bootstrap_job(spec: &BootstrapJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let BootstrapJobSpec {
        resource_name,
        instance_key,
        chain,
        mint_lightning,
        payer_lightning,
        bitcoin_image,
        lnd_image,
        funding_sat,
        channel_sat,
        push_sat,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let bcli = format!(
        "bitcoin-cli -regtest -rpcconnect={chain} -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only"
    );
    let chain_init = format!(
        "set -eu; until {bcli} getblockchaininfo >/dev/null 2>&1; do sleep 1; done; {bcli} createwallet default >/dev/null 2>&1 || true; addr=$({bcli} -rpcwallet=default getnewaddress); {bcli} -rpcwallet=default generatetoaddress 101 \"$addr\" >/dev/null; printf '%s' \"$addr\" >/shared/miner-address"
    );
    let address = |node: &str, path: &str, output: &str| {
        format!(
            "set -eu; until lncli --lnddir={path} --network=regtest --rpcserver={node}:10009 getinfo >/dev/null 2>&1; do sleep 1; done; lncli --lnddir={path} --network=regtest --rpcserver={node}:10009 newaddress p2wkh | grep -o '\"address\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4 >{output}; test -s {output}"
        )
    };
    let fund = format!(
        "set -eu; a=$(cat /shared/mint-address); b=$(cat /shared/payer-address); {bcli} -rpcwallet=default sendtoaddress \"$a\" {} >/dev/null; {bcli} -rpcwallet=default sendtoaddress \"$b\" {} >/dev/null; {bcli} -rpcwallet=default generatetoaddress 6 \"$(cat /shared/miner-address)\" >/dev/null",
        sats_to_btc(funding_sat),
        sats_to_btc(funding_sat)
    );
    let channel = format!(
        "set -eu; mint='lncli --lnddir=/mint-lnd --network=regtest --rpcserver={mint_lightning}:10009'; payer='lncli --lnddir=/payer-lnd --network=regtest --rpcserver={payer_lightning}:10009'; pk=$($mint getinfo | grep -o '\"identity_pubkey\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4); test -n \"$pk\"; $payer connect \"$pk@{mint_lightning}:9735\" >/dev/null 2>&1 || true; $payer listchannels --peer \"$pk\" | grep -o '\"channel_point\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4 >/shared/channels-before || true; $payer openchannel --node_key=\"$pk\" --local_amt={channel_sat} --push_amt={push_sat} >/dev/null; printf '%s' \"$pk\" >/shared/peer-pubkey"
    );
    let confirm = format!(
        "set -eu; {bcli} -rpcwallet=default generatetoaddress 6 \"$(cat /shared/miner-address)\" >/dev/null"
    );
    let channel_verify = format!(
        "set -eu; payer='lncli --lnddir=/payer-lnd --network=regtest --rpcserver={payer_lightning}:10009'; pk=$(cat /shared/peer-pubkey); point=''; until test -n \"$point\"; do for candidate in $($payer listchannels --peer \"$pk\" | grep -o '\"channel_point\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4); do if ! grep -Fxq \"$candidate\" /shared/channels-before; then point=$candidate; break; fi; done; test -n \"$point\" || sleep 1; done; printf '%s' \"$point\" >/shared/channel-point"
    );
    let result = format!(
        "set -eu; point=$(cat /shared/channel-point); digest=$(printf '%s' \"$point\" | sha256sum | cut -d' ' -f1); printf '%s' '{{\"funding_sat\":{funding_sat},\"channel_sat\":{channel_sat},\"push_sat\":{push_sat},\"channel_id\":\"ch-'\"$digest\"'\",\"ready\":true}}' >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [
            container("chain-init", bitcoin_image, &chain_init, &[mount("shared", "/shared", false)]),
            container("mint-address", lnd_image, &address(mint_lightning, "/mint-lnd", "/shared/mint-address"), &[mount("shared", "/shared", false), mount("mint-lnd", "/mint-lnd", true)]),
            container("payer-address", lnd_image, &address(payer_lightning, "/payer-lnd", "/shared/payer-address"), &[mount("shared", "/shared", false), mount("payer-lnd", "/payer-lnd", true)]),
            container("chain-fund", bitcoin_image, &fund, &[mount("shared", "/shared", false)]),
            container("channel-open", lnd_image, &channel, &[mount("shared", "/shared", false), mount("mint-lnd", "/mint-lnd", true), mount("payer-lnd", "/payer-lnd", true)]),
            container("channel-confirm", bitcoin_image, &confirm, &[mount("shared", "/shared", false)]),
            container("channel-verify", lnd_image, &channel_verify, &[mount("shared", "/shared", false), mount("payer-lnd", "/payer-lnd", true)])
        ],
        "containers": [container("result", "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662", &result, &[mount("shared", "/shared", true)])],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "mint-lnd", "persistentVolumeClaim": {"claimName": format!("data-{mint_lightning}-0")}},
            {"name": "payer-lnd", "persistentVolumeClaim": {"claimName": format!("data-{payer_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "bootstrap",
        300,
        &pod,
    )
}

fn lightning_cli(adapter: LightningAdapter, mount: &str, component: &str) -> String {
    match adapter {
        LightningAdapter::Lnd => {
            format!("lncli --lnddir={mount} --network=regtest --rpcserver={component}:10009")
        }
        LightningAdapter::Cln => {
            format!("lightning-cli --lightning-dir={mount} --network=regtest")
        }
    }
}

fn lightning_identity_script(
    adapter: LightningAdapter,
    mount: &str,
    component: &str,
    output: &str,
) -> String {
    let cli = lightning_cli(adapter, mount, component);
    let extract = match adapter {
        LightningAdapter::Lnd => {
            "$cli getinfo | grep -o '\"identity_pubkey\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4"
        }
        LightningAdapter::Cln => "$cli getinfo | jq -r '.id'",
    };
    format!(
        "set -eu; cli='{cli}'; until $cli getinfo >/dev/null 2>&1; do sleep 1; done; pk=$({extract}); test -n \"$pk\"; test \"$pk\" != null; printf '%s' \"$pk\" >{output}"
    )
}

fn peer_connected_test(adapter: LightningAdapter, cli: &str, peer: &str) -> String {
    match adapter {
        LightningAdapter::Lnd => format!("{cli} listpeers | grep -q \"{peer}\""),
        LightningAdapter::Cln => {
            format!("{cli} listpeers \"{peer}\" | grep -Eq '\"connected\":[[:space:]]*true'")
        }
    }
}

fn peer_connect_command(adapter: LightningAdapter, cli: &str, peer: &str, host: &str) -> String {
    match adapter {
        LightningAdapter::Lnd => {
            format!("{cli} connect \"{peer}@{host}:9735\" >/shared/peer-connect.log 2>&1 || true")
        }
        LightningAdapter::Cln => {
            format!(
                "{cli} connect \"{peer}\" \"{host}\" 9735 >/shared/peer-connect.log 2>&1 || true"
            )
        }
    }
}

fn peer_disconnect_command(adapter: LightningAdapter, cli: &str, peer: &str) -> String {
    match adapter {
        LightningAdapter::Lnd => {
            format!("{cli} disconnect \"{peer}\" >/dev/null 2>&1 || true")
        }
        LightningAdapter::Cln => {
            format!("{cli} disconnect \"{peer}\" true >/dev/null 2>&1 || true")
        }
    }
}

fn channel_points_command(adapter: LightningAdapter, cli: &str, peer: &str) -> String {
    match adapter {
        LightningAdapter::Lnd => format!(
            "{cli} listchannels --peer \"{peer}\" | grep -o '\"channel_point\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4"
        ),
        LightningAdapter::Cln => format!(
            "{cli} listpeerchannels \"{peer}\" | jq -r '.channels[] | select(.funding_txid != null) | \"\\(.funding_txid):\\(.funding_outnum)\"'"
        ),
    }
}

fn active_channel_points_command(adapter: LightningAdapter, cli: &str, peer: &str) -> String {
    match adapter {
        LightningAdapter::Lnd => channel_points_command(adapter, cli, peer),
        LightningAdapter::Cln => format!(
            "{cli} listpeerchannels \"{peer}\" | jq -r '.channels[] | select(.state == \"CHANNELD_NORMAL\") | \"\\(.funding_txid):\\(.funding_outnum)\"'"
        ),
    }
}

/// Render a bounded logical Lightning peer-connect job for the installed adapter.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_peer_connect_job(spec: &PeerConnectJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let PeerConnectJobSpec {
        resource_name,
        instance_key,
        from_lightning,
        to_lightning,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let identity = lightning_identity_script(to_adapter, "/to", to_lightning, "/shared/to-pubkey");
    let from_cli = lightning_cli(from_adapter, "/from", from_lightning);
    let connect = peer_connect_command(from_adapter, "$from", "$pk", to_lightning);
    let connected = peer_connected_test(from_adapter, "$from", "$pk");
    let script = format!(
        "set -eu; from='{from_cli}'; until $from getinfo >/dev/null 2>&1; do sleep 1; done; pk=$(cat /shared/to-pubkey); for attempt in $(seq 1 60); do if {connected}; then break; fi; {connect}; sleep 1; done; if ! {connected}; then echo 'peer connection did not become ready after 60 attempts' >&2; tail -c 2048 /shared/peer-connect.log >&2 2>/dev/null || true; exit 1; fi; printf '{{\"from\":\"{from_lightning}\",\"to\":\"{to_lightning}\",\"connected\":true}}' >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [container("to-identity", to_image, &identity, &[
            mount("shared", "/shared", false), mount("to", "/to", true)
        ])],
        "containers": [container("result", from_image, &script, &[
            mount("shared", "/shared", true), mount("from", "/from", true)
        ])],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
            {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "peer-connect",
        90,
        &pod,
    )
}

/// Render a bounded logical Lightning peer-disconnect job.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_peer_disconnect_job(
    spec: &PeerDisconnectJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let PeerDisconnectJobSpec {
        resource_name,
        instance_key,
        from_lightning,
        to_lightning,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    if from_adapter == LightningAdapter::Lnd && to_adapter == LightningAdapter::Lnd {
        let from_cli = lightning_cli(from_adapter, "/from", from_lightning);
        let to_cli = lightning_cli(to_adapter, "/to", to_lightning);
        let script = format!(
            "set -eu; from='{from_cli}'; to='{to_cli}'; until $from getinfo >/dev/null 2>&1 && $to getinfo >/dev/null 2>&1; do sleep 1; done; from_pk=$($from getinfo | grep -o '\"identity_pubkey\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4); to_pk=$($to getinfo | grep -o '\"identity_pubkey\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4); $from disconnect \"$to_pk\" >/dev/null 2>&1 || true; $to disconnect \"$from_pk\" >/dev/null 2>&1 || true; sleep 2; if $from listpeers | grep -q \"$to_pk\" || $to listpeers | grep -q \"$from_pk\"; then exit 1; fi; printf '{{\"from\":\"{from_lightning}\",\"to\":\"{to_lightning}\",\"disconnected\":true}}' >/dev/termination-log"
        );
        let pod = json!({
            "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
            "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
            "containers": [container("result", from_image, &script, &[
                mount("from", "/from", true), mount("to", "/to", true)
            ])],
            "volumes": [
                {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
                {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
            ]
        });
        return job(
            resource_name,
            &namespace,
            instance_key,
            "peer-disconnect",
            90,
            &pod,
        );
    }
    let from_identity =
        lightning_identity_script(from_adapter, "/from", from_lightning, "/shared/from-pubkey");
    let to_identity =
        lightning_identity_script(to_adapter, "/to", to_lightning, "/shared/to-pubkey");
    let from_cli = lightning_cli(from_adapter, "/from", from_lightning);
    let to_cli = lightning_cli(to_adapter, "/to", to_lightning);
    let from_disconnect = peer_disconnect_command(from_adapter, "$cli", "$pk");
    let to_disconnect = peer_disconnect_command(to_adapter, "$cli", "$pk");
    let from_connected = peer_connected_test(from_adapter, "$cli", "$pk");
    let to_connected = peer_connected_test(to_adapter, "$cli", "$pk");
    let from_disconnect_script =
        format!("set -eu; cli='{from_cli}'; pk=$(cat /shared/to-pubkey); {from_disconnect}");
    let to_disconnect_script =
        format!("set -eu; cli='{to_cli}'; pk=$(cat /shared/from-pubkey); {to_disconnect}");
    let from_verify = format!(
        "set -eu; cli='{from_cli}'; pk=$(cat /shared/to-pubkey); sleep 2; if {from_connected}; then exit 1; fi"
    );
    let result = format!(
        "set -eu; cli='{to_cli}'; pk=$(cat /shared/from-pubkey); if {to_connected}; then exit 1; fi; printf '{{\"from\":\"{from_lightning}\",\"to\":\"{to_lightning}\",\"disconnected\":true}}' >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [
            container("from-identity", from_image, &from_identity, &[mount("shared", "/shared", false), mount("from", "/from", true)]),
            container("to-identity", to_image, &to_identity, &[mount("shared", "/shared", false), mount("to", "/to", true)]),
            container("from-disconnect", from_image, &from_disconnect_script, &[mount("shared", "/shared", true), mount("from", "/from", true)]),
            container("to-disconnect", to_image, &to_disconnect_script, &[mount("shared", "/shared", true), mount("to", "/to", true)]),
            container("from-verify", from_image, &from_verify, &[mount("shared", "/shared", true), mount("from", "/from", true)])
        ],
        "containers": [container("result", to_image, &result, &[
            mount("shared", "/shared", true), mount("to", "/to", true)
        ])],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
            {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "peer-disconnect",
        90,
        &pod,
    )
}

/// Render a bounded Lightning channel-open and confirmation job.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_channel_open_job(spec: &ChannelOpenJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let ChannelOpenJobSpec {
        resource_name,
        instance_key,
        chain,
        from_lightning,
        to_lightning,
        bitcoin_image,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
        channel_sat,
        push_sat,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let to_identity =
        lightning_identity_script(to_adapter, "/to", to_lightning, "/shared/to-pubkey");
    let from_cli = lightning_cli(from_adapter, "/from", from_lightning);
    let points = channel_points_command(from_adapter, "$from", "$pk");
    let active_points = active_channel_points_command(from_adapter, "$from", "$pk");
    let connected = peer_connected_test(from_adapter, "$from", "$pk");
    let connect = peer_connect_command(from_adapter, "$from", "$pk", to_lightning);
    let open_command = match from_adapter {
        LightningAdapter::Lnd => format!(
            "if ! $from openchannel --node_key=\"$pk\" --local_amt={channel_sat} --push_amt={push_sat} >/shared/channel-open.log 2>&1; then cat /shared/channel-open.log >&2; exit 1; fi"
        ),
        LightningAdapter::Cln => format!(
            "if ! $from fundchannel -k \"id=$pk\" \"amount={channel_sat}sat\" \"announce=true\" \"push_msat={push_sat}msat\" >/shared/open.json 2>/shared/channel-open.log; then cat /shared/channel-open.log >&2; exit 1; fi; txid=$(jq -r '.txid' /shared/open.json); outnum=$(jq -r '.outnum' /shared/open.json); test -n \"$txid\"; test \"$txid\" != null; printf '%s:%s' \"$txid\" \"$outnum\" >/shared/channel-point"
        ),
    };
    let open = format!(
        "set -eu; from='{from_cli}'; until $from getinfo >/dev/null 2>&1; do sleep 1; done; pk=$(cat /shared/to-pubkey); for attempt in $(seq 1 60); do if {connected}; then break; fi; {connect}; sleep 1; done; if ! {connected}; then echo 'channel endpoint peer connection did not become ready after 60 attempts' >&2; tail -c 2048 /shared/peer-connect.log >&2 2>/dev/null || true; exit 1; fi; {points} >/shared/channels-before || true; {open_command}; printf '%s' \"$pk\" >/shared/peer-pubkey"
    );
    let bcli = format!(
        "bitcoin-cli -regtest -rpcconnect={chain} -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only"
    );
    let confirm = format!(
        "set -eu; until {bcli} getblockchaininfo >/dev/null 2>&1; do sleep 1; done; addr=$({bcli} -rpcwallet=default getnewaddress); {bcli} -rpcwallet=default generatetoaddress 6 \"$addr\" >/dev/null"
    );
    let verify = format!(
        "set -eu; from='{from_cli}'; pk=$(cat /shared/peer-pubkey); expected=$(cat /shared/channel-point 2>/dev/null || true); point=''; until test -n \"$point\"; do for candidate in $({active_points}); do if test -n \"$expected\"; then test \"$candidate\" = \"$expected\" && point=$candidate && break; elif ! grep -Fxq \"$candidate\" /shared/channels-before; then point=$candidate; break; fi; done; test -n \"$point\" || sleep 1; done; printf '%s' \"$point\" >/shared/channel-point"
    );
    let result = format!(
        "set -eu; point=$(cat /shared/channel-point); digest=$(printf '%s' \"$point\" | sha256sum | cut -d' ' -f1); printf '%s' '{{\"from\":\"{from_lightning}\",\"to\":\"{to_lightning}\",\"channel_id\":\"ch-'\"$digest\"'\",\"channel_sat\":{channel_sat},\"push_sat\":{push_sat},\"active\":true}}' >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [
            container("to-identity", to_image, &to_identity, &[mount("shared", "/shared", false), mount("to", "/to", true)]),
            container("channel-open", from_image, &open, &[mount("shared", "/shared", false), mount("from", "/from", true)]),
            container("channel-confirm", bitcoin_image, &confirm, &[]),
            container("channel-verify", from_image, &verify, &[mount("shared", "/shared", false), mount("from", "/from", true)])
        ],
        "containers": [container("result", "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662", &result, &[mount("shared", "/shared", true)])],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
            {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "channel-open",
        180,
        &pod,
    )
}

/// Render a bounded, adapter-specific outgoing channel-policy update.
///
/// The caller supplies only logical endpoint identities and numeric policy;
/// native channel identifiers and credentials remain inside the Job.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_channel_policy_set_job(
    spec: &ChannelPolicySetJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let ChannelPolicySetJobSpec {
        resource_name,
        instance_key,
        from_lightning,
        to_lightning,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
        base_fee_msat,
        fee_rate_ppm,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let to_identity =
        lightning_identity_script(to_adapter, "/to", to_lightning, "/shared/to-pubkey");
    let from_cli = lightning_cli(from_adapter, "/from", from_lightning);
    let update = match from_adapter {
        LightningAdapter::Lnd => format!(
            r#"points=$($from listchannels --peer "$pk" | grep -o '"channel_point":[[:space:]]*"[^"]*"' | cut -d'"' -f4)
count=0
for point in $points; do
  if ! $from updatechanpolicy --base_fee_msat={base_fee_msat} --fee_rate_ppm={fee_rate_ppm} --time_lock_delta=40 --chan_point="$point" >/shared/policy-update.log 2>&1; then
    cat /shared/policy-update.log >&2
    exit 1
  fi
  count=$((count + 1))
done
test "$count" -gt 0"#
        ),
        LightningAdapter::Cln => format!(
            r#"if ! $from setchannel "id=$pk" "feebase={base_fee_msat}" "feeppm={fee_rate_ppm}" >/shared/policy-update.log 2>&1; then
  cat /shared/policy-update.log >&2
  exit 1
fi"#
        ),
    };
    let result = format!(
        r#"set -eu
from='{from_cli}'
until $from getinfo >/dev/null 2>&1; do sleep 1; done
pk=$(cat /shared/to-pubkey)
{update}
printf '%s' '{{"from":"{from_lightning}","to":"{to_lightning}","base_fee_msat":{base_fee_msat},"fee_rate_ppm":{fee_rate_ppm},"updated":true}}' >/dev/termination-log"#
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [
            container("to-identity", to_image, &to_identity, &[mount("shared", "/shared", false), mount("to", "/to", true)])
        ],
        "containers": [
            container("result", from_image, &result, &[mount("shared", "/shared", false), mount("from", "/from", true)])
        ],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
            {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "channel-policy-set",
        90,
        &pod,
    )
}

/// Render a bounded circular LND payment between two opaque channel handles.
///
/// Payment material and native channel identifiers remain in the Job's
/// ephemeral volume; the terminal artifact contains only logical identities,
/// opaque handles, amounts, and observed balance deltas.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_channel_rebalance_job(
    spec: &ChannelRebalanceJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let ChannelRebalanceJobSpec {
        resource_name,
        instance_key,
        lightning,
        lightning_image,
        outgoing_channel_id,
        incoming_channel_id,
        amount_sat,
        max_fee_sat,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let cli = lightning_cli(LightningAdapter::Lnd, "/lightning", lightning);
    let script = format!(
        r#"set -eu
node='{cli}'
until $node getinfo >/dev/null 2>&1; do sleep 1; done
snapshot() {{
  $node listchannels --active_only --skip_peer_alias_lookup | awk '
    function val(line) {{ sub(/^[^:]*:[[:space:]]*/, "", line); gsub(/[",[:space:]]/, "", line); return line }}
    /"active":/ {{ active=val($0) }}
    /"remote_pubkey":/ {{ peer=val($0) }}
    /"channel_point":/ {{ point=val($0) }}
    /"scid":/ {{ chan=val($0) }}
    /"local_balance":/ {{ local=val($0) }}
    /"remote_balance":/ {{ remote=val($0); print point "|" chan "|" peer "|" local "|" remote "|" active }}'
}}
rounds=0
while :; do
  snapshot >/shared/before
  out_chan=''; out_peer=''; out_before=''; in_chan=''; in_peer=''; in_before=''; in_remote=''
  while IFS='|' read -r point chan peer local remote active; do
    test "$active" = true || continue
    digest=$(printf '%s' "$point" | sha256sum | cut -d' ' -f1)
    if test "ch-$digest" = '{outgoing_channel_id}'; then out_chan=$chan; out_peer=$peer; out_before=$local; fi
    if test "ch-$digest" = '{incoming_channel_id}'; then in_chan=$chan; in_peer=$peer; in_before=$local; in_remote=$remote; fi
  done </shared/before
  if test -n "$out_chan" && test -n "$in_chan" && test "$out_chan" != "$in_chan" && test "$out_peer" != "$in_peer"; then break; fi
  rounds=$((rounds + 1)); test "$rounds" -lt 30; sleep 1
done
test "$out_before" -ge $(({amount_sat} + {max_fee_sat})); test "$in_remote" -ge {amount_sat}
invoice=$($node addinvoice --memo=proofstorm-rebalance --amt={amount_sat} --private --expiry=120 | grep -o '"payment_request":[[:space:]]*"[^"]*"' | cut -d'"' -f4)
test -n "$invoice"
attempts=0
until $node sendpayment --pay_req="$invoice" --outgoing_chan_id="$out_chan" --last_hop="$in_peer" --fee_limit={max_fee_sat} --timeout=10s --max_parts=1 --allow_self_payment --force --json >/shared/payment.json 2>&1 && grep -Eq '"status":[[:space:]]*"SUCCEEDED"' /shared/payment.json; do
  attempts=$((attempts + 1)); test "$attempts" -lt 45; sleep 2
done
snapshot >/shared/after
out_after=''; in_after=''
while IFS='|' read -r point chan peer local remote active; do
  if test "$chan" = "$out_chan"; then out_after=$local; fi
  if test "$chan" = "$in_chan"; then in_after=$local; fi
done </shared/after
test -n "$out_after"; test -n "$in_after"
out_delta=$((out_before - out_after)); in_delta=$((in_after - in_before)); fee_sat=$((out_delta - {amount_sat}))
test "$out_delta" -ge {amount_sat}; test "$in_delta" -ge {amount_sat}; test "$fee_sat" -ge 0; test "$fee_sat" -le {max_fee_sat}
printf '{{"lightning":"{lightning}","outgoing_channel_id":"{outgoing_channel_id}","incoming_channel_id":"{incoming_channel_id}","amount_sat":{amount_sat},"fee_sat":%s,"outgoing_local_before_sat":%s,"outgoing_local_after_sat":%s,"incoming_local_before_sat":%s,"incoming_local_after_sat":%s,"rebalanced":true}}' "$fee_sat" "$out_before" "$out_after" "$in_before" "$in_after" >/dev/termination-log
"#
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [container("result", lightning_image, &script, &[
            mount("shared", "/shared", false), mount("lightning", "/lightning", true)
        ])],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "lightning", "persistentVolumeClaim": {"claimName": format!("data-{lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "channel-rebalance",
        120,
        &pod,
    )
}

fn render_cln_channel_close_job(
    spec: &ChannelCloseJobSpec<'_>,
    namespace: &str,
    to_identity: &str,
    from_cli: &str,
) -> Result<Job, serde_json::Error> {
    let ChannelCloseJobSpec {
        resource_name,
        instance_key,
        chain,
        from_lightning,
        to_lightning,
        channel_id,
        bitcoin_image,
        from_image,
        to_image,
        force,
        ..
    } = *spec;
    let unilateral_timeout = u8::from(force);
    let close = format!(
        "set -eu; from='{from_cli}'; until $from getinfo >/dev/null 2>&1; do sleep 1; done; pk=$(cat /shared/to-pubkey); $from listpeerchannels \"$pk\" | jq -r '.channels[] | select(.funding_txid != null) | \"\\(.funding_txid):\\(.funding_outnum) \\(.channel_id)\"' >/shared/channels; point=''; native=''; while read -r candidate candidate_native; do digest=$(printf '%s' \"$candidate\" | sha256sum | cut -d' ' -f1); if [ \"ch-$digest\" = \"{channel_id}\" ]; then point=$candidate; native=$candidate_native; break; fi; done </shared/channels; test -n \"$point\"; test -n \"$native\"; printf '%s' \"$point\" >/shared/channel-point; printf '%s' \"$native\" >/shared/native-channel-id; touch /shared/close-started; $from close \"$native\" {unilateral_timeout} >/shared/close.json; touch /shared/close-done"
    );
    let bcli = format!(
        "bitcoin-cli -regtest -rpcconnect={chain} -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only"
    );
    let confirm = format!(
        "set -eu; until test -f /shared/close-started; do sleep 1; done; until {bcli} getblockchaininfo >/dev/null 2>&1; do sleep 1; done; addr=$({bcli} -rpcwallet=default getnewaddress); rounds=0; until test -f /shared/close-done; do {bcli} -rpcwallet=default generatetoaddress 1 \"$addr\" >/dev/null; rounds=$((rounds + 1)); test \"$rounds\" -lt 60; sleep 1; done; {bcli} -rpcwallet=default generatetoaddress 6 \"$addr\" >/dev/null; touch /shared/mined"
    );
    let states = if force {
        "AWAITING_UNILATERAL|FUNDING_SPEND_SEEN|ONCHAIN"
    } else {
        "CLOSINGD_COMPLETE|FUNDING_SPEND_SEEN|ONCHAIN"
    };
    let result = format!(
        "set -eu; until test -f /shared/mined; do sleep 1; done; from='{from_cli}'; native=$(cat /shared/native-channel-id); rounds=0; while :; do state=$($from listpeerchannels | jq -r --arg id \"$native\" '.channels[] | select(.channel_id == $id) | .state'); if printf '%s' \"$state\" | grep -q CHANNELD_NORMAL; then exit 1; fi; if test -n \"$state\" && printf '%s' \"$state\" | grep -Eq '{states}'; then break; fi; if test -z \"$state\" && $from listclosedchannels | grep -q \"$native\"; then break; fi; rounds=$((rounds + 1)); test \"$rounds\" -lt 30; sleep 1; done; printf '%s' '{{\"from\":\"{from_lightning}\",\"to\":\"{to_lightning}\",\"channel_id\":\"{channel_id}\",\"closed\":true,\"confirmed\":true,\"force\":{force},\"pending_resolution\":{force}}}' >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [container("to-identity", to_image, to_identity, &[
            mount("shared", "/shared", false), mount("to", "/to", true)
        ])],
        "containers": [
            container("channel-close", from_image, &close, &[mount("shared", "/shared", false), mount("from", "/from", true)]),
            container("channel-confirm", bitcoin_image, &confirm, &[mount("shared", "/shared", false)]),
            container("result", from_image, &result, &[mount("shared", "/shared", true), mount("from", "/from", true)])
        ],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
            {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        namespace,
        instance_key,
        if force {
            "channel-force-close"
        } else {
            "channel-close"
        },
        180,
        &pod,
    )
}

/// Render a bounded cooperative or force channel-close and confirmation job.
///
/// The public channel handle is matched against hashes of active LND channel
/// points inside the credential-bearing Job. Raw implementation identifiers do
/// not cross the controller boundary.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_channel_close_job(spec: &ChannelCloseJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let ChannelCloseJobSpec {
        resource_name,
        instance_key,
        chain,
        from_lightning,
        to_lightning,
        channel_id,
        bitcoin_image,
        from_adapter,
        from_image,
        to_adapter,
        to_image,
        force,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let to_identity =
        lightning_identity_script(to_adapter, "/to", to_lightning, "/shared/to-pubkey");
    let from_cli = lightning_cli(from_adapter, "/from", from_lightning);
    if from_adapter == LightningAdapter::Cln {
        return render_cln_channel_close_job(spec, &namespace, &to_identity, &from_cli);
    }
    let close = match from_adapter {
        LightningAdapter::Lnd => {
            let force_flag = if force { " --force" } else { "" };
            format!(
                "set -eu; from='{from_cli}'; until $from getinfo >/dev/null 2>&1; do sleep 1; done; pk=$(cat /shared/to-pubkey); point=''; for candidate in $($from listchannels --peer \"$pk\" | grep -o '\"channel_point\":[[:space:]]*\"[^\"]*\"' | cut -d'\"' -f4); do digest=$(printf '%s' \"$candidate\" | sha256sum | cut -d' ' -f1); if [ \"ch-$digest\" = \"{channel_id}\" ]; then point=$candidate; break; fi; done; test -n \"$point\"; printf '%s' \"$point\" >/shared/channel-point; txid=${{point%:*}}; index=${{point##*:}}; $from closechannel{force_flag} --funding_txid=\"$txid\" --output_index=\"$index\" >/shared/close.json"
            )
        }
        LightningAdapter::Cln => {
            let unilateral_timeout = u8::from(force);
            format!(
                "set -eu; from='{from_cli}'; until $from getinfo >/dev/null 2>&1; do sleep 1; done; pk=$(cat /shared/to-pubkey); $from listpeerchannels \"$pk\" | jq -r '.channels[] | select(.funding_txid != null) | \"\\(.funding_txid):\\(.funding_outnum) \\(.channel_id)\"' >/shared/channels; point=''; native=''; while read -r candidate candidate_native; do digest=$(printf '%s' \"$candidate\" | sha256sum | cut -d' ' -f1); if [ \"ch-$digest\" = \"{channel_id}\" ]; then point=$candidate; native=$candidate_native; break; fi; done </shared/channels; test -n \"$point\"; test -n \"$native\"; printf '%s' \"$point\" >/shared/channel-point; printf '%s' \"$native\" >/shared/native-channel-id; $from close \"$native\" {unilateral_timeout} >/shared/close.json"
            )
        }
    };
    let bcli = format!(
        "bitcoin-cli -regtest -rpcconnect={chain} -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only"
    );
    let confirm = format!(
        "set -eu; until {bcli} getblockchaininfo >/dev/null 2>&1; do sleep 1; done; addr=$({bcli} -rpcwallet=default getnewaddress); {bcli} -rpcwallet=default generatetoaddress 6 \"$addr\" >/dev/null"
    );
    let verify = match from_adapter {
        LightningAdapter::Lnd => {
            let terminal_check = if force {
                "$from pendingchannels | grep -q \"$point\""
            } else {
                "$from closedchannels | grep -q \"$point\""
            };
            format!(
                "set -eu; from='{from_cli}'; point=$(cat /shared/channel-point); if $from listchannels | grep -q \"$point\"; then exit 1; fi; {terminal_check}"
            )
        }
        LightningAdapter::Cln => {
            let states = if force {
                "AWAITING_UNILATERAL|FUNDING_SPEND_SEEN|ONCHAIN"
            } else {
                "CLOSINGD_COMPLETE|FUNDING_SPEND_SEEN|ONCHAIN"
            };
            format!(
                "set -eu; from='{from_cli}'; native=$(cat /shared/native-channel-id); until state=$($from listpeerchannels | jq -r --arg id \"$native\" '.channels[] | select(.channel_id == $id) | .state'); do sleep 1; done; if printf '%s' \"$state\" | grep -q CHANNELD_NORMAL; then exit 1; fi; if test -n \"$state\"; then printf '%s' \"$state\" | grep -Eq '{states}'; else $from listclosedchannels | grep -q \"$native\"; fi"
            )
        }
    };
    let result = format!(
        "printf '%s' '{{\"from\":\"{from_lightning}\",\"to\":\"{to_lightning}\",\"channel_id\":\"{channel_id}\",\"closed\":true,\"confirmed\":true,\"force\":{force},\"pending_resolution\":{force}}}' >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [
            container("to-identity", to_image, &to_identity, &[mount("shared", "/shared", false), mount("to", "/to", true)]),
            container("channel-close", from_image, &close, &[mount("shared", "/shared", false), mount("from", "/from", true)]),
            container("channel-confirm", bitcoin_image, &confirm, &[]),
            container("channel-verify", from_image, &verify, &[mount("shared", "/shared", true), mount("from", "/from", true)])
        ],
        "containers": [container("result", "docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662", &result, &[])],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "from", "persistentVolumeClaim": {"claimName": format!("data-{from_lightning}-0")}},
            {"name": "to", "persistentVolumeClaim": {"claimName": format!("data-{to_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        if force {
            "channel-force-close"
        } else {
            "channel-close"
        },
        180,
        &pod,
    )
}

/// Run the fixed secret-bearing authentication baseline in the locked mint image.
///
/// # Panics
///
/// Panics only if the controller-owned container template stops rendering its
/// environment as a JSON array.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_authentication_conformance_job(
    spec: &AuthenticationConformanceJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let AuthenticationConformanceJobSpec {
        resource_name,
        instance_key,
        mint,
        identity_provider,
        mint_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let mint_url = format!("http://{mint}:3338");
    let script = "exec python3 -c \"$PROOFSTORM_AUTHENTICATION_DRIVER\"";
    let mut authentication = container_with_env(
        "authentication",
        mint_image,
        script,
        &[],
        vec![
            (
                "PROOFSTORM_AUTHENTICATION_DRIVER",
                AUTHENTICATION_CONFORMANCE_DRIVER,
            ),
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_IDENTITY_PROVIDER", identity_provider),
            ("PROOFSTORM_MINT_URL", mint_url.as_str()),
            ("PYTHONUNBUFFERED", "1"),
        ],
    );
    authentication["env"]
        .as_array_mut()
        .expect("authentication environment is an array")
        .extend([
            json!({
                "name": "OIDC_TEST_USERNAME",
                "valueFrom": {"secretKeyRef": {
                    "name": format!("{identity_provider}-credentials"),
                    "key": "OIDC_TEST_USERNAME"
                }}
            }),
            json!({
                "name": "OIDC_TEST_PASSWORD",
                "valueFrom": {"secretKeyRef": {
                    "name": format!("{identity_provider}-credentials"),
                    "key": "OIDC_TEST_PASSWORD"
                }}
            }),
        ]);
    // This driver handles every exception and emits a fixed diagnostic. Do not
    // fall back to container logs: an unexpected library traceback is not a
    // valid secret-bearing artifact.
    authentication["terminationMessagePolicy"] = json!("File");
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(),
        "affinity": instance_affinity(instance_key),
        "containers": [authentication]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "authentication-conformance",
        120,
        &pod,
    )
}

/// Mint and spend a BAT while keeping the token in a private termination message.
///
/// # Panics
///
/// Panics only if the controller-owned container template stops rendering its
/// environment as a JSON array.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_authentication_protected_spend_job(
    spec: &AuthenticationProtectedSpendJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let AuthenticationProtectedSpendJobSpec {
        resource_name,
        instance_key,
        mint,
        identity_provider,
        mint_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let mint_url = format!("http://{mint}:3338");
    let script = "exec python3 -c \"$PROOFSTORM_AUTHENTICATION_DRIVER\"";
    let mut authentication = container_with_env(
        "authentication",
        mint_image,
        script,
        &[],
        vec![
            (
                "PROOFSTORM_AUTHENTICATION_DRIVER",
                AUTHENTICATION_PROTECTED_SPEND_DRIVER,
            ),
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_IDENTITY_PROVIDER", identity_provider),
            ("PROOFSTORM_MINT_URL", mint_url.as_str()),
            ("PYTHONUNBUFFERED", "1"),
        ],
    );
    authentication["env"]
        .as_array_mut()
        .expect("authentication environment is an array")
        .extend(authentication_identity_environment(identity_provider));
    authentication["terminationMessagePolicy"] = json!("File");
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(),
        "affinity": instance_affinity(instance_key),
        "containers": [authentication]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "authentication-protected-spend",
        120,
        &pod,
    )
}

/// Replay a private spent BAT and prove that a fresh BAT still works.
///
/// # Panics
///
/// Panics only if the controller-owned container template stops rendering its
/// environment as a JSON array.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_authentication_replay_job(
    spec: &AuthenticationReplayJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let AuthenticationReplayJobSpec {
        resource_name,
        instance_key,
        mint,
        identity_provider,
        mint_image,
        session_secret,
        source_operation_id,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let mint_url = format!("http://{mint}:3338");
    let script = "exec python3 -c \"$PROOFSTORM_AUTHENTICATION_DRIVER\"";
    let mut authentication = container_with_env(
        "authentication",
        mint_image,
        script,
        &[],
        vec![
            (
                "PROOFSTORM_AUTHENTICATION_DRIVER",
                AUTHENTICATION_REPLAY_DRIVER,
            ),
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_IDENTITY_PROVIDER", identity_provider),
            ("PROOFSTORM_MINT_URL", mint_url.as_str()),
            ("PROOFSTORM_SOURCE_OPERATION_ID", source_operation_id),
            ("PYTHONUNBUFFERED", "1"),
        ],
    );
    let mut private_environment = authentication_identity_environment(identity_provider);
    private_environment.push(json!({
        "name": "PROOFSTORM_SPENT_BAT",
        "valueFrom": {"secretKeyRef": {
            "name": session_secret,
            "key": "SPENT_BAT"
        }}
    }));
    authentication["env"]
        .as_array_mut()
        .expect("authentication environment is an array")
        .extend(private_environment);
    authentication["terminationMessagePolicy"] = json!("File");
    let pod = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "proofstorm-workload",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "securityContext": pod_security(),
        "affinity": instance_affinity(instance_key),
        "containers": [authentication]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "authentication-replay",
        120,
        &pod,
    )
}

fn authentication_identity_environment(identity_provider: &str) -> Vec<Value> {
    vec![
        json!({
            "name": "OIDC_TEST_USERNAME",
            "valueFrom": {"secretKeyRef": {
                "name": format!("{identity_provider}-credentials"),
                "key": "OIDC_TEST_USERNAME"
            }}
        }),
        json!({
            "name": "OIDC_TEST_PASSWORD",
            "valueFrom": {"secretKeyRef": {
                "name": format!("{identity_provider}-credentials"),
                "key": "OIDC_TEST_PASSWORD"
            }}
        }),
    ]
}

/// Initialize a persistent logical wallet through its locked adapter.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_wallet_initialize_job(spec: &WalletJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let WalletJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        wallet_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let script = format!(
        "set -eu; cd /app; cashu() {{ python3 -c 'from cashu.wallet.cli.cli import cli; cli()' -h http://{mint}:3338 -u sat -w {wallet} -t -y \"$@\"; }}; balance=$(cashu balance | grep -o 'Balance: *[0-9][0-9]*' | grep -o '[0-9][0-9]*' | tail -1); test -n \"$balance\"; printf '{{\"wallet\":\"{wallet}\",\"mint\":\"{mint}\",\"initialized\":true,\"balance_sat\":%s}}' \"$balance\" >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [container_with_env("wallet", wallet_image, &script, &[mount("wallet", "/wallet", false)], vec![("HOME", "/wallet")])],
        "volumes": [{"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}}]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-initialize",
        90,
        &pod,
    )
}

/// Read a sanitized balance from a disposable snapshot of a logical wallet.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_wallet_balance_job(spec: &WalletJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let WalletJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        wallet_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let script = format!(
        "set -eu; cd /app; cashu() {{ python3 -c 'from cashu.wallet.cli.cli import cli; cli()' -h http://{mint}:3338 -u sat -w {wallet} -t -y \"$@\"; }}; balance=$(cashu balance | grep -o 'Balance: *[0-9][0-9]*' | grep -o '[0-9][0-9]*' | tail -1); test -n \"$balance\"; printf '{{\"wallet\":\"{wallet}\",\"mint\":\"{mint}\",\"balance_sat\":%s}}' \"$balance\" >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [container("snapshot", wallet_image, "set -eu; cp -R /source/. /wallet/", &[mount("source", "/source", true), mount("wallet", "/wallet", false)])],
        "containers": [container_with_env("wallet", wallet_image, &script, &[mount("wallet", "/wallet", false)], vec![("HOME", "/wallet")])],
        "volumes": [
            {"name": "source", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}},
            {"name": "wallet", "emptyDir": {}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-balance",
        90,
        &pod,
    )
}

/// Fund a persistent wallet with a bounded mint quote paid by a logical node.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_wallet_fund_job(spec: &WalletFundJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let WalletFundJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        payer_lightning,
        wallet_image,
        lightning_image,
        amount_sat,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let completion_script = format!(
        "balance=$(cashu balance | grep -o 'Balance: *[0-9][0-9]*' | grep -o '[0-9][0-9]*' | tail -1); test -n \"$balance\" || fail balance balance_unavailable; printf '{{\"wallet\":\"{wallet}\",\"mint\":\"{mint}\",\"funded_sat\":{amount_sat},\"balance_sat\":%s}}' \"$balance\" >/dev/termination-log; touch /shared/done"
    );
    let wallet_script = wallet_receive_script(wallet, mint, amount_sat, &completion_script);
    let payer_script = wallet_payer_script(payer_lightning);
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [
            container_with_env("wallet", wallet_image, &wallet_script, &[mount("shared", "/shared", false), mount("wallet", "/wallet", false)], vec![("HOME", "/wallet"), ("PYTHONUNBUFFERED", "1")]),
            container("payer", lightning_image, &payer_script, &[mount("shared", "/shared", false), mount("payer-lnd", "/payer-lnd", true)])
        ],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}},
            {"name": "payer-lnd", "persistentVolumeClaim": {"claimName": format!("data-{payer_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-fund",
        180,
        &pod,
    )
}

/// Create a receive quote and expose only its adapter-native id and sanitized
/// initial observation.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_wallet_invoice_job(
    spec: &WalletInvoiceJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let WalletInvoiceJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        wallet_image,
        amount_sat,
        timeout_seconds,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let deadline_seconds = timeout_seconds.saturating_add(30);
    let script = format!(
        "set -eu; umask 077; cd /app; output=$(mktemp /tmp/proofstorm-invoice.XXXXXX); cleanup() {{ rm -f \"$output\"; }}; trap cleanup EXIT; trap 'cleanup; exit 143' HUP INT TERM; cashu() {{ HOME=/wallet python3 -c 'from cashu.wallet.cli.cli import cli; cli()' -h http://{mint}:3338 -u sat -w {wallet} -t -y \"$@\"; }}; cashu invoice {amount_sat} --no-check >\"$output\" 2>&1; PROOFSTORM_INVOICE_OUTPUT_PATH=\"$output\" python3 -c \"$PROOFSTORM_QUOTE_DRIVER\" >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [container_with_env("wallet", wallet_image, &script, &[mount("wallet", "/wallet", false)], vec![("HOME", "/wallet"), ("PYTHONUNBUFFERED", "1"), ("PROOFSTORM_QUOTE_DRIVER", WALLET_QUOTE_DRIVER), ("PROOFSTORM_QUOTE_DRIVER_MODE", "observe-invoice"), ("PROOFSTORM_WALLET", wallet), ("PROOFSTORM_MINT", mint), ("PROOFSTORM_EXPECTED_MINT_URL", &format!("http://{mint}:3338"))])],
        "volumes": [{"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}}]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-invoice",
        i64::from(deadline_seconds),
        &pod,
    )
}

/// Pay a private receive quote from a distinct persistent wallet.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_wallet_pay_job(spec: &WalletPayJobSpec<'_>) -> Result<Job, serde_json::Error> {
    let WalletPayJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        recipient_wallet,
        recipient_mint,
        mint_quote_id,
        wallet_image,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let script = "set -eu; cd /app; python3 -c \"$PROOFSTORM_QUOTE_DRIVER\" >/dev/termination-log";
    // SQLite opens the database itself with mode=ro, but WAL readers still
    // need the mount writable so SQLite can maintain its -shm lock file.
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [container_with_env("wallet", wallet_image, script, &[mount("wallet", "/wallet", false), mount("recipient", "/recipient", false), mount("payer-mint", "/payer-mint", false)], vec![
            ("HOME", "/wallet"),
            ("PYTHONUNBUFFERED", "1"),
            ("PROOFSTORM_QUOTE_DRIVER", WALLET_QUOTE_DRIVER),
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "pay-and-claim"),
            ("PROOFSTORM_WALLET", wallet),
            ("PROOFSTORM_MINT", mint),
            ("PROOFSTORM_EXPECTED_MINT_URL", &format!("http://{mint}:3338")),
            ("PROOFSTORM_MINT_DB_DIR", "/payer-mint"),
            ("PROOFSTORM_MINT_QUOTE_ID", mint_quote_id),
            ("PROOFSTORM_RECIPIENT_HOME", "/recipient"),
            ("PROOFSTORM_RECIPIENT_WALLET", recipient_wallet),
            ("PROOFSTORM_RECIPIENT_MINT", recipient_mint),
            ("PROOFSTORM_RECIPIENT_MINT_URL", &format!("http://{recipient_mint}:3338")),
        ])],
        "volumes": [
            {"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}},
            {"name": "recipient", "persistentVolumeClaim": {"claimName": format!("{recipient_wallet}-data")}},
            {"name": "payer-mint", "persistentVolumeClaim": {"claimName": format!("{mint}-data")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-pay",
        180,
        &pod,
    )
}

/// Claim an exact recipient-side mint quote without attempting payment.
pub fn render_wallet_quote_claim_job(
    spec: &WalletQuoteClaimJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let WalletQuoteClaimJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        mint_quote_id,
        wallet_image,
        timeout_seconds,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let script = "set -eu; cd /app; python3 -c \"$PROOFSTORM_QUOTE_DRIVER\" >/dev/termination-log";
    let timeout = timeout_seconds.to_string();
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [container_with_env("wallet", wallet_image, script, &[mount("wallet", "/wallet", false)], vec![("HOME", "/wallet"), ("PYTHONUNBUFFERED", "1"), ("PROOFSTORM_QUOTE_DRIVER", WALLET_QUOTE_DRIVER), ("PROOFSTORM_QUOTE_DRIVER_MODE", "claim-receive"), ("PROOFSTORM_WALLET", wallet), ("PROOFSTORM_MINT", mint), ("PROOFSTORM_EXPECTED_MINT_URL", &format!("http://{mint}:3338")), ("PROOFSTORM_MINT_QUOTE_ID", mint_quote_id), ("PROOFSTORM_CLAIM_TIMEOUT_SECONDS", timeout.as_str())])],
        "volumes": [{"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}}]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-quote-claim",
        i64::from(timeout_seconds.saturating_add(30)),
        &pod,
    )
}

/// Refresh an exact payer-side melt quote and prove reservation release.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes Job contract cannot be decoded.
pub fn render_wallet_melt_quote_refresh_job(
    spec: &WalletMeltQuoteRefreshJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let WalletMeltQuoteRefreshJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        melt_quote_id,
        wallet_image,
        timeout_seconds,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let script = "set -eu; cd /app; python3 -c \"$PROOFSTORM_QUOTE_DRIVER\" >/dev/termination-log";
    let timeout = timeout_seconds.to_string();
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [container_with_env("wallet", wallet_image, script, &[mount("wallet", "/wallet", false)], vec![("HOME", "/wallet"), ("PYTHONUNBUFFERED", "1"), ("PROOFSTORM_QUOTE_DRIVER", WALLET_QUOTE_DRIVER), ("PROOFSTORM_QUOTE_DRIVER_MODE", "refresh-melt"), ("PROOFSTORM_WALLET", wallet), ("PROOFSTORM_MINT", mint), ("PROOFSTORM_EXPECTED_MINT_URL", &format!("http://{mint}:3338")), ("PROOFSTORM_MELT_QUOTE_ID", melt_quote_id), ("PROOFSTORM_DB_TIMEOUT_SECONDS", timeout.as_str())])],
        "volumes": [{"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}}]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-melt-quote-refresh",
        i64::from(timeout_seconds.saturating_add(30)),
        &pod,
    )
}

/// Render a disposable wallet mint-and-self-swap job with an out-of-band payer.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_wallet_round_trip_job(
    spec: &WalletRoundTripJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let WalletRoundTripJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        payer_lightning,
        wallet_image,
        lnd_image,
        amount_sat,
        tolerance_sat,
    } = *spec;
    let namespace = instance_namespace(instance_key);
    let completion_script = format!(
        "before=$(cashu balance | grep -o 'Balance: *[0-9][0-9]*' | grep -o '[0-9][0-9]*' | tail -1); test -n \"$before\" || fail balance balance_unavailable; cashu selfpay >/shared/swap.log 2>&1 || fail selfpay selfpay_failed; after=$(cashu balance | grep -o 'Balance: *[0-9][0-9]*' | grep -o '[0-9][0-9]*' | tail -1); test -n \"$after\" || fail balance balance_unavailable_after_selfpay; test \"$after\" -le \"$before\" || fail conservation balance_increased; test $((before-after)) -le {tolerance_sat} || fail conservation tolerance_exceeded; printf '{{\"minted_sat\":%s,\"balance_before_swap_sat\":%s,\"balance_after_swap_sat\":%s,\"inflation\":false}}' '{amount_sat}' \"$before\" \"$after\" >/dev/termination-log; touch /shared/done"
    );
    let wallet_script = wallet_receive_script(wallet, mint, amount_sat, &completion_script);
    let payer_script = wallet_payer_script(payer_lightning);
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "containers": [
            container_with_env("wallet", wallet_image, &wallet_script, &[mount("shared", "/shared", false), mount("wallet", "/wallet", false)], vec![("HOME", "/wallet"), ("PYTHONUNBUFFERED", "1")]),
            container("payer", lnd_image, &payer_script, &[mount("shared", "/shared", false), mount("payer-lnd", "/payer-lnd", true)])
        ],
        "volumes": [
            {"name": "shared", "emptyDir": {}},
            {"name": "wallet", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}},
            {"name": "payer-lnd", "persistentVolumeClaim": {"claimName": format!("data-{payer_lightning}-0")}}
        ]
    });
    job(
        resource_name,
        &namespace,
        instance_key,
        "wallet-round-trip",
        240,
        &pod,
    )
}

fn wallet_receive_script(
    wallet: &str,
    mint: &str,
    amount_sat: u64,
    completion_script: &str,
) -> String {
    format!(
        concat!(
            "set -eu; cd /app; pid=; watchdog_pid=; ",
            "cleanup() {{ if test -n \"$watchdog_pid\"; then kill \"$watchdog_pid\" 2>/dev/null || true; wait \"$watchdog_pid\" 2>/dev/null || true; fi; if test -n \"$pid\"; then kill \"$pid\" 2>/dev/null || true; wait \"$pid\" 2>/dev/null || true; fi; }}; ",
            "trap cleanup EXIT; trap 'cleanup; exit 143' HUP INT TERM; ",
            "fail() {{ stage=\"$1\"; reason=\"$2\"; printf '{{\"code\":\"wallet_orchestration_failed\",\"stage\":\"%s\",\"reason\":\"%s\"}}' \"$stage\" \"$reason\" >/dev/termination-log; printf '%s:%s\\n' \"$stage\" \"$reason\" >/shared/wallet.failed; exit 1; }}; ",
            "classify_log() {{ log=\"$1\"; last=$(tail -n 1 \"$log\"); if printf '%s' \"$last\" | grep -Eqi 'quote.*(not found|unknown)'; then printf quote_not_found; elif printf '%s' \"$last\" | grep -Eqi 'quote.*not paid|not paid.*quote'; then printf quote_not_paid; elif printf '%s' \"$last\" | grep -Eqi 'already.*issued|quote.*issued'; then printf quote_already_issued; elif printf '%s' \"$last\" | grep -Eqi 'database.*locked|locked.*database'; then printf wallet_database_locked; elif printf '%s' \"$last\" | grep -Eqi 'invalid.*signature|signature.*invalid'; then printf invalid_quote_signature; elif printf '%s' \"$last\" | grep -Eqi 'blind'; then printf invalid_blinded_output; elif printf '%s' \"$last\" | grep -Eqi 'proof'; then printf proof_error; elif printf '%s' \"$last\" | grep -Eqi 'keyset'; then printf keyset_error; elif printf '%s' \"$last\" | grep -Eqi 'amount|unit'; then printf amount_or_unit_error; elif printf '%s' \"$last\" | grep -Eqi 'connect|connection|timed out|timeout'; then printf mint_connection_failed; else printf command_failed; fi; }}; ",
            "run_bounded() {{ duration=\"$1\"; marker=\"$2\"; shift 2; rm -f \"$marker\"; \"$@\" & pid=$!; (sleep \"$duration\"; if kill -0 \"$pid\" 2>/dev/null; then touch \"$marker\"; kill \"$pid\" 2>/dev/null || true; sleep 2; kill -9 \"$pid\" 2>/dev/null || true; fi) & watchdog_pid=$!; if wait \"$pid\"; then command_rc=0; else command_rc=$?; fi; pid=; kill \"$watchdog_pid\" 2>/dev/null || true; wait \"$watchdog_pid\" 2>/dev/null || true; watchdog_pid=; return \"$command_rc\"; }}; ",
            "cashu() {{ python3 -c 'from cashu.wallet.cli.cli import cli; cli()' -h http://{mint}:3338 -u sat -w {wallet} -t -y \"$@\"; }}; ",
            "if run_bounded 30 /shared/invoice-request.timed-out cashu invoice {amount_sat} --no-check >/shared/invoice.log 2>&1; then :; else test ! -f /shared/invoice-request.timed-out || fail invoice invoice_request_timeout; invoice_reason=$(classify_log /shared/invoice.log); fail invoice \"$invoice_reason\"; fi; ",
            "quote_id=$(sed -n 's/.*--id \\([^[:space:]]*\\).*/\\1/p' /shared/invoice.log | tail -1); test -n \"$quote_id\" || fail invoice quote_id_not_observed; ",
            "elapsed=0; until test -f /shared/paid; do test ! -f /shared/payer.failed || fail payment payer_failed; elapsed=$((elapsed+1)); test \"$elapsed\" -lt 105 || fail payment payment_wait_timeout; sleep 1; done; ",
            "if run_bounded 35 /shared/invoice-settlement.timed-out cashu invoice {amount_sat} --id \"$quote_id\" >/shared/settlement.log 2>&1; then :; else test ! -f /shared/invoice-settlement.timed-out || fail settlement invoice_settlement_timeout; settlement_reason=$(classify_log /shared/settlement.log); fail settlement \"$settlement_reason\"; fi; ",
            "{completion_script}"
        ),
        mint = mint,
        wallet = wallet,
        amount_sat = amount_sat,
        completion_script = completion_script
    )
}

fn wallet_payer_script(payer_lightning: &str) -> String {
    format!(
        "set -eu; pay_pid=; watchdog_pid=; cleanup() {{ if test -n \"$watchdog_pid\"; then kill \"$watchdog_pid\" 2>/dev/null || true; wait \"$watchdog_pid\" 2>/dev/null || true; fi; if test -n \"$pay_pid\"; then kill \"$pay_pid\" 2>/dev/null || true; wait \"$pay_pid\" 2>/dev/null || true; fi; }}; trap cleanup EXIT; trap 'cleanup; exit 143' HUP INT TERM; fail() {{ stage=\"$1\"; reason=\"$2\"; printf '{{\"code\":\"wallet_orchestration_failed\",\"stage\":\"%s\",\"reason\":\"%s\"}}' \"$stage\" \"$reason\" >/dev/termination-log; printf '%s:%s\\n' \"$stage\" \"$reason\" >/shared/payer.failed; exit 1; }}; elapsed=0; while :; do if invoice=$(grep -Eo 'ln(bcrt|bc|tb|tbs)[0-9a-z]+' /shared/invoice.log 2>/dev/null | head -1) && test -n \"$invoice\"; then break; fi; test ! -f /shared/wallet.failed || fail invoice wallet_failed; elapsed=$((elapsed+1)); test \"$elapsed\" -lt 75 || fail invoice invoice_not_observed; sleep 1; done; lncli --lnddir=/payer-lnd --network=regtest --rpcserver={payer_lightning}:10009 payinvoice --force \"$invoice\" >/tmp/payment.log 2>&1 & pay_pid=$!; (sleep 60; if kill -0 \"$pay_pid\" 2>/dev/null; then touch /shared/payment.timed-out; kill \"$pay_pid\" 2>/dev/null || true; sleep 2; kill -9 \"$pay_pid\" 2>/dev/null || true; fi) & watchdog_pid=$!; set +e; wait \"$pay_pid\"; pay_rc=$?; set -e; pay_pid=; kill \"$watchdog_pid\" 2>/dev/null || true; wait \"$watchdog_pid\" 2>/dev/null || true; watchdog_pid=; test ! -f /shared/payment.timed-out || fail payment payment_timeout; test \"$pay_rc\" -eq 0 || fail payment payment_failed; touch /shared/paid; elapsed=0; until test -f /shared/done; do test ! -f /shared/wallet.failed || fail settlement wallet_failed_after_payment; elapsed=$((elapsed+1)); test \"$elapsed\" -lt 35 || fail settlement wallet_completion_timeout; sleep 1; done"
    )
}

/// Render a read-only-wallet conservation oracle job.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_conservation_oracle_job(
    spec: &ConservationOracleJobSpec<'_>,
) -> Result<Job, serde_json::Error> {
    let ConservationOracleJobSpec {
        resource_name,
        instance_key,
        wallet,
        mint,
        wallet_image,
        baseline_operation_id,
        treatment_operation_id,
        expected_sat,
        tolerance_sat,
    } = spec;
    let namespace = instance_namespace(instance_key);
    let script = format!(
        "set -eu; cd /app; cashu() {{ python3 -c 'from cashu.wallet.cli.cli import cli; cli()' -h http://{mint}:3338 -u sat -w {wallet} -t -y \"$@\"; }}; actual=$(cashu balance | grep -o 'Balance: *[0-9][0-9]*' | grep -o '[0-9][0-9]*' | tail -1); test -n \"$actual\"; delta=$((actual-{expected_sat})); test \"$delta\" -ge 0 || delta=$((-delta)); conserved=false; test \"$delta\" -le {tolerance_sat} && conserved=true; printf '{{\"baseline_operation_id\":\"{baseline_operation_id}\",\"treatment_operation_id\":\"{treatment_operation_id}\",\"expected_sat\":{expected_sat},\"actual_sat\":%s,\"tolerance_sat\":{tolerance_sat},\"conserved\":%s}}' \"$actual\" \"$conserved\" >/dev/termination-log"
    );
    let pod = json!({
        "restartPolicy": "Never", "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
        "securityContext": pod_security(), "affinity": instance_affinity(instance_key),
        "initContainers": [container("snapshot", wallet_image, "set -eu; cp -R /source/. /wallet/", &[mount("source", "/source", true), mount("wallet", "/wallet", false)])],
        "containers": [container_with_env("oracle", wallet_image, &script, &[mount("wallet", "/wallet", false)], vec![("HOME", "/wallet")])],
        "volumes": [
            {"name": "source", "persistentVolumeClaim": {"claimName": format!("{wallet}-data")}},
            {"name": "wallet", "emptyDir": {}}
        ]
    });
    job(resource_name, &namespace, instance_key, "oracle", 120, &pod)
}

fn job(
    name: &str,
    namespace: &str,
    instance_key: &str,
    operation: &str,
    deadline_seconds: i64,
    pod: &Value,
) -> Result<Job, serde_json::Error> {
    resource(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": metadata(name, namespace, instance_key, operation),
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": deadline_seconds,
            "ttlSecondsAfterFinished": 600,
            "template": {
                "metadata": {"labels": labels(instance_key, operation)},
                "spec": pod
            }
        }
    }))
}

fn sats_to_btc(sats: u64) -> String {
    format!("{}.{:08}", sats / 100_000_000, sats % 100_000_000)
}

fn metadata(name: &str, namespace: &str, instance_key: &str, operation: &str) -> Value {
    json!({"name": name, "namespace": namespace, "labels": labels(instance_key, operation)})
}

fn labels(instance_key: &str, operation: &str) -> Value {
    json!({"proofstorm.dev/instance": instance_key, "proofstorm.dev/operation": operation,
        "app.kubernetes.io/managed-by": "proofstorm-mcp"})
}

fn pod_security() -> Value {
    json!({"runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000, "fsGroup": 1000,
        "seccompProfile": {"type": "RuntimeDefault"}})
}

fn instance_affinity(instance_key: &str) -> Value {
    json!({"podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
        "labelSelector": {"matchLabels": {"proofstorm.dev/instance": instance_key}},
        "topologyKey": "kubernetes.io/hostname"
    }]}})
}

fn mount(name: &str, path: &str, read_only: bool) -> Value {
    json!({"name": name, "mountPath": path, "readOnly": read_only})
}

fn container(name: &str, image: &str, script: &str, mounts: &[Value]) -> Value {
    container_with_env(name, image, script, mounts, Vec::<(&str, &str)>::new())
}

fn container_with_env<N, V>(
    name: &str,
    image: &str,
    script: &str,
    mounts: &[Value],
    environment: Vec<(N, V)>,
) -> Value
where
    N: AsRef<str>,
    V: AsRef<str>,
{
    // Typed scripts send native command output to the Pod log, which the
    // controller does not read for a failed action. FallbackToLogsOnError makes
    // Kubernetes itself populate the termination message from that log whenever
    // a container exits non-zero without writing its own diagnostic, so a
    // failure carries the native error instead of a bare exit code.
    json!({"name": name, "image": image, "imagePullPolicy": "IfNotPresent",
        "command": ["/bin/sh", "-c", script], "volumeMounts": mounts,
        "env": environment.into_iter().map(|(name, value)| json!({"name": name.as_ref(), "value": value.as_ref()})).collect::<Vec<_>>(),
        "terminationMessagePolicy": "FallbackToLogsOnError",
        "securityContext": {"allowPrivilegeEscalation": false, "capabilities": {"drop": ["ALL"]}}})
}

fn resource(value: Value) -> Result<Job, serde_json::Error> {
    serde_json::from_value(value)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::process::Command;

    use proofstorm_core::{
        API_VERSION, ComponentCondition, ComponentSpec, ComponentStatus, ControlClass, LabPolicy,
        LabSpec, default_catalog, resolve_lock,
    };

    use super::*;

    #[cfg(unix)]
    fn assert_shell_syntax(script: &str) {
        assert!(
            Command::new("/bin/sh")
                .args(["-n", "-c", script])
                .status()
                .expect("shell syntax check")
                .success()
        );
    }

    fn typed_bootstrap() -> (ProofstormLab, ProofstormLabAction) {
        let component = |id: &str, kind: ComponentKind, implementation: &str| ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: match implementation {
                "bitcoin-core" => "bitcoin-core/30/v1",
                "lnd" => "lnd/0.20/v1",
                "cln" => "cln/26.06/v1",
                "cdk" => "cdk-mintd/0.18/v1",
                "nutshell-wallet" => "nutshell-wallet/0.20/v1",
                _ => panic!("unknown test implementation {implementation:?}"),
            }
            .into(),
            control: if kind == ComponentKind::Mint {
                ControlClass::Target
            } else {
                ControlClass::Laboratory
            },
            config: BTreeMap::new(),
        };
        let lab_spec = LabSpec {
            api_version: API_VERSION.into(),
            name: "action-lab".into(),
            components: vec![
                component("chain", ComponentKind::Bitcoin, "bitcoin-core"),
                component("chain-b", ComponentKind::Bitcoin, "bitcoin-core"),
                component("mint-lnd", ComponentKind::Lightning, "lnd"),
                component("payer-lnd", ComponentKind::Lightning, "lnd"),
                component("attacker-cln", ComponentKind::Lightning, "cln"),
                component("mint", ComponentKind::Mint, "cdk"),
                component("wallet", ComponentKind::Wallet, "nutshell-wallet"),
            ],
            links: vec![proofstorm_core::LinkSpec {
                id: "mint-bolt11".into(),
                kind: proofstorm_core::LinkKind::PaymentBackend,
                from: "mint".into(),
                to: "mint-lnd".into(),
                binding: Some(proofstorm_core::DependencyBinding::Payment {
                    method: proofstorm_core::PaymentMethod::Bolt11,
                    unit: "sat".into(),
                }),
            }],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab_spec, default_catalog()).expect("lock");
        let lab = ProofstormLab::new(
            "lab-resource",
            crate::ProofstormLabSpec {
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                revision_digest: "sha256:revision".into(),
                lock,
                lab: lab_spec,
            },
        );
        let action = ProofstormLabAction::new(
            "action-123",
            crate::ProofstormLabActionSpec {
                lab_name: "lab-resource".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                experiment_id: "experiment".into(),
                lease_id: "lease".into(),
                principal_id: "principal".into(),
                sequence: 1,
                operation_id: "bootstrap".into(),
                request_digest: "sha256:request".into(),
                capability: Capability::WalletFund,
                accepted_at_unix: 1,
                action: LabAction::BootstrapLiquidity(BootstrapLiquidityAction {
                    chain: "chain".into(),
                    mint_lightning: "mint-lnd".into(),
                    payer_lightning: "payer-lnd".into(),
                    funding_sat: 100_000_000,
                    channel_sat: 10_000_000,
                    push_sat: 5_000_000,
                }),
            },
        );
        (lab, action)
    }

    fn ready_admission_status(lab: &mut ProofstormLab) {
        lab.metadata.annotations.get_or_insert_default().insert(
            crate::PROTOCOL_PROBER_LEASE_ANNOTATION.into(),
            "lease-current".into(),
        );
        let plans = crate::compile_component_plans(
            &lab.spec.instance_key,
            &lab.spec.revision_digest,
            &lab.spec.lab,
            &lab.spec.lock,
        )
        .expect("plans");
        let components = plans
            .iter()
            .map(|plan| ComponentStatus {
                id: plan.component_id.clone(),
                kind: plan.kind,
                observed_revision_digest: plan.revision_digest.clone(),
                observed_rollout_digest: plan.rollout_digest.clone(),
                conditions: plan
                    .applicable_conditions
                    .iter()
                    .map(|condition_type| ComponentCondition {
                        condition_type: *condition_type,
                        state: ComponentConditionState::True,
                        reason: match condition_type {
                            ComponentConditionType::WorkloadReady => {
                                ComponentConditionReason::WorkloadAvailable
                            }
                            ComponentConditionType::StorageReady => {
                                ComponentConditionReason::StorageBound
                            }
                            ComponentConditionType::CredentialsReady => {
                                ComponentConditionReason::CredentialsProjected
                            }
                            ComponentConditionType::ServiceReady => {
                                ComponentConditionReason::EndpointsReady
                            }
                            ComponentConditionType::ProtocolReady => {
                                ComponentConditionReason::ProtocolResponding
                            }
                            ComponentConditionType::DependenciesReady => {
                                ComponentConditionReason::DependenciesSatisfied
                            }
                            ComponentConditionType::ComponentReady => {
                                ComponentConditionReason::ComponentOperational
                            }
                            ComponentConditionType::ExperimentControllable => {
                                ComponentConditionReason::ControlAvailable
                            }
                        },
                        message: "ready".into(),
                        last_transition_unix: 1,
                    })
                    .collect(),
                ready: true,
                service: format!("{}.instance.svc", plan.component_id),
                ports: plan.target_descriptor.ports.clone(),
            })
            .collect();
        lab.status = Some(crate::ProofstormLabStatus {
            phase: crate::LabPhase::Ready,
            observed_revision_digest: lab.spec.revision_digest.clone(),
            observed_protocol_probe_lease: Some("lease-current".into()),
            components,
            ..crate::ProofstormLabStatus::default()
        });
    }

    fn set_condition(
        lab: &mut ProofstormLab,
        component: &str,
        condition_type: ComponentConditionType,
        state: ComponentConditionState,
        reason: ComponentConditionReason,
    ) {
        let condition = lab
            .status
            .as_mut()
            .expect("status")
            .components
            .iter_mut()
            .find(|status| status.id == component)
            .expect("component status")
            .conditions
            .iter_mut()
            .find(|condition| condition.condition_type == condition_type)
            .expect("condition");
        condition.state = state;
        condition.reason = reason;
    }

    #[test]
    fn admission_uses_operation_prerequisites_instead_of_lab_ready() {
        let (mut lab, mut action) = typed_bootstrap();

        action.spec.action = LabAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "mint-lnd".into(),
            script: "bitcoin-cli -help".into(),
            timeout_seconds: 30,
        });
        assert!(
            evaluate_action_admission(&action, &lab).is_ok(),
            "immutable execution and target contracts do not require lab readiness"
        );

        ready_admission_status(&mut lab);
        lab.status.as_mut().expect("status").phase = crate::LabPhase::Pending;
        set_condition(
            &mut lab,
            "chain",
            ComponentConditionType::ProtocolReady,
            ComponentConditionState::False,
            ComponentConditionReason::ProtocolProbeFailed,
        );
        action.spec.action = LabAction::NodeStart(crate::NodeControlAction {
            component: "chain".into(),
        });
        assert!(
            evaluate_action_admission(&action, &lab).is_ok(),
            "start depends on storage, not protocol or aggregate lab phase"
        );

        set_condition(
            &mut lab,
            "chain",
            ComponentConditionType::StorageReady,
            ComponentConditionState::False,
            ComponentConditionReason::StoragePending,
        );
        assert!(matches!(
            evaluate_action_admission(&action, &lab),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::Storage,
                ..
            })
        ));
    }

    #[test]
    fn admission_allows_stopped_recovery_and_rejects_unhealthy_mutation() {
        let (mut lab, mut action) = typed_bootstrap();
        ready_admission_status(&mut lab);
        set_condition(
            &mut lab,
            "mint-lnd",
            ComponentConditionType::WorkloadReady,
            ComponentConditionState::False,
            ComponentConditionReason::IntentionallyStopped,
        );
        set_condition(
            &mut lab,
            "mint-lnd",
            ComponentConditionType::ProtocolReady,
            ComponentConditionState::False,
            ComponentConditionReason::IntentionallyStopped,
        );

        action.spec.action = LabAction::NodeRestart(crate::NodeControlAction {
            component: "mint-lnd".into(),
        });
        assert!(evaluate_action_admission(&action, &lab).is_ok());

        action.spec.action = LabAction::PeerConnect(PeerConnectAction {
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
        });
        assert_eq!(
            evaluate_action_admission(&action, &lab),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                component: "mint-lnd".into(),
                operation: OperationClass::PeerChannelMutation,
                prerequisite: ReadinessPrerequisite::Protocol,
                condition: Some(ComponentConditionType::ProtocolReady),
                state: Some(ComponentConditionState::False),
                reason: Some(ComponentConditionReason::IntentionallyStopped),
            })
        );
    }

    #[test]
    fn workload_identity_rejects_stale_status_but_network_control_does_not() {
        let (mut lab, mut action) = typed_bootstrap();
        ready_admission_status(&mut lab);
        lab.status
            .as_mut()
            .expect("status")
            .components
            .iter_mut()
            .find(|status| status.id == "chain")
            .expect("chain")
            .observed_rollout_digest = "sha256:stale".into();

        action.spec.action = LabAction::NodeStop(crate::NodeControlAction {
            component: "chain".into(),
        });
        assert!(matches!(
            evaluate_action_admission(&action, &lab),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::WorkloadIdentity,
                ..
            })
        ));

        action.spec.action = LabAction::NetworkPartition(crate::NetworkPartitionAction {
            from_component: "chain".into(),
            to_component: "mint-lnd".into(),
        });
        assert!(evaluate_action_admission(&action, &lab).is_ok());
    }

    #[test]
    fn read_only_wallet_inspection_survives_mint_protocol_failure() {
        let (mut lab, mut action) = typed_bootstrap();
        ready_admission_status(&mut lab);
        set_condition(
            &mut lab,
            "mint",
            ComponentConditionType::ProtocolReady,
            ComponentConditionState::False,
            ComponentConditionReason::ProtocolProbeFailed,
        );

        action.spec.action = LabAction::WalletBalance(crate::WalletBalanceAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
        });
        assert!(evaluate_action_admission(&action, &lab).is_ok());

        action.spec.action = LabAction::WalletFund(crate::WalletFundAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
            payer_lightning: "payer-lnd".into(),
            amount_sat: 100,
        });
        assert_eq!(
            evaluate_action_admission(&action, &lab),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                component: "mint".into(),
                operation: OperationClass::WalletPayment,
                prerequisite: ReadinessPrerequisite::Protocol,
                condition: Some(ComponentConditionType::ProtocolReady),
                state: Some(ComponentConditionState::False),
                reason: Some(ComponentConditionReason::ProtocolProbeFailed),
            })
        );
    }

    #[test]
    fn stale_lab_revision_fences_runtime_admission_only() {
        let (mut lab, mut action) = typed_bootstrap();
        ready_admission_status(&mut lab);
        lab.status
            .as_mut()
            .expect("status")
            .observed_revision_digest = "sha256:previous-revision".into();

        action.spec.action = LabAction::NodeRestart(crate::NodeControlAction {
            component: "chain".into(),
        });
        assert!(matches!(
            evaluate_action_admission(&action, &lab),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::WorkloadIdentity,
                state: None,
                reason: None,
                ..
            })
        ));

        action.spec.action = LabAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "chain-b".into(),
            script: "bitcoin-cli -help".into(),
            timeout_seconds: 30,
        });
        assert!(
            evaluate_action_admission(&action, &lab).is_ok(),
            "a newly compiled immutable execution contract does not consume stale status"
        );
    }

    #[test]
    fn scheduler_lease_fences_protocol_admission_without_blocking_recovery() {
        let (mut lab, mut action) = typed_bootstrap();
        ready_admission_status(&mut lab);
        lab.metadata
            .annotations
            .as_mut()
            .expect("annotations")
            .insert(
                crate::PROTOCOL_PROBER_LEASE_ANNOTATION.into(),
                "inactive".into(),
            );

        action.spec.action = LabAction::PeerConnect(PeerConnectAction {
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
        });
        assert!(matches!(
            evaluate_action_admission(&action, &lab),
            Err(ActionAdmissionError::PrerequisiteUnsatisfied {
                prerequisite: ReadinessPrerequisite::Dependencies | ReadinessPrerequisite::Protocol,
                state: None,
                reason: None,
                ..
            })
        ));

        action.spec.action = LabAction::NodeStart(crate::NodeControlAction {
            component: "mint-lnd".into(),
        });
        assert!(evaluate_action_admission(&action, &lab).is_ok());
        action.spec.action = LabAction::ComponentForensics(ComponentForensicsAction {
            component: "mint-lnd".into(),
            target_component: "payer-lnd".into(),
            script: "lncli --help".into(),
            timeout_seconds: 30,
        });
        assert!(evaluate_action_admission(&action, &lab).is_ok());
    }

    #[test]
    fn typed_job_containers_fall_back_to_logs_for_their_diagnostic() {
        let job = render_bootstrap_job(&BootstrapJobSpec {
            resource_name: "op-boot",
            instance_key: "i0123456789012345678",
            chain: "chain",
            mint_lightning: "mint-lnd",
            payer_lightning: "payer-lnd",
            bitcoin_image: "bitcoin",
            lnd_image: "lnd",
            funding_sat: 100_000_000,
            channel_sat: 10_000_000,
            push_sat: 5_000_000,
        })
        .expect("job");
        let pod = job.spec.expect("spec").template.spec.expect("pod");
        let containers = pod
            .init_containers
            .iter()
            .flatten()
            .chain(pod.containers.iter());
        let mut observed = 0;
        for container in containers {
            observed += 1;
            assert_eq!(
                container.termination_message_policy.as_deref(),
                Some("FallbackToLogsOnError"),
                "container {} must surface its native error on failure",
                container.name
            );
        }
        assert!(observed > 1, "the bootstrap renders staged containers");
    }

    #[test]
    fn authentication_conformance_job_keeps_credentials_in_secret_refs() {
        let job = render_authentication_conformance_job(&AuthenticationConformanceJobSpec {
            resource_name: "op-auth",
            instance_key: "i0123456789012345678",
            mint: "mint",
            identity_provider: "identity",
            mint_image: "nutshell-image",
        })
        .expect("authentication conformance job");
        let encoded = serde_json::to_value(&job).expect("job JSON");
        let pod = &encoded["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        let container = &pod["containers"][0];
        assert_eq!(container["name"], "authentication");
        assert_eq!(container["image"], "nutshell-image");
        assert_eq!(container["terminationMessagePolicy"], "File");
        let environment = container["env"].as_array().expect("environment");
        for (name, key) in [
            ("OIDC_TEST_USERNAME", "OIDC_TEST_USERNAME"),
            ("OIDC_TEST_PASSWORD", "OIDC_TEST_PASSWORD"),
        ] {
            let variable = environment
                .iter()
                .find(|entry| entry["name"] == name)
                .expect("secret environment variable");
            assert_eq!(
                variable["valueFrom"]["secretKeyRef"]["name"],
                "identity-credentials"
            );
            assert_eq!(variable["valueFrom"]["secretKeyRef"]["key"], key);
            assert!(variable.get("value").is_none());
        }
        let driver = environment
            .iter()
            .find(|entry| entry["name"] == "PROOFSTORM_AUTHENTICATION_DRIVER")
            .and_then(|entry| entry["value"].as_str())
            .expect("fixed driver");
        assert!(driver.contains("proofstorm/authentication-conformance/v1"));
        assert!(driver.contains("os.environ[\"OIDC_TEST_PASSWORD\"]"));
        assert!(!driver.contains("traceback"));

        let protected =
            render_authentication_protected_spend_job(&AuthenticationProtectedSpendJobSpec {
                resource_name: "op-auth-spend",
                instance_key: "i0123456789012345678",
                mint: "mint",
                identity_provider: "identity",
                mint_image: "nutshell-image",
            })
            .expect("protected spend job");
        let protected = serde_json::to_value(protected).expect("protected job JSON");
        let protected_container = &protected["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(protected_container["terminationMessagePolicy"], "File");
        assert!(
            protected_container["env"]
                .as_array()
                .expect("environment")
                .iter()
                .any(|entry| entry["name"] == "OIDC_TEST_PASSWORD"
                    && entry["valueFrom"]["secretKeyRef"]["name"] == "identity-credentials")
        );

        let replay = render_authentication_replay_job(&AuthenticationReplayJobSpec {
            resource_name: "op-auth-replay",
            instance_key: "i0123456789012345678",
            mint: "mint",
            identity_provider: "identity",
            mint_image: "nutshell-image",
            session_secret: "op-source-auth-session",
            source_operation_id: "auth-source",
        })
        .expect("authentication replay job");
        let replay = serde_json::to_value(replay).expect("replay job JSON");
        let replay_pod = &replay["spec"]["template"]["spec"];
        assert_eq!(replay_pod["automountServiceAccountToken"], false);
        let replay_environment = replay_pod["containers"][0]["env"]
            .as_array()
            .expect("replay environment");
        let spent_bat = replay_environment
            .iter()
            .find(|entry| entry["name"] == "PROOFSTORM_SPENT_BAT")
            .expect("private spent BAT");
        assert_eq!(
            spent_bat["valueFrom"]["secretKeyRef"]["name"],
            "op-source-auth-session"
        );
        assert_eq!(spent_bat["valueFrom"]["secretKeyRef"]["key"], "SPENT_BAT");
        assert!(spent_bat.get("value").is_none());
    }

    #[test]
    fn wallet_pay_job_uses_exact_private_quote_and_claim_driver() {
        let job = render_wallet_pay_job(&WalletPayJobSpec {
            resource_name: "op-pay",
            instance_key: "i0123456789012345678",
            wallet: "wallet-b",
            mint: "mint",
            recipient_wallet: "wallet-a",
            recipient_mint: "mint",
            mint_quote_id: "quote-1",
            wallet_image: "nutshell",
        })
        .expect("pay job");
        let pod = job.spec.expect("spec").template.spec.expect("pod");
        let container = &pod.containers[0];
        let script = container
            .command
            .as_ref()
            .expect("command")
            .last()
            .expect("script");
        assert!(
            !script.contains("\"phase\":\"paid\""),
            "the pay script must not assert settlement itself"
        );
        assert!(script.contains("python3 -c \"$PROOFSTORM_QUOTE_DRIVER\""));
        #[cfg(unix)]
        assert_shell_syntax(script);
        let env = container.env.as_ref().expect("env");
        let driver = env
            .iter()
            .find(|variable| variable.name == "PROOFSTORM_QUOTE_DRIVER")
            .and_then(|variable| variable.value.as_deref())
            .expect("settlement driver shipped by environment");
        assert!(driver.contains("bolt11_melt_quotes"));
        assert!(driver.contains("FROM melt_quotes"));
        assert!(driver.contains("melt_quote_missing"));
        for (name, value) in [
            ("PROOFSTORM_MINT_QUOTE_ID", "quote-1"),
            ("PROOFSTORM_WALLET", "wallet-b"),
            ("PROOFSTORM_RECIPIENT_WALLET", "wallet-a"),
            ("PROOFSTORM_RECIPIENT_MINT", "mint"),
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "pay-and-claim"),
            ("PROOFSTORM_MINT_DB_DIR", "/payer-mint"),
        ] {
            assert_eq!(
                env.iter()
                    .find(|variable| variable.name == name)
                    .and_then(|variable| variable.value.as_deref()),
                Some(value),
                "{name}"
            );
        }
        let mint_mount = container
            .volume_mounts
            .as_ref()
            .expect("volume mounts")
            .iter()
            .find(|mount| mount.name == "payer-mint")
            .expect("payer mint database mount");
        assert_eq!(mint_mount.mount_path, "/payer-mint");
        assert_eq!(mint_mount.read_only, Some(false));
    }

    #[test]
    fn wallet_melt_refresh_is_exact_bounded_and_uses_the_wallet_identity() {
        let job = render_wallet_melt_quote_refresh_job(&WalletMeltQuoteRefreshJobSpec {
            resource_name: "op-refresh",
            instance_key: "i0123456789012345678",
            wallet: "payer-wallet",
            mint: "payer-mint",
            melt_quote_id: "melt-opaque-1",
            wallet_image: "nutshell-wallet",
            timeout_seconds: 45,
        })
        .expect("refresh job");
        let spec = job.spec.expect("job spec");
        assert_eq!(spec.active_deadline_seconds, Some(75));
        let pod = spec.template.spec.expect("pod spec");
        assert_eq!(pod.automount_service_account_token, Some(false));
        let container = &pod.containers[0];
        let env = container.env.as_ref().expect("environment");
        for (name, value) in [
            ("HOME", "/wallet"),
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "refresh-melt"),
            ("PROOFSTORM_WALLET", "payer-wallet"),
            ("PROOFSTORM_MINT", "payer-mint"),
            ("PROOFSTORM_EXPECTED_MINT_URL", "http://payer-mint:3338"),
            ("PROOFSTORM_MELT_QUOTE_ID", "melt-opaque-1"),
        ] {
            assert_eq!(
                env.iter()
                    .find(|variable| variable.name == name)
                    .and_then(|variable| variable.value.as_deref()),
                Some(value),
                "{name}"
            );
        }
        assert_eq!(
            pod.volumes
                .as_ref()
                .and_then(|volumes| volumes[0].persistent_volume_claim.as_ref())
                .map(|claim| claim.claim_name.as_str()),
            Some("payer-wallet-data")
        );
    }

    #[test]
    fn bounded_jobs_have_deadlines_and_no_service_account_tokens() {
        let job = render_bootstrap_job(&BootstrapJobSpec {
            resource_name: "op-123",
            instance_key: "i0123456789012345678",
            chain: "chain",
            mint_lightning: "mint-lnd",
            payer_lightning: "payer-lnd",
            bitcoin_image: "bitcoin",
            lnd_image: "lnd",
            funding_sat: 100_000_000,
            channel_sat: 10_000_000,
            push_sat: 5_000_000,
        })
        .expect("job");
        assert_eq!(
            job.spec
                .as_ref()
                .and_then(|spec| spec.active_deadline_seconds),
            Some(300)
        );
        let pod = &job.spec.expect("spec").template.spec.expect("pod");
        assert_eq!(pod.automount_service_account_token, Some(false));
    }

    #[test]
    fn native_exec_uses_locked_component_image_data_and_uninterpolated_script() {
        let (lab, mut action) = typed_bootstrap();
        let locked_bitcoin = lab
            .spec
            .lock
            .entries
            .iter()
            .find(|entry| entry.component_id == "chain")
            .expect("bitcoin lock")
            .image
            .clone();
        let script = "bitcoin-cli --help; printf '%s' '$NOT_EXPANDED_BY_RENDERER'";
        action.spec.capability = Capability::ComponentForensics;
        action.spec.action = LabAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "chain".into(),
            script: script.into(),
            timeout_seconds: 30,
        });

        let job = render_lab_action_job(&action, &lab).expect("native exec job");
        let spec = job.spec.as_ref().expect("job spec");
        assert_eq!(spec.active_deadline_seconds, Some(40));
        let pod = spec.template.spec.as_ref().expect("pod");
        assert_eq!(pod.automount_service_account_token, Some(false));
        let exec = &pod.containers[0];
        assert_eq!(exec.name, "exec");
        assert_eq!(exec.image.as_deref(), Some(locked_bitcoin.as_str()));
        assert!(exec.command.as_ref().expect("wrapper command")[2].contains("$PROOFSTORM_SCRIPT"));
        assert!(!exec.command.as_ref().expect("wrapper command")[2].contains(script));
        assert_eq!(
            exec.env
                .as_ref()
                .expect("exec environment")
                .iter()
                .find(|entry| entry.name == "PROOFSTORM_SCRIPT")
                .and_then(|entry| entry.value.as_deref()),
            Some(script)
        );
        assert_eq!(
            pod.volumes.as_ref().expect("data volume")[0]
                .persistent_volume_claim
                .as_ref()
                .map(|claim| claim.claim_name.as_str()),
            Some("data-chain-0")
        );
        assert_eq!(action_result_container(&action.spec.action), "forensics");
        let pod_labels = job
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.labels.as_ref())
            .expect("pod labels");
        assert_eq!(
            pod_labels
                .get("proofstorm.dev/network-identity")
                .map(String::as_str),
            Some("chain")
        );
        assert!(!pod_labels.contains_key("proofstorm.dev/component"));
        assert!(!pod_labels.contains_key("proofstorm.dev/operation"));
    }

    #[test]
    fn native_exec_can_target_a_distinct_bitcoin_component() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::ComponentForensics;
        action.spec.action = LabAction::ComponentForensics(ComponentForensicsAction {
            component: "chain".into(),
            target_component: "chain-b".into(),
            script: "bitcoin-cli getblockchaininfo".into(),
            timeout_seconds: 30,
        });

        let job = render_lab_action_job(&action, &lab).expect("targeted native exec job");
        let pod = job
            .spec
            .as_ref()
            .expect("job spec")
            .template
            .spec
            .as_ref()
            .expect("pod spec");
        let environment = pod.containers[0]
            .env
            .as_ref()
            .expect("target environment")
            .iter()
            .map(|entry| {
                (
                    entry.name.as_str(),
                    entry.value.as_deref().unwrap_or_default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(environment["PROOFSTORM_EXEC_COMPONENT"], "chain");
        assert_eq!(environment["PROOFSTORM_TARGET_COMPONENT"], "chain-b");
        assert_eq!(environment["PROOFSTORM_TARGET_HOST"], "chain-b");
        assert_eq!(
            environment["PROOFSTORM_TARGET_FQDN"],
            "chain-b.proofstorm-i0123456789012345678.svc"
        );
        assert_eq!(environment["PROOFSTORM_TARGET_PORT_RPC"], "18443");
        assert_eq!(environment["BITCOIN_RPC_HOST"], "chain-b");
        assert_eq!(environment["BITCOIN_RPC_PORT"], "18443");
        assert_eq!(environment["BITCOIN_RPC_USER"], "proofstorm");
        assert_eq!(
            environment["BITCOIN_RPC_PASSWORD"],
            "proofstorm-regtest-only"
        );
        let labels = job
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.labels.as_ref())
            .expect("pod labels");
        assert_eq!(labels["proofstorm.dev/network-identity"], "chain");
        assert!(!labels.contains_key("proofstorm.dev/component"));
    }

    #[test]
    fn native_exec_mounts_are_compiled_from_the_executor_plan() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::ComponentForensics;
        action.spec.action = LabAction::ComponentForensics(ComponentForensicsAction {
            component: "mint".into(),
            target_component: "chain-b".into(),
            script: "cdk-mintd --help".into(),
            timeout_seconds: 30,
        });

        let job = render_lab_action_job(&action, &lab).expect("mint native exec");
        let pod = job
            .spec
            .as_ref()
            .expect("job spec")
            .template
            .spec
            .as_ref()
            .expect("pod spec");
        let volumes = pod.volumes.as_ref().expect("plan volumes");
        assert_eq!(
            volumes[0].config_map.as_ref().expect("config").name,
            "mint-config"
        );
        assert_eq!(
            volumes[1]
                .persistent_volume_claim
                .as_ref()
                .expect("mint data")
                .claim_name,
            "mint-data"
        );
        assert_eq!(
            volumes[2]
                .persistent_volume_claim
                .as_ref()
                .expect("linked LND data")
                .claim_name,
            "data-mint-lnd-0"
        );
        let mounts = pod.containers[0]
            .volume_mounts
            .as_ref()
            .expect("plan mounts");
        assert_eq!(mounts[2].mount_path, "/lnd");
        assert_eq!(mounts[2].read_only, Some(true));

        let mut plans = crate::compile_component_plans(
            &lab.spec.instance_key,
            &lab.spec.revision_digest,
            &lab.spec.lab,
            &lab.spec.lock,
        )
        .expect("compiled plans");
        let mint = plans
            .iter_mut()
            .find(|plan| plan.component_id == "mint")
            .expect("mint plan");
        mint.credentials[0].claim_name = "opaque-linked-state".into();
        let context = native_exec_component_context(mint).expect("compiled native context");
        assert_eq!(
            context.volumes[2]["persistentVolumeClaim"]["claimName"],
            "opaque-linked-state"
        );
    }

    #[test]
    fn typed_peer_and_channel_actions_are_bounded_and_adapter_locked() {
        let (lab, mut action) = typed_bootstrap();
        let locked_lnd = lab
            .spec
            .lock
            .entries
            .iter()
            .find(|entry| entry.component_id == "mint-lnd")
            .expect("lnd lock")
            .image
            .clone();

        action.spec.capability = Capability::PeerConnect;
        action.spec.action = LabAction::PeerConnect(PeerConnectAction {
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
        });
        let peer = render_lab_action_job(&action, &lab).expect("peer job");
        assert_eq!(
            peer.spec
                .as_ref()
                .and_then(|spec| spec.active_deadline_seconds),
            Some(90)
        );
        assert_eq!(
            peer.spec
                .expect("peer spec")
                .template
                .spec
                .expect("pod")
                .containers[0]
                .image
                .as_deref(),
            Some(locked_lnd.as_str())
        );

        action.spec.capability = Capability::ChannelOpen;
        action.spec.action = LabAction::ChannelOpen(ChannelOpenAction {
            chain: "chain".into(),
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
            channel_sat: 2_000_000,
            push_sat: 0,
        });
        let channel = render_lab_action_job(&action, &lab).expect("channel job");
        assert_eq!(
            channel
                .spec
                .as_ref()
                .and_then(|spec| spec.active_deadline_seconds),
            Some(180)
        );
        let channel_open = channel
            .spec
            .as_ref()
            .and_then(|spec| spec.template.spec.as_ref())
            .and_then(|pod| pod.init_containers.as_ref())
            .and_then(|containers| containers.get(1))
            .and_then(|container| container.command.as_ref())
            .and_then(|command| command.get(2))
            .expect("channel open script");
        assert!(channel_open.contains("for attempt in $(seq 1 60)"));
        assert!(channel_open.contains("connect \"$pk@payer-lnd:9735\""));
        assert!(channel_open.contains("channel endpoint peer connection did not become ready"));
        assert!(channel_open.contains("/shared/channel-open.log"));

        action.spec.action = LabAction::ChannelPolicySet(ChannelPolicySetAction {
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
            base_fee_msat: 100_000,
            fee_rate_ppm: 250,
        });
        let policy = render_lab_action_job(&action, &lab).expect("channel policy job");
        assert_eq!(
            policy
                .spec
                .as_ref()
                .and_then(|spec| spec.active_deadline_seconds),
            Some(90)
        );
        let policy_script = policy
            .spec
            .as_ref()
            .and_then(|spec| spec.template.spec.as_ref())
            .and_then(|pod| pod.containers.first())
            .and_then(|container| container.command.as_ref())
            .and_then(|command| command.get(2))
            .expect("policy script");
        assert!(policy_script.contains("updatechanpolicy"));
        assert!(policy_script.contains("--base_fee_msat=100000"));
        assert!(policy_script.contains("--fee_rate_ppm=250"));
        assert!(policy_script.contains("--chan_point=\"$point\""));

        action.spec.action = LabAction::ChannelOpen(ChannelOpenAction {
            chain: "chain".into(),
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
            channel_sat: 2_000_000,
            push_sat: 2_000_000,
        });
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));
    }

    #[test]
    fn typed_peer_and_channel_teardown_uses_opaque_handles() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::PeerDisconnect;
        action.spec.action = LabAction::PeerDisconnect(PeerDisconnectAction {
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
        });
        let disconnect = render_lab_action_job(&action, &lab).expect("peer disconnect job");
        let disconnect_pod = disconnect
            .spec
            .expect("disconnect spec")
            .template
            .spec
            .expect("disconnect pod");
        assert!(disconnect_pod.init_containers.is_none());
        let disconnect_script = disconnect_pod.containers[0]
            .command
            .as_ref()
            .expect("disconnect command")[2]
            .as_str();
        assert!(disconnect_script.contains("$from disconnect \"$to_pk\""));
        assert!(disconnect_script.contains("$to disconnect \"$from_pk\""));
        assert!(!disconnect_script.contains("disconnectpeer"));

        let channel_id = format!("ch-{}", "a".repeat(64));
        action.spec.capability = Capability::ChannelClose;
        action.spec.action = LabAction::ChannelClose(ChannelCloseAction {
            chain: "chain".into(),
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
            channel_id: channel_id.clone(),
        });
        let close = render_lab_action_job(&action, &lab).expect("channel close job");
        let close_script = close
            .spec
            .expect("close spec")
            .template
            .spec
            .expect("close pod")
            .init_containers
            .expect("close init containers")[1]
            .command
            .as_ref()
            .expect("close command")[2]
            .clone();
        assert!(close_script.contains("closechannel"));
        assert!(!close_script.contains("closechannel --force"));

        action.spec.capability = Capability::ChannelForceClose;
        action.spec.action = LabAction::ChannelForceClose(ChannelCloseAction {
            chain: "chain".into(),
            from_lightning: "mint-lnd".into(),
            to_lightning: "payer-lnd".into(),
            channel_id,
        });
        let force_close = render_lab_action_job(&action, &lab).expect("force close job");
        let force_script = force_close
            .spec
            .expect("force close spec")
            .template
            .spec
            .expect("force close pod")
            .init_containers
            .expect("force init containers")[1]
            .command
            .as_ref()
            .expect("force close command")[2]
            .clone();
        assert!(force_script.contains("closechannel --force"));

        let LabAction::ChannelForceClose(request) = &mut action.spec.action else {
            panic!("force close action");
        };
        request.channel_id = "raw-channel-point".into();
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one adapter-parity test keeps peer, open, policy, and close scripts comparable"
    )]
    fn cln_and_lnd_peer_channel_jobs_use_endpoint_specific_adapters() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::PeerConnect;
        action.spec.action = LabAction::PeerConnect(PeerConnectAction {
            from_lightning: "attacker-cln".into(),
            to_lightning: "mint-lnd".into(),
        });
        let peer = render_lab_action_job(&action, &lab).expect("CLN to LND peer job");
        let peer_pod = peer
            .spec
            .expect("peer spec")
            .template
            .spec
            .expect("peer pod");
        let identity = &peer_pod.init_containers.expect("identity init")[0];
        assert!(
            identity
                .image
                .as_deref()
                .is_some_and(|image| image.contains("polarlightning/lnd"))
        );
        let connect = peer_pod.containers[0]
            .command
            .as_ref()
            .expect("connect command")[2]
            .as_str();
        assert!(connect.contains("lightning-cli --lightning-dir=/from"));
        assert!(connect.contains("connect \"$pk\" \"mint-lnd\" 9735"));

        action.spec.capability = Capability::ChannelOpen;
        action.spec.action = LabAction::ChannelOpen(ChannelOpenAction {
            chain: "chain".into(),
            from_lightning: "attacker-cln".into(),
            to_lightning: "mint-lnd".into(),
            channel_sat: 1_000_000,
            push_sat: 0,
        });
        let channel = render_lab_action_job(&action, &lab).expect("CLN channel job");
        let channel_pod = channel
            .spec
            .expect("channel spec")
            .template
            .spec
            .expect("channel pod");
        let init = channel_pod.init_containers.expect("channel init");
        let open = init[1].command.as_ref().expect("open command")[2].as_str();
        assert!(open.contains("fundchannel -k"));
        assert!(open.contains("jq -r '.txid'"));
        assert!(open.contains("connect \"$pk\" \"mint-lnd\" 9735"));
        assert!(open.contains("for attempt in $(seq 1 60)"));
        assert_ne!(init[0].image, init[1].image);

        action.spec.action = LabAction::ChannelPolicySet(ChannelPolicySetAction {
            from_lightning: "attacker-cln".into(),
            to_lightning: "mint-lnd".into(),
            base_fee_msat: 25_000,
            fee_rate_ppm: 500,
        });
        let policy = render_lab_action_job(&action, &lab).expect("CLN policy job");
        let policy_pod = policy
            .spec
            .expect("policy spec")
            .template
            .spec
            .expect("policy pod");
        let policy_script = policy_pod.containers[0]
            .command
            .as_ref()
            .expect("policy command")[2]
            .as_str();
        assert!(policy_script.contains("setchannel \"id=$pk\""));
        assert!(policy_script.contains("feebase=25000"));
        assert!(policy_script.contains("feeppm=500"));

        action.spec.capability = Capability::ChannelClose;
        action.spec.action = LabAction::ChannelClose(ChannelCloseAction {
            chain: "chain".into(),
            from_lightning: "attacker-cln".into(),
            to_lightning: "mint-lnd".into(),
            channel_id: format!("ch-{}", "a".repeat(64)),
        });
        let close = render_lab_action_job(&action, &lab).expect("CLN close job");
        let close_pod = close
            .spec
            .expect("close spec")
            .template
            .spec
            .expect("close pod");
        assert_eq!(
            close_pod
                .init_containers
                .as_ref()
                .expect("identity init")
                .len(),
            1
        );
        assert_eq!(close_pod.containers.len(), 3);
        let close_script = close_pod.containers[0]
            .command
            .as_ref()
            .expect("close command")[2]
            .as_str();
        let confirm_script = close_pod.containers[1]
            .command
            .as_ref()
            .expect("confirm command")[2]
            .as_str();
        let result_script = close_pod.containers[2]
            .command
            .as_ref()
            .expect("result command")[2]
            .as_str();
        assert!(close_script.contains("touch /shared/close-started"));
        assert!(close_script.contains("touch /shared/close-done"));
        assert!(confirm_script.contains("until test -f /shared/close-done"));
        assert!(result_script.contains("until test -f /shared/mined"));
        assert!(result_script.contains("test \"$rounds\" -lt 30"));
        assert!(result_script.contains("FUNDING_SPEND_SEEN|ONCHAIN"));
    }

    #[test]
    fn typed_rebalance_uses_opaque_handles_and_rejects_unsupported_adapters() {
        let (lab, mut action) = typed_bootstrap();
        let outgoing = format!("ch-{}", "a".repeat(64));
        let incoming = format!("ch-{}", "b".repeat(64));
        action.spec.capability = Capability::ChannelRebalance;
        action.spec.action = LabAction::ChannelRebalance(ChannelRebalanceAction {
            lightning: "mint-lnd".into(),
            outgoing_channel_id: outgoing.clone(),
            incoming_channel_id: incoming.clone(),
            amount_sat: 100_000,
            max_fee_sat: 100,
        });
        let rebalance = render_lab_action_job(&action, &lab).expect("rebalance job");
        let spec = rebalance.spec.expect("job spec");
        assert_eq!(spec.active_deadline_seconds, Some(120));
        let pod = spec.template.spec.expect("rebalance pod");
        assert!(pod.init_containers.is_none());
        assert_eq!(pod.containers.len(), 1);
        let script = pod.containers[0]
            .command
            .as_ref()
            .expect("rebalance command")[2]
            .as_str();
        assert!(script.contains("--allow_self_payment"));
        assert!(script.contains("--max_parts=1"));
        assert!(script.contains("--last_hop=\"$in_peer\""));
        assert!(script.contains("attempts=$((attempts + 1))"));
        assert!(script.contains("test \"$attempts\" -lt 45"));
        assert!(script.contains("rounds=$((rounds + 1))"));
        assert!(script.contains("test \"$rounds\" -lt 30"));
        assert!(script.contains("--timeout=10s"));
        assert!(script.contains("/\"scid\":/"));
        assert!(!script.contains("/\"chan_id\":/"));
        assert!(script.contains("cut -d'\"' -f4"));
        assert!(!script.contains("cut -d'\\\"'"));
        assert!(script.contains("'\"status\":[[:space:]]*\"SUCCEEDED\"'"));
        assert!(script.contains(&outgoing));
        assert!(script.contains(&incoming));
        assert!(script.contains("outgoing_local_before_sat"));
        assert!(!script.contains("payment_request\":\""));

        if let LabAction::ChannelRebalance(request) = &mut action.spec.action {
            request.incoming_channel_id.clone_from(&outgoing);
        } else {
            panic!("rebalance action");
        }
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));
        if let LabAction::ChannelRebalance(request) = &mut action.spec.action {
            request.incoming_channel_id = incoming;
            request.lightning = "attacker-cln".into();
        }
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::UnsupportedAdapter { .. })
        ));
    }

    #[test]
    fn wallet_initialize_and_balance_use_the_locked_adapter_and_snapshot_reads() {
        let (lab, mut action) = typed_bootstrap();
        let wallet_image = lab
            .spec
            .lock
            .entries
            .iter()
            .find(|entry| entry.component_id == "wallet")
            .expect("wallet lock")
            .image
            .clone();
        action.spec.capability = Capability::WalletCreate;
        action.spec.action = LabAction::WalletInitialize(WalletInitializeAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
        });
        let initialize = render_lab_action_job(&action, &lab).expect("initialize job");
        assert_eq!(
            initialize
                .spec
                .expect("spec")
                .template
                .spec
                .expect("pod")
                .containers[0]
                .image
                .as_deref(),
            Some(wallet_image.as_str())
        );

        action.spec.capability = Capability::WalletControl;
        action.spec.action = LabAction::WalletBalance(WalletBalanceAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
        });
        let balance = render_lab_action_job(&action, &lab).expect("balance job");
        let snapshot = &balance
            .spec
            .expect("spec")
            .template
            .spec
            .expect("pod")
            .init_containers
            .expect("snapshot")[0];
        assert_eq!(snapshot.name, "snapshot");
        assert_eq!(
            snapshot.volume_mounts.as_ref().expect("mounts")[0].read_only,
            Some(true)
        );
    }

    #[test]
    fn wallet_fund_is_bounded_and_uses_locked_wallet_and_payer_adapters() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::WalletFund;
        action.spec.action = LabAction::WalletFund(WalletFundAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
            payer_lightning: "payer-lnd".into(),
            amount_sat: 1_000,
        });
        let funded = render_lab_action_job(&action, &lab).expect("fund job");
        let funded_spec = funded.spec.expect("spec");
        assert_eq!(funded_spec.active_deadline_seconds, Some(180));
        let pod = funded_spec.template.spec.expect("pod");
        assert_eq!(pod.containers[0].name, "wallet");
        assert_eq!(pod.containers[1].name, "payer");
        let wallet_script = pod.containers[0]
            .command
            .as_ref()
            .expect("wallet command")
            .last()
            .expect("wallet script");
        assert!(wallet_script.contains("invoice 1000 --no-check"));
        assert!(wallet_script.contains("--id \"$quote_id\""));
        assert!(wallet_script.contains("quote_id_not_observed"));
        assert!(wallet_script.contains("quote_not_paid"));
        assert!(wallet_script.contains("/shared/payer.failed"));
        assert!(wallet_script.contains("payment_wait_timeout"));
        assert!(wallet_script.contains("invoice_settlement_timeout"));
        assert!(wallet_script.contains("wallet_orchestration_failed"));
        #[cfg(unix)]
        assert_shell_syntax(wallet_script);
        let payer_script = pod.containers[1]
            .command
            .as_ref()
            .expect("payer command")
            .last()
            .expect("payer script");
        assert!(payer_script.contains("/shared/wallet.failed"));
        assert!(payer_script.contains("invoice_not_observed"));
        assert!(payer_script.contains("payment_timeout"));
        assert!(payer_script.contains("wallet_completion_timeout"));
        assert!(payer_script.contains("ln(bcrt|bc|tb|tbs)"));
        #[cfg(unix)]
        assert_shell_syntax(payer_script);
        let LabAction::WalletFund(request) = &mut action.spec.action else {
            panic!("fund action");
        };
        request.amount_sat = 500_001;
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));
    }

    #[test]
    fn wallet_invoice_and_pay_keep_payment_material_in_private_volumes() {
        let (lab, mut invoice_action) = typed_bootstrap();
        invoice_action.spec.capability = Capability::WalletFund;
        invoice_action.spec.action = LabAction::WalletInvoice(WalletInvoiceAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
            amount_sat: 100,
            timeout_seconds: 300,
        });
        let invoice = render_lab_action_job(&invoice_action, &lab).expect("invoice job");
        let invoice_spec = invoice.spec.expect("invoice spec");
        assert_eq!(invoice_spec.active_deadline_seconds, Some(330));
        let invoice_pod = invoice_spec.template.spec.expect("invoice pod");
        let invoice_script = invoice_pod.containers[0]
            .command
            .as_ref()
            .expect("invoice command")
            .last()
            .expect("invoice script");
        assert!(invoice_script.contains("mktemp /tmp/proofstorm-invoice"));
        assert!(invoice_script.contains("cashu invoice 100 --no-check"));
        assert!(invoice_script.contains("trap cleanup EXIT"));
        assert!(!invoice_script.contains("lnbcrt1"));
        assert!(
            render_lab_action_cleanup_job(&invoice_action, &lab)
                .expect("cleanup render")
                .is_none()
        );

        let mut pay_lab = lab;
        let mut receiver = pay_lab
            .spec
            .lab
            .components
            .iter()
            .find(|component| component.id == "wallet")
            .expect("wallet")
            .clone();
        receiver.id = "receiver-wallet".into();
        pay_lab.spec.lab.components.push(receiver);
        let mut receiver_lock = pay_lab
            .spec
            .lock
            .entries
            .iter()
            .find(|entry| entry.component_id == "wallet")
            .expect("wallet lock")
            .clone();
        receiver_lock.component_id = "receiver-wallet".into();
        pay_lab.spec.lock.entries.push(receiver_lock);
        let mut pay_action = invoice_action;
        pay_action.spec.capability = Capability::WalletControl;
        pay_action.spec.action = LabAction::WalletPay(WalletPayAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
            recipient_wallet: "receiver-wallet".into(),
            recipient_mint: "mint".into(),
            mint_quote_id: "quote-one".into(),
        });
        let pay = render_lab_action_job(&pay_action, &pay_lab).expect("pay job");
        let pod = pay.spec.expect("pay spec").template.spec.expect("pay pod");
        let recipient_mount = pod.containers[0]
            .volume_mounts
            .as_ref()
            .expect("mounts")
            .iter()
            .find(|mount| mount.name == "recipient")
            .expect("recipient mount");
        assert_eq!(recipient_mount.read_only, Some(false));

        let LabAction::WalletPay(request) = &mut pay_action.spec.action else {
            panic!("pay action");
        };
        request.recipient_wallet = "wallet".into();
        assert!(matches!(
            render_lab_action_job(&pay_action, &pay_lab),
            Err(ActionRenderError::Bounds(_))
        ));
    }

    #[test]
    fn oracle_snapshots_the_wallet_and_round_trip_has_a_fixed_deadline() {
        let oracle = render_conservation_oracle_job(&ConservationOracleJobSpec {
            resource_name: "op-oracle",
            instance_key: "i0123456789012345678",
            wallet: "wallet",
            mint: "mint",
            wallet_image: "wallet-image",
            baseline_operation_id: "balance-before",
            treatment_operation_id: "treatment",
            expected_sat: 100,
            tolerance_sat: 2,
        })
        .expect("oracle");
        let oracle_pod = oracle.spec.expect("spec").template.spec.expect("pod");
        assert_eq!(
            oracle_pod.init_containers.expect("snapshot")[0].name,
            "snapshot"
        );
        assert_eq!(oracle_pod.containers[0].name, "oracle");
        let oracle_command = oracle_pod.containers[0]
            .command
            .as_ref()
            .expect("oracle command");
        assert!(
            oracle_command
                .iter()
                .any(|part| part.contains("\"conserved\":%s"))
        );
        assert!(
            oracle_command
                .iter()
                .all(|part| !part.contains("test \"$conserved\" = true")),
            "a negative conservation finding is evidence, not a failed Job"
        );

        let round_trip = render_wallet_round_trip_job(&WalletRoundTripJobSpec {
            resource_name: "op-wallet",
            instance_key: "i0123456789012345678",
            wallet: "wallet",
            mint: "mint",
            payer_lightning: "payer-lnd",
            wallet_image: "wallet",
            lnd_image: "lnd",
            amount_sat: 100,
            tolerance_sat: 2,
        })
        .expect("round trip");
        assert_eq!(
            round_trip
                .spec
                .as_ref()
                .and_then(|spec| spec.active_deadline_seconds),
            Some(240)
        );
        let round_trip_pod = round_trip
            .spec
            .expect("round trip spec")
            .template
            .spec
            .expect("round trip pod");
        let wallet_script = round_trip_pod.containers[0]
            .command
            .as_ref()
            .expect("wallet command")
            .last()
            .expect("wallet script");
        assert!(wallet_script.contains("/shared/payer.failed"));
        assert!(wallet_script.contains("selfpay_failed"));
        let payer_script = round_trip_pod.containers[1]
            .command
            .as_ref()
            .expect("payer command")
            .last()
            .expect("payer script");
        assert!(payer_script.contains("/shared/wallet.failed"));
        assert!(payer_script.contains("payment_timeout"));
    }

    #[test]
    fn typed_bootstrap_is_identity_checked_and_controller_owned() {
        let (lab, mut action) = typed_bootstrap();
        let job = render_lab_action_job(&action, &lab).expect("typed job");
        assert_eq!(job.metadata.name.as_deref(), Some("action-123"));
        assert_eq!(
            job.metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
                .map(String::as_str),
            Some("proofstormd")
        );
        action.spec.instance_id = "another-instance".into();
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Identity("instance_id"))
        ));
    }

    #[test]
    fn typed_bootstrap_refuses_out_of_bounds_and_unknown_fields() {
        let (lab, mut action) = typed_bootstrap();
        let LabAction::BootstrapLiquidity(request) = &mut action.spec.action else {
            panic!("expected bootstrap action");
        };
        request.push_sat = request.channel_sat;
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));

        let mut document = serde_json::to_value(&action.spec).expect("serialize action");
        document["action"]["parameters"]["command"] = json!("arbitrary shell");
        assert!(serde_json::from_value::<crate::ProofstormLabActionSpec>(document).is_err());
    }

    #[test]
    fn node_lifecycle_is_typed_and_never_renders_a_privileged_job() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::NodeControl;
        action.spec.action = LabAction::NodeRestart(crate::NodeControlAction {
            component: "chain".into(),
        });
        let serialized = serde_json::to_value(&action.spec.action).expect("serialize action");
        assert_eq!(serialized["kind"], "node_restart");
        assert_eq!(serialized["parameters"]["component"], "chain");
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));
        assert_eq!(action_result_container(&action.spec.action), "result");
    }

    #[test]
    fn typed_wallet_round_trip_uses_the_locked_wallet_adapter_image() {
        let (mut lab, mut action) = typed_bootstrap();
        let locked_image = "registry.example/nutshell@sha256:locked-wallet-image";
        lab.spec
            .lock
            .entries
            .iter_mut()
            .find(|entry| entry.component_id == "wallet")
            .expect("wallet lock entry")
            .image = locked_image.into();
        action.spec.capability = Capability::WalletControl;
        action.spec.action = LabAction::WalletRoundTrip(WalletRoundTripAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
            payer_lightning: "payer-lnd".into(),
            amount_sat: 1_000,
            tolerance_sat: 100,
        });

        let job = render_lab_action_job(&action, &lab).expect("typed wallet job");
        let pod = job.spec.expect("job spec").template.spec.expect("pod spec");
        let wallet = pod
            .containers
            .iter()
            .find(|container| container.name == "wallet")
            .expect("wallet result container");
        assert_eq!(wallet.image.as_deref(), Some(locked_image));
        assert_eq!(action_result_container(&action.spec.action), "wallet");
    }

    #[test]
    fn typed_conservation_oracle_snapshots_with_the_locked_wallet_image() {
        let (mut lab, mut action) = typed_bootstrap();
        let locked_image = "registry.example/nutshell@sha256:locked-oracle-image";
        lab.spec
            .lock
            .entries
            .iter_mut()
            .find(|entry| entry.component_id == "wallet")
            .expect("wallet lock entry")
            .image = locked_image.into();
        action.spec.capability = Capability::OracleRun;
        action.spec.action = LabAction::ConservationOracle(ConservationOracleAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
            baseline_operation_id: "balance-before".into(),
            treatment_operation_id: "payment-under-test".into(),
            expected_sat: 997,
            tolerance_sat: 0,
        });

        let job = render_lab_action_job(&action, &lab).expect("typed oracle job");
        let pod = job.spec.expect("job spec").template.spec.expect("pod spec");
        let snapshot = pod.init_containers.expect("snapshot container");
        assert_eq!(snapshot[0].image.as_deref(), Some(locked_image));
        assert_eq!(pod.containers[0].image.as_deref(), Some(locked_image));
        assert_eq!(action_result_container(&action.spec.action), "oracle");
        let command = pod.containers[0].command.as_ref().expect("oracle command");
        assert!(command.iter().any(|part| part.contains("balance-before")));
        assert!(
            command
                .iter()
                .any(|part| part.contains("payment-under-test"))
        );

        action.spec.capability = Capability::WalletControl;
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Capability)
        ));

        action.spec.capability = Capability::OracleRun;
        lab.spec
            .lab
            .components
            .iter_mut()
            .find(|component| component.id == "wallet")
            .expect("wallet component")
            .implementation = "cocod-wallet".into();
        lab.spec
            .lock
            .entries
            .iter_mut()
            .find(|entry| entry.component_id == "wallet")
            .expect("wallet lock entry")
            .catalog_id = "cocod-wallet".into();
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::UnsupportedAdapter { adapter, .. })
                if adapter == "cocod-wallet"
        ));
    }

    #[test]
    fn reachability_oracle_uses_source_firewall_identity_and_advertised_service() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::OracleRun;
        action.spec.action = LabAction::ReachabilityOracle(ReachabilityOracleAction {
            from_component: "wallet".into(),
            to_component: "mint".into(),
            service: "http".into(),
            timeout_seconds: 2,
            attempts: 3,
        });

        let job = render_lab_action_job(&action, &lab).expect("reachability job");
        assert!(
            job.metadata
                .labels
                .as_ref()
                .is_some_and(|labels| labels.contains_key("proofstorm.dev/operation"))
        );
        let template = &job.spec.as_ref().expect("job spec").template;
        let labels = template
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.labels.as_ref())
            .expect("pod labels");
        assert_eq!(
            labels
                .get("proofstorm.dev/network-identity")
                .map(String::as_str),
            Some("wallet")
        );
        assert!(!labels.contains_key("proofstorm.dev/component"));
        assert!(!labels.contains_key("proofstorm.dev/operation"));
        let pod = template.spec.as_ref().expect("pod spec");
        assert!(!pod.automount_service_account_token.unwrap_or(true));
        let probe = &pod.containers[0];
        assert_eq!(probe.image.as_deref(), Some(REACHABILITY_PROBE_IMAGE));
        let script = &probe.command.as_ref().expect("shell command")[2];
        assert!(script.contains("nc -z -w 2 mint 3338"));
        assert!(script.contains("\"reachable\":%s"));
        assert_eq!(action_result_container(&action.spec.action), "oracle");
    }

    #[test]
    fn reachability_oracle_refuses_unknown_services_and_unbounded_probes() {
        let (lab, mut action) = typed_bootstrap();
        action.spec.capability = Capability::OracleRun;
        action.spec.action = LabAction::ReachabilityOracle(ReachabilityOracleAction {
            from_component: "wallet".into(),
            to_component: "mint".into(),
            service: "ssh".into(),
            timeout_seconds: 2,
            attempts: 3,
        });
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::UnknownService { .. })
        ));

        let LabAction::ReachabilityOracle(request) = &mut action.spec.action else {
            unreachable!()
        };
        request.service = "http".into();
        request.attempts = 6;
        assert!(matches!(
            render_lab_action_job(&action, &lab),
            Err(ActionRenderError::Bounds(_))
        ));
    }
}
