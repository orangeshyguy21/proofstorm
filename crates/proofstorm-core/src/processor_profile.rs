//! Audited CDK gRPC capabilities, selected by implementation rather than inferred
//! from an endpoint's response. A profile alone does not enable a catalog entry.
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use serde_json::Value;

use crate::{DependencyBinding, PaymentMethod, processor_ids};

/// Authored processor setting that selects the advertised subset.
pub const PAYMENT_METHODS_FIELD: &str = "payment_methods";

/// Bark's custom method for out-of-round Ark payments.
const BARK_ARKOOR: &str = "arkoor";

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

    /// Every method the native processor can advertise, custom methods
    /// included. Upstream advertises all of them when no subset is selected.
    #[must_use]
    pub fn supported_methods(self) -> BTreeSet<PaymentMethod> {
        match self {
            Self::LdkServer => [PaymentMethod::Bolt11, PaymentMethod::Bolt12].into(),
            Self::Bark => [
                PaymentMethod::Bolt11,
                PaymentMethod::Onchain,
                PaymentMethod::Custom(BARK_ARKOOR.into()),
            ]
            .into(),
        }
    }

    /// Whether a component selects its advertised subset through
    /// [`PAYMENT_METHODS_FIELD`]. Fixed profiles always advertise every method.
    #[must_use]
    pub const fn configurable(self) -> bool {
        matches!(self, Self::Bark)
    }

    /// Units a mint may bind. Bark serves only sat upstream; the LDK
    /// processor's native msat responses reach the mint through CDK's sat
    /// conversion, the only combination qualified.
    #[must_use]
    pub fn binding_units(self) -> BTreeSet<&'static str> {
        ["sat"].into()
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
            Self::Bark => "one sat binding for each method in the processor's payment_methods",
        }
    }

    /// A non-empty subset this processor can advertise; fixed profiles require all.
    #[must_use]
    pub fn accepts_methods(self, methods: &BTreeSet<PaymentMethod>) -> bool {
        let supported = self.supported_methods();
        !methods.is_empty()
            && methods.is_subset(&supported)
            && (self.configurable() || *methods == supported)
    }

    /// The set a component advertises, or `None` for an invalid selection.
    /// An omitted selection advertises everything, as upstream does.
    #[must_use]
    pub fn advertised_methods(
        self,
        config: &BTreeMap<String, Value>,
    ) -> Option<BTreeSet<PaymentMethod>> {
        let Some(value) = config
            .get(PAYMENT_METHODS_FIELD)
            .filter(|_| self.configurable())
        else {
            return Some(self.supported_methods());
        };
        let items = value.as_array()?;
        let methods = items
            .iter()
            .map(|item| item.as_str()?.parse().ok())
            .collect::<Option<BTreeSet<_>>>()?;
        (methods.len() == items.len() && self.accepts_methods(&methods)).then_some(methods)
    }

    /// Parse a comma-separated method list, rejecting duplicates and
    /// methods outside this profile.
    #[must_use]
    pub fn parse_methods(self, value: &str) -> Option<BTreeSet<PaymentMethod>> {
        let names = value.split(',').collect::<Vec<_>>();
        let methods = names
            .iter()
            .map(|name| name.parse().ok())
            .collect::<Option<BTreeSet<_>>>()?;
        (methods.len() == names.len() && self.accepts_methods(&methods)).then_some(methods)
    }

    /// The unit and method set bound by a mint's links: every binding a
    /// payment binding in one supported unit, each method once, and an
    /// advertisable set.
    pub fn bound_methods<'a>(
        self,
        bindings: impl IntoIterator<Item = Option<&'a DependencyBinding>>,
    ) -> Option<(String, BTreeSet<PaymentMethod>)> {
        let mut bound_unit = None;
        let mut seen = BTreeSet::new();
        for binding in bindings {
            let Some(DependencyBinding::Payment { method, unit }) = binding else {
                return None;
            };
            if !self.binding_units().contains(unit.as_str())
                || bound_unit.is_some_and(|bound| bound != unit)
                || !seen.insert(method.clone())
            {
                return None;
            }
            bound_unit = Some(unit);
        }
        let unit = bound_unit?.clone();
        self.accepts_methods(&seen).then_some((unit, seen))
    }
}

/// The comma-separated form used by native settings and driver arguments.
#[must_use]
pub fn method_list(methods: &BTreeSet<PaymentMethod>) -> String {
    methods
        .iter()
        .map(PaymentMethod::as_str)
        .collect::<Vec<_>>()
        .join(",")
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(methods: &[&str]) -> BTreeSet<PaymentMethod> {
        methods
            .iter()
            .map(|method| method.parse().unwrap())
            .collect()
    }

    #[test]
    fn bark_advertises_any_nonempty_subset_and_defaults_to_all() {
        let all = set(&["bolt11", "onchain", "arkoor"]);
        assert_eq!(ProcessorProfile::Bark.supported_methods(), all);
        let advertised = |value| {
            ProcessorProfile::Bark.advertised_methods(&BTreeMap::from([(
                PAYMENT_METHODS_FIELD.to_string(),
                value,
            )]))
        };
        assert_eq!(
            ProcessorProfile::Bark.advertised_methods(&BTreeMap::new()),
            Some(all)
        );
        assert_eq!(
            advertised(json!(["arkoor", "onchain"])),
            Some(set(&["onchain", "arkoor"]))
        );
        for invalid in [
            json!([]),
            json!(["bolt12"]),
            json!(["bolt11", "bolt11"]),
            json!(["lightning"]),
            json!(["Arkoor"]),
            json!("bolt11"),
        ] {
            assert_eq!(advertised(invalid.clone()), None, "{invalid}");
        }
    }

    #[test]
    fn ldk_keeps_its_fixed_method_set() {
        let config = BTreeMap::from([(PAYMENT_METHODS_FIELD.to_string(), json!(["bolt11"]))]);
        assert_eq!(
            ProcessorProfile::LdkServer.advertised_methods(&config),
            Some(set(&["bolt11", "bolt12"]))
        );
        assert!(!ProcessorProfile::LdkServer.accepts_methods(&set(&["bolt11"])));
    }

    #[test]
    fn method_lists_round_trip_and_reject_ambiguity() {
        let methods = set(&["arkoor", "bolt11"]);
        assert_eq!(method_list(&methods), "bolt11,arkoor");
        assert_eq!(
            ProcessorProfile::Bark.parse_methods("arkoor,bolt11"),
            Some(methods)
        );
        for invalid in ["", "bolt11,", "bolt11,bolt11", "bolt12", "bolt11, onchain"] {
            assert_eq!(
                ProcessorProfile::Bark.parse_methods(invalid),
                None,
                "{invalid:?}"
            );
        }
    }
}
