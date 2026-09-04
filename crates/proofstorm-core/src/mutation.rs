use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    CatalogResponse, ComponentSpec, LabSpec, LinkSpec, validate_catalog_component, validate_lab,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftMutation {
    AddComponent { component: ComponentSpec },
    UpdateComponent { component: ComponentSpec },
    RemoveComponent { component_id: String },
    AddLink { link: LinkSpec },
    RemoveLink { link: LinkSpec },
}

/// Apply one deterministic authoring mutation to a lab draft.
///
/// Component and link order is canonicalized after every mutation. Component
/// removal refuses while links still reference it so an agent cannot
/// accidentally erase topology intent.
///
/// # Errors
///
/// Returns an error for catalog mismatches, duplicate/missing identities,
/// dangling links, policy limit violations, or an otherwise invalid result.
pub fn apply_draft_mutation(
    lab: &mut LabSpec,
    mutation: &DraftMutation,
    catalog: &CatalogResponse,
) -> Result<(), String> {
    let mut candidate = lab.clone();
    match mutation {
        DraftMutation::AddComponent { component } => {
            if candidate
                .components
                .iter()
                .any(|item| item.id == component.id)
            {
                return Err(format!("component {:?} already exists", component.id));
            }
            validate_catalog_component(component, catalog)?;
            candidate.components.push(component.clone());
        }
        DraftMutation::UpdateComponent { component } => {
            validate_catalog_component(component, catalog)?;
            let existing = candidate
                .components
                .iter_mut()
                .find(|item| item.id == component.id)
                .ok_or_else(|| format!("component {:?} does not exist", component.id))?;
            *existing = component.clone();
        }
        DraftMutation::RemoveComponent { component_id } => {
            if candidate
                .links
                .iter()
                .any(|link| link.from == *component_id || link.to == *component_id)
            {
                return Err(format!(
                    "component {component_id:?} still has links; remove them first"
                ));
            }
            let before = candidate.components.len();
            candidate.components.retain(|item| item.id != *component_id);
            if candidate.components.len() == before {
                return Err(format!("component {component_id:?} does not exist"));
            }
        }
        DraftMutation::AddLink { link } => {
            if candidate
                .links
                .iter()
                .any(|candidate| candidate.id == link.id)
            {
                return Err(format!("binding {:?} already exists", link.id));
            }
            candidate.links.push(link.clone());
        }
        DraftMutation::RemoveLink { link } => {
            let before = candidate.links.len();
            candidate.links.retain(|item| item != link);
            if candidate.links.len() == before {
                return Err(format!(
                    "binding {:?} ({:?} {:?} -> {:?}) does not exist",
                    link.id, link.kind, link.from, link.to
                ));
            }
        }
    }
    candidate
        .components
        .sort_by(|left, right| left.id.cmp(&right.id));
    candidate.links.sort();
    let report = validate_lab(&candidate);
    if report.valid {
        *lab = candidate;
        Ok(())
    } else {
        Err(serde_json::to_string(&report.issues)
            .unwrap_or_else(|_| "mutated lab is invalid".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{API_VERSION, ComponentKind, ControlClass, LabPolicy, LinkKind};

    use super::*;

    fn lab() -> LabSpec {
        LabSpec {
            api_version: API_VERSION.into(),
            name: "composed-lab".into(),
            components: vec![],
            links: vec![],
            policy: LabPolicy::default(),
        }
    }

    fn component(
        id: &str,
        kind: ComponentKind,
        implementation: &str,
        control: ControlClass,
    ) -> ComponentSpec {
        ComponentSpec {
            id: id.into(),
            kind,
            implementation: implementation.into(),
            version: None,
            config_version: match implementation {
                "bitcoin-core" => "bitcoin-core/30/v1",
                "lnd" => "lnd/0.20/v1",
                "nutshell-wallet" => "nutshell-wallet/0.20/v1",
                _ => panic!("unknown test implementation {implementation:?}"),
            }
            .into(),
            control,
            config: BTreeMap::new(),
        }
    }

    #[test]
    fn mutations_are_catalog_checked_and_canonically_ordered() {
        let mut lab = lab();
        let catalog = crate::default_catalog();
        for item in [
            component(
                "wallet",
                ComponentKind::Wallet,
                "nutshell-wallet",
                ControlClass::Laboratory,
            ),
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
        ] {
            apply_draft_mutation(
                &mut lab,
                &DraftMutation::AddComponent { component: item },
                catalog,
            )
            .expect("component mutation");
        }
        assert_eq!(
            lab.components
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["chain", "wallet"]
        );
        let invalid = component(
            "fake-wallet",
            ComponentKind::Wallet,
            "bitcoin-core",
            ControlClass::Laboratory,
        );
        assert!(
            apply_draft_mutation(
                &mut lab,
                &DraftMutation::AddComponent { component: invalid },
                catalog,
            )
            .expect_err("kind mismatch")
            .contains("does not match catalog kind")
        );
    }

    #[test]
    fn linked_component_removal_refuses_and_link_kinds_are_typed() {
        let mut lab = lab();
        let catalog = crate::default_catalog();
        for item in [
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "node",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
        ] {
            apply_draft_mutation(
                &mut lab,
                &DraftMutation::AddComponent { component: item },
                catalog,
            )
            .expect("component mutation");
        }
        let link = LinkSpec {
            id: "node-chain".into(),
            kind: LinkKind::ChainBackend,
            from: "node".into(),
            to: "chain".into(),
            binding: Some(crate::DependencyBinding::Chain {
                network: crate::BitcoinNetwork::Regtest,
            }),
        };
        apply_draft_mutation(
            &mut lab,
            &DraftMutation::AddLink { link: link.clone() },
            catalog,
        )
        .expect("typed link");
        assert!(
            apply_draft_mutation(
                &mut lab,
                &DraftMutation::RemoveComponent {
                    component_id: "chain".into(),
                },
                catalog,
            )
            .expect_err("linked removal")
            .contains("remove them first")
        );
        let wrong = LinkSpec {
            id: "wrong-backend".into(),
            kind: LinkKind::PaymentBackend,
            from: "node".into(),
            to: "chain".into(),
            binding: Some(crate::DependencyBinding::Payment {
                method: crate::PaymentMethod::Bolt11,
                unit: "sat".into(),
            }),
        };
        assert!(
            apply_draft_mutation(&mut lab, &DraftMutation::AddLink { link: wrong }, catalog,)
                .expect_err("wrong link kinds")
                .contains("incompatible_link_kinds")
        );
        assert_eq!(lab.links, vec![link]);
    }
}
