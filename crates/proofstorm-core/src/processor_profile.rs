//! Audited CDK gRPC capabilities, selected by implementation rather than inferred
//! from an endpoint's response. A profile alone does not enable a catalog entry.
use std::{collections::BTreeSet, fmt, str::FromStr};

use crate::{DependencyBinding, PaymentMethod, processor_ids};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorProfile {
    LdkServer,
    Bark,
}

impl ProcessorProfile {
    #[must_use]
    pub fn for_implementation(implementation: &str) -> Option<Self> {
        match implementation {
            processor_ids::LDK_PROCESSOR => Some(Self::LdkServer),
            processor_ids::BARK_PROCESSOR => Some(Self::Bark),
            _ => None,
        }
    }

    #[must_use]
    pub const fn implementation(self) -> &'static str {
        match self {
            Self::LdkServer => processor_ids::LDK_PROCESSOR,
            Self::Bark => processor_ids::BARK_PROCESSOR,
        }
    }

    #[must_use]
    pub const fn methods(self) -> &'static [PaymentMethod] {
        match self {
            Self::LdkServer => &[PaymentMethod::Bolt11, PaymentMethod::Bolt12],
            Self::Bark => &[PaymentMethod::Bolt11],
        }
    }

    /// Authored payment bindings use the mint's unit, including CDK's conversion
    /// from the LDK processor's native msat responses.
    #[must_use]
    pub const fn binding_unit(self) -> &'static str {
        "sat"
    }

    /// The native unit required from the authenticated `GetSettings` response.
    #[must_use]
    pub const fn settings_unit(self) -> &'static str {
        match self {
            Self::LdkServer => "msat",
            Self::Bark => "sat",
        }
    }

    #[must_use]
    pub const fn binding_description(self) -> &'static str {
        match self {
            Self::LdkServer => "bolt11/sat and bolt12/sat",
            Self::Bark => "bolt11/sat",
        }
    }

    /// Require every declared method exactly once, with no missing qualifiers,
    /// duplicate bindings, extra rails or alternative units.
    pub fn accepts_bindings<'a>(
        self,
        bindings: impl IntoIterator<Item = Option<&'a DependencyBinding>>,
    ) -> bool {
        let mut seen = BTreeSet::new();
        for binding in bindings {
            let Some(DependencyBinding::Payment { method, unit }) = binding else {
                return false;
            };
            if unit != self.binding_unit()
                || !self.methods().contains(method)
                || !seen.insert(*method)
            {
                return false;
            }
        }
        seen.len() == self.methods().len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownProcessorProfile;

impl fmt::Display for UnknownProcessorProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unknown payment processor profile")
    }
}

impl std::error::Error for UnknownProcessorProfile {}

impl FromStr for ProcessorProfile {
    type Err = UnknownProcessorProfile;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::for_implementation(value).ok_or(UnknownProcessorProfile)
    }
}
