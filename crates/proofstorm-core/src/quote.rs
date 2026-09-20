use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The economic direction of a wallet quote from the wallet's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WalletQuoteDirection {
    Receive,
    Pay,
}

/// The role of one wallet-native quote observation in the operation that
/// produced it. Roles make retry deduplication explicit without inventing a
/// second quote lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WalletQuoteObservationRole {
    InvoiceReceive,
    PaymentMelt,
    PaymentReceive,
    ClaimReceive,
}

/// An immutable, attributed observation of one adapter-native wallet quote.
/// This is a historical observation, not live wallet state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteObservation {
    pub observation_sequence: u64,
    pub workspace_id: String,
    pub instance_id: String,
    pub experiment_id: String,
    pub session_id: String,
    pub principal_id: String,
    pub observed_by_operation: String,
    pub role: WalletQuoteObservationRole,
    pub wallet_id: String,
    pub mint_id: String,
    pub direction: WalletQuoteDirection,
    pub quote_id: String,
    pub amount_sat: u64,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_created_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_paid_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_expires_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_reserve_sat: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_paid_sat: Option<u64>,
    pub observed_at_unix: i64,
}
