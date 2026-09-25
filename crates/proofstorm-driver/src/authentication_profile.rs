//! Shared NUT-21/22 wire-error expectations for drivers and result validation.

/// Protocol errors observed from each supported mint implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticationProfile {
    /// A parseable BAT with an invalid signature.
    pub invalid_bat: u32,
    /// An invalid clear-auth token.
    pub invalid_cat: u32,
    /// An issuance request above the configured BAT maximum.
    pub bat_maximum: u32,
    /// Reuse of a previously spent BAT.
    pub spent_bat: u32,
    /// CAT issuance rate limiting, when the implementation supports it.
    pub cat_rate_limit: Option<u32>,
}

impl AuthenticationProfile {
    /// Select the shared protocol contract; unknown implementations fail closed.
    #[must_use]
    pub fn for_implementation(implementation: &str) -> Option<Self> {
        match implementation {
            "nutshell" => Some(Self {
                invalid_bat: 31_002,
                invalid_cat: 30_002,
                bat_maximum: 31_003,
                spent_bat: 31_002,
                cat_rate_limit: Some(31_004),
            }),
            // CDK uses generic proof/amount errors for blind-auth failures.
            "cdk" => Some(Self {
                invalid_bat: 10_001,
                invalid_cat: 30_002,
                bat_maximum: 11_006,
                spent_bat: 11_001,
                cat_rate_limit: None,
            }),
            _ => None,
        }
    }
}
