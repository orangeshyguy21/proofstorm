mod adapter;
mod api;
mod operation;
mod render;

pub const ACTION_CANCEL_ANNOTATION: &str = "proofstorm.dev/cancel-token";
pub const LIFECYCLE_STATE_ANNOTATION: &str = "proofstorm.dev/lifecycle-state";
pub const LIFECYCLE_SEQUENCE_ANNOTATION: &str = "proofstorm.dev/lifecycle-sequence";
pub const LIFECYCLE_RESTART_ANNOTATION: &str = "proofstorm.dev/restart-token";

pub use adapter::{
    AdapterError, RenderedLab, component_ports, render_component_network_policy, render_lab,
};
pub use api::{
    ActionPhase, BootstrapLiquidityAction, ChannelCloseAction, ChannelOpenAction,
    ChannelRebalanceAction, ConservationOracleAction, LabAction, LabPhase, NetworkHealAction,
    NetworkPartitionAction, NodeControlAction, PeerConnectAction, PeerDisconnectAction,
    ProofstormLab, ProofstormLabAction, ProofstormLabActionSpec, ProofstormLabActionStatus,
    ProofstormLabSpec, ProofstormLabStatus, ReachabilityOracleAction, TeardownReceipt,
    WalletBalanceAction, WalletFundAction, WalletInitializeAction, WalletInvoiceAction,
    WalletPayAction, WalletRoundTripAction,
};
pub use operation::{
    ActionRenderError, BootstrapJobSpec, ChannelCloseJobSpec, ChannelOpenJobSpec,
    ChannelRebalanceJobSpec, LightningAdapter, PeerConnectJobSpec, PeerDisconnectJobSpec,
    WalletFundJobSpec, WalletInvoiceJobSpec, WalletJobSpec, WalletPayJobSpec,
    WalletRoundTripJobSpec, action_result_container, render_bootstrap_job,
    render_channel_close_job, render_channel_open_job, render_channel_rebalance_job,
    render_conservation_oracle_job, render_lab_action_cleanup_job, render_lab_action_job,
    render_peer_connect_job, render_peer_disconnect_job, render_wallet_balance_job,
    render_wallet_fund_job, render_wallet_initialize_job, render_wallet_invoice_job,
    render_wallet_pay_job, render_wallet_round_trip_job,
};
pub use render::{
    INSTANCE_LABEL, RenderedSecuritySpine, instance_namespace, render_security_spine,
};
