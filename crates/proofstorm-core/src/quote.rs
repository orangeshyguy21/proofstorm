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

/// Adapter-native quote facts decoded from one sanitized operation artifact.
/// The durable store adds authority and operation attribution when recording
/// the observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuoteObservationInput {
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
    pub lease_id: String,
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

/// Decode the optional quote-observation array carried by a sanitized terminal
/// action artifact. Non-quote artifacts decode to an empty observation set.
///
/// # Errors
///
/// Returns an error when `quote_observations` is present but does not match the
/// strict adapter-neutral observation input contract.
pub fn wallet_quote_observations_from_artifact(
    artifact: &serde_json::Value,
) -> Result<Vec<WalletQuoteObservationInput>, serde_json::Error> {
    artifact.get("quote_observations").map_or_else(
        || Ok(Vec::new()),
        |observations| serde_json::from_value(observations.clone()),
    )
}

/// Adapter-neutral quote progress. Adapter quote IDs and payment requests are
/// deliberately absent from the public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WalletQuotePhase {
    Requested,
    Ready,
    Pending,
    Paid,
    Settled,
    Expired,
    Failed,
    Inconclusive,
    Cancelled,
}

impl WalletQuotePhase {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Settled | Self::Expired | Self::Failed | Self::Cancelled
        )
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Requested => matches!(
                next,
                Self::Ready
                    | Self::Settled
                    | Self::Expired
                    | Self::Failed
                    | Self::Inconclusive
                    | Self::Cancelled
            ),
            Self::Ready => matches!(
                next,
                Self::Pending
                    | Self::Paid
                    | Self::Settled
                    | Self::Expired
                    | Self::Failed
                    | Self::Inconclusive
                    | Self::Cancelled
            ),
            Self::Pending => matches!(
                next,
                Self::Paid | Self::Settled | Self::Expired | Self::Failed | Self::Inconclusive
            ),
            Self::Paid => matches!(
                next,
                Self::Settled | Self::Expired | Self::Failed | Self::Inconclusive
            ),
            Self::Inconclusive => matches!(next, Self::Paid | Self::Settled),
            Self::Settled | Self::Expired | Self::Failed | Self::Cancelled => false,
        }
    }
}

/// A durable Proofstorm-owned handle for an adapter-private Cashu quote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletQuote {
    pub id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub experiment_id: String,
    pub lease_id: String,
    pub principal_id: String,
    pub wallet_id: String,
    pub mint_id: String,
    pub direction: WalletQuoteDirection,
    pub amount_sat: u64,
    pub phase: WalletQuotePhase,
    pub created_at_unix: i64,
    pub updated_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_progress_is_monotonic_and_terminal() {
        assert!(WalletQuotePhase::Requested.can_transition_to(WalletQuotePhase::Ready));
        assert!(WalletQuotePhase::Ready.can_transition_to(WalletQuotePhase::Pending));
        assert!(WalletQuotePhase::Pending.can_transition_to(WalletQuotePhase::Paid));
        assert!(WalletQuotePhase::Pending.can_transition_to(WalletQuotePhase::Inconclusive));
        assert!(!WalletQuotePhase::Inconclusive.is_terminal());
        assert!(WalletQuotePhase::Inconclusive.can_transition_to(WalletQuotePhase::Paid));
        assert!(WalletQuotePhase::Inconclusive.can_transition_to(WalletQuotePhase::Settled));
        assert!(!WalletQuotePhase::Inconclusive.can_transition_to(WalletQuotePhase::Failed));
        assert!(WalletQuotePhase::Paid.can_transition_to(WalletQuotePhase::Settled));
        assert!(WalletQuotePhase::Settled.is_terminal());
        assert!(!WalletQuotePhase::Settled.can_transition_to(WalletQuotePhase::Pending));
        assert!(!WalletQuotePhase::Paid.can_transition_to(WalletQuotePhase::Ready));
    }

    #[test]
    fn sanitized_artifacts_decode_zero_one_or_two_native_observations() {
        let none = wallet_quote_observations_from_artifact(&serde_json::json!({
            "code": "action_failed"
        }))
        .expect("non-quote artifact");
        assert!(none.is_empty());

        let receive = serde_json::json!({
            "role": "invoice_receive",
            "wallet_id": "recipient-wallet",
            "mint_id": "recipient-mint",
            "direction": "receive",
            "quote_id": "01234567-89ab-cdef-0123-456789abcdef",
            "amount_sat": 100,
            "state": "UNPAID",
            "wallet_created_at_unix": 1,
            "wallet_expires_at_unix": 301
        });
        let decoded = wallet_quote_observations_from_artifact(&serde_json::json!({
            "quote_observations": [receive.clone()]
        }))
        .expect("receive observation");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].role, WalletQuoteObservationRole::InvoiceReceive);
        assert_eq!(decoded[0].state, "UNPAID");

        let melt = serde_json::json!({
            "role": "payment_melt",
            "wallet_id": "payer-wallet",
            "mint_id": "payer-mint",
            "direction": "pay",
            "quote_id": "fedcba98-7654-3210-fedc-ba9876543210",
            "amount_sat": 100,
            "state": "PAID",
            "fee_reserve_sat": 2,
            "fee_paid_sat": 1
        });
        for state in ["UNPAID", "PENDING"] {
            let mut observation = melt.clone();
            observation["state"] = serde_json::json!(state);
            let decoded = wallet_quote_observations_from_artifact(&serde_json::json!({
                "quote_observations": [observation]
            }))
            .expect("melt-only observation");
            assert_eq!(decoded[0].state, state);
        }

        let mut paid_but_unclaimed = receive.clone();
        paid_but_unclaimed["role"] = serde_json::json!("payment_receive");
        paid_but_unclaimed["state"] = serde_json::json!("PAID");
        let decoded = wallet_quote_observations_from_artifact(&serde_json::json!({
            "code": "payment_paid_claim_unverified",
            "quote_observations": [melt.clone(), paid_but_unclaimed]
        }))
        .expect("paid but unclaimed observations");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].role, WalletQuoteObservationRole::PaymentMelt);
        assert_eq!(decoded[0].state, "PAID");
        assert_eq!(decoded[1].role, WalletQuoteObservationRole::PaymentReceive);
        assert_eq!(decoded[1].state, "PAID");

        let mut issued = receive;
        issued["role"] = serde_json::json!("payment_receive");
        issued["state"] = serde_json::json!("ISSUED");
        let decoded = wallet_quote_observations_from_artifact(&serde_json::json!({
            "quote_observations": [melt, issued]
        }))
        .expect("paid and issued observations");
        assert_eq!(decoded[0].state, "PAID");
        assert_eq!(decoded[1].state, "ISSUED");
    }

    #[test]
    fn malformed_observation_envelopes_fail_closed() {
        let error = wallet_quote_observations_from_artifact(&serde_json::json!({
            "quote_observations": [{
                "role": "payment_melt",
                "wallet_id": "payer-wallet",
                "mint_id": "payer-mint",
                "direction": "pay",
                "quote_id": "fedcba98-7654-3210-fedc-ba9876543210",
                "amount_sat": 100
            }]
        }))
        .expect_err("state is required");
        assert!(error.to_string().contains("state"));
    }
}
