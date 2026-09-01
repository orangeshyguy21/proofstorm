use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The economic direction of a wallet quote from the wallet's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WalletQuoteDirection {
    Receive,
    Pay,
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
}
