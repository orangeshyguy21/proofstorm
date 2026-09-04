mod adapter;
mod api;
mod operation;
mod render;
mod scheduler;

pub const ACTION_CANCEL_ANNOTATION: &str = "proofstorm.dev/cancel-token";
pub const BACKEND_ID_ANNOTATION: &str = "proofstorm.dev/backend-id";
pub const EXECUTION_STATE_CONTRACT_ANNOTATION: &str = "proofstorm.dev/execution-state-contract";
pub const LIFECYCLE_STATE_ANNOTATION: &str = "proofstorm.dev/lifecycle-state";
pub const LIFECYCLE_SEQUENCE_ANNOTATION: &str = "proofstorm.dev/lifecycle-sequence";
pub const LIFECYCLE_RESTART_ANNOTATION: &str = "proofstorm.dev/restart-token";
pub const REVISION_DIGEST_ANNOTATION: &str = "proofstorm.dev/revision-digest";
pub const ROLLOUT_DIGEST_ANNOTATION: &str = "proofstorm.dev/rollout-digest";

pub use adapter::{
    AdapterError, COMPONENT_LABEL, ComponentObservationResources,
    PROTOCOL_PROBER_DIGEST_ANNOTATION, PROTOCOL_PROBER_LABEL, PROTOCOL_PROBER_LEASE_ANNOTATION,
    PROTOCOL_PROBER_NAME, RenderedComponent, RenderedLab, compile_component_plans, component_ports,
    observe_component_statuses, render_attacker_component, render_bitcoin_component,
    render_cdk_component, render_cln_component, render_component_network_policy,
    render_keycloak_component, render_lab, render_lnd_component, render_nutshell_mint_component,
    render_postgres_component, render_protocol_prober, render_redis_component,
    render_wallet_component,
};
pub use api::{
    ActionPhase, AuthenticationConformanceAction, AuthenticationConformanceFailureStage,
    AuthenticationConformanceResult, AuthenticationProtectedSpendAction,
    AuthenticationProtectedSpendResult, AuthenticationReplayAction, AuthenticationReplayResult,
    AuthenticationSessionFailureStage, BootstrapLiquidityAction, ChannelCloseAction,
    ChannelOpenAction, ChannelPolicySetAction, ChannelRebalanceAction, ComponentExecLiveAction,
    ComponentForensicsAction, ComponentLogsAction, ConservationOracleAction, LabAction, LabPhase,
    NetworkHealAction, NetworkPartitionAction, NodeControlAction, PeerConnectAction,
    PeerDisconnectAction, ProofstormLab, ProofstormLabAction, ProofstormLabActionSpec,
    ProofstormLabActionStatus, ProofstormLabSpec, ProofstormLabStatus, ReachabilityOracleAction,
    TeardownReceipt, WalletBalanceAction, WalletFundAction, WalletInitializeAction,
    WalletInvoiceAction, WalletMeltQuoteRefreshAction, WalletPayAction, WalletQuoteClaimAction,
    WalletRoundTripAction,
};
pub use operation::{
    ActionAdmissionError, ActionRenderError, AuthenticationConformanceJobSpec,
    AuthenticationProtectedSpendJobSpec, AuthenticationReplayJobSpec, BootstrapJobSpec,
    ChannelCloseJobSpec, ChannelOpenJobSpec, ChannelPolicySetJobSpec, ChannelRebalanceJobSpec,
    ConservationOracleJobSpec, LightningAdapter, PeerConnectJobSpec, PeerDisconnectJobSpec,
    WalletFundJobSpec, WalletInvoiceJobSpec, WalletJobSpec, WalletMeltQuoteRefreshJobSpec,
    WalletPayJobSpec, WalletRoundTripJobSpec, action_result_container, evaluate_action_admission,
    render_authentication_conformance_job, render_authentication_protected_spend_job,
    render_authentication_replay_job, render_bootstrap_job, render_channel_close_job,
    render_channel_open_job, render_channel_policy_set_job, render_channel_rebalance_job,
    render_conservation_oracle_job, render_lab_action_cleanup_job, render_lab_action_job,
    render_peer_connect_job, render_peer_disconnect_job, render_wallet_balance_job,
    render_wallet_fund_job, render_wallet_initialize_job, render_wallet_invoice_job,
    render_wallet_melt_quote_refresh_job, render_wallet_pay_job, render_wallet_round_trip_job,
};
pub use render::{
    INSTANCE_LABEL, RenderedSecuritySpine, instance_namespace, render_security_spine,
};
pub use scheduler::{
    MAX_ACTIVE_PROTOCOL_PROBER_LABS, MAX_GLOBAL_PROTOCOL_PROBES, MAX_PROTOCOL_PROBES_PER_LAB,
    PROTOCOL_PROBE_LEASE_SECONDS, ProtocolProbeSchedule, schedule_protocol_probers,
};
