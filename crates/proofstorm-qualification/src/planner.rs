use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use proofstorm_core::{
    AuthenticationMode, CatalogEntry, CatalogPlatform, CatalogResponse, StorageBackend,
    SupportLifecycle, catalog_for_platform, catalog_image_source, digest_json,
};
use serde::Serialize;

use crate::{Case, Component, Identity, MintRoundtrip, Mode, Plan, Scenario};

/// Catalog for an explicitly native qualification target, independent of host.
///
/// # Errors
/// Rejects any platform outside the two supported native Linux architectures.
pub fn catalog(platform: &str) -> Result<CatalogResponse> {
    Ok(catalog_for_platform(match platform {
        "linux/amd64" => CatalogPlatform::LinuxAmd64,
        "linux/arm64" => CatalogPlatform::LinuxArm64,
        _ => bail!("unsupported qualification platform {platform}"),
    }))
}

fn value(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("serializable catalog contract")
}

fn claim(entry: &CatalogEntry, kind: &str, detail: &impl Serialize) -> String {
    format!("{}@{}:{kind}:{}", entry.id, entry.version, value(detail))
}

fn selected(entry: &CatalogEntry) -> bool {
    entry.support_lifecycle.is_supported()
        || matches!(entry.id.as_str(), "ldk-server" | "cdk-ldk-server-processor")
}

fn component(entry: &CatalogEntry) -> Result<Component> {
    Ok(Component {
        implementation: entry.id.clone(),
        version: entry.version.clone(),
        image: entry.image.clone(),
        source: catalog_image_source(&entry.image).map_err(anyhow::Error::msg)?,
    })
}

fn entry<'a>(catalog: &'a CatalogResponse, name: &str, version: &str) -> Result<&'a CatalogEntry> {
    catalog
        .entries
        .iter()
        .find(|entry| entry.id == name && entry.version == version && selected(entry))
        .with_context(|| format!("qualification references unavailable {name}@{version}"))
}

fn preferred<'a>(catalog: &'a CatalogResponse, name: &str) -> Result<&'a CatalogEntry> {
    catalog
        .entries
        .iter()
        .find(|entry| entry.id == name && entry.support_lifecycle == SupportLifecycle::Preferred)
        .with_context(|| format!("qualification has no preferred {name}"))
}

fn obligations(entry: &CatalogEntry) -> BTreeSet<String> {
    let mut claims = BTreeSet::from([
        claim(entry, "image", &entry.image),
        claim(entry, "behavior", &entry.id),
    ]);
    for storage in &entry.support_matrix.storage {
        claims.insert(claim(entry, "storage", storage));
    }
    for auth in &entry.support_matrix.authentication {
        claims.insert(claim(entry, "authentication", auth));
    }
    for binding in &entry.support_matrix.payment_bindings {
        for version in &binding.backend.versions {
            claims.insert(claim(
                entry,
                "payment",
                &(
                    binding.method,
                    &binding.unit,
                    &binding.backend.implementation,
                    version,
                ),
            ));
        }
    }
    for binding in &entry.support_matrix.embedded_payment_bindings {
        claims.insert(claim(entry, "embedded_payment", binding));
    }
    for dependency in &entry.compatible_dependencies {
        for version in &dependency.versions {
            claims.insert(claim(
                entry,
                "dependency",
                &(dependency.link_kind, &dependency.implementation, version),
            ));
        }
    }
    for wallet in &entry.support_matrix.compatible_wallet_adapters {
        for version in &wallet.versions {
            claims.insert(claim(entry, "wallet", &(&wallet.implementation, version)));
        }
    }
    claims
}

fn behavioral(entry: &CatalogEntry) -> BTreeSet<String> {
    obligations(entry)
        .into_iter()
        .filter(|key| !key.contains(":image:"))
        .collect()
}

fn dependencies(entry: &CatalogEntry, components: &[Component]) -> BTreeSet<String> {
    entry
        .compatible_dependencies
        .iter()
        .flat_map(|dependency| {
            components
                .iter()
                .filter(move |component| {
                    dependency.implementation == component.implementation
                        && dependency.versions.contains(&component.version)
                })
                .map(move |component| {
                    claim(
                        entry,
                        "dependency",
                        &(
                            dependency.link_kind,
                            &component.implementation,
                            &component.version,
                        ),
                    )
                })
        })
        .collect()
}

struct Builder<'a> {
    catalog: &'a CatalogResponse,
    platform: &'a str,
    mode: Mode,
    cases: Vec<Case>,
}

impl Builder<'_> {
    fn push(
        &mut self,
        label: &str,
        scenario: Scenario,
        mut components: Vec<Component>,
        claims: BTreeSet<String>,
        baseline: bool,
    ) {
        components
            .sort_by(|a, b| (&a.implementation, &a.version).cmp(&(&b.implementation, &b.version)));
        components.dedup();
        let digest = digest_json(&(self.platform, &scenario, &components, &claims));
        let id = format!("{label}-{}", &digest[7..23]);
        let stress = matches!(&scenario, Scenario::Gate { name, .. }
            if matches!(name.as_str(), "cashu-double-spend" | "cdk-bdk-stress" | "cdk-bdk-postgres-stress"));
        self.cases.push(Case {
            id,
            platform: self.platform.into(),
            scenario,
            components,
            claims,
            required: self.mode == Mode::Full
                || (!stress && (self.mode == Mode::Compatibility || baseline)),
            reason: if stress {
                "opt-in upstream adversarial/stress scenario"
            } else if baseline {
                "fixed compatibility baseline"
            } else if self.mode == Mode::Documentation {
                "documentation-only PR: covered by the main compatibility run"
            } else {
                "supported catalog compatibility"
            }
            .into(),
        });
    }

    fn gate(
        &mut self,
        name: &str,
        subjects: &[&CatalogEntry],
        other_components: &[&str],
        claims: BTreeSet<String>,
        baseline: bool,
    ) -> Result<()> {
        let mut components = subjects
            .iter()
            .map(|entry| component(entry))
            .collect::<Result<Vec<_>>>()?;
        for implementation in other_components {
            if !components
                .iter()
                .any(|entry| entry.implementation == *implementation)
            {
                components.push(component(preferred(self.catalog, implementation)?)?);
            }
        }
        let versions = components
            .iter()
            .map(|component| (component.implementation.clone(), component.version.clone()))
            .collect();
        self.push(
            name,
            Scenario::Gate {
                name: name.into(),
                versions,
            },
            components,
            claims,
            baseline,
        );
        Ok(())
    }

    fn mint(&mut self, mint: &CatalogEntry) -> Result<()> {
        for binding in &mint.support_matrix.payment_bindings {
            if binding.backend.implementation == "cdk-ldk-server-processor" {
                continue; // Explicit experimental scenario below, never hidden by the mint lifecycle.
            }
            ensure!(
                value(&binding.method) == "\"bolt11\"" && binding.unit == "sat",
                "unmapped payment method"
            );
            ensure!(
                matches!(binding.backend.implementation.as_str(), "lnd" | "cln"),
                "unmapped native mint backend"
            );
            for version in &binding.backend.versions {
                let lightning = entry(self.catalog, &binding.backend.implementation, version)?;
                for storage in &mint.support_matrix.storage {
                    ensure!(
                        matches!(storage, StorageBackend::Sqlite | StorageBackend::Postgres),
                        "unmapped mint storage"
                    );
                    for wallet in &mint.support_matrix.compatible_wallet_adapters {
                        ensure!(
                            wallet.implementation == "nutshell-wallet",
                            "unmapped wallet contract"
                        );
                        for wallet_version in &wallet.versions {
                            let wallet =
                                entry(self.catalog, &wallet.implementation, wallet_version)?;
                            let mut components = vec![
                                component(mint)?,
                                component(wallet)?,
                                component(lightning)?,
                                component(preferred(self.catalog, "bitcoin-core")?)?,
                                component(preferred(self.catalog, "lnd")?)?,
                            ];
                            if *storage == StorageBackend::Postgres {
                                components.push(component(preferred(self.catalog, "postgresql")?)?);
                            }
                            let mut claims = dependencies(mint, &components);
                            claims.extend([
                                claim(mint, "behavior", &mint.id),
                                claim(mint, "storage", storage),
                                claim(mint, "authentication", &AuthenticationMode::Unauthenticated),
                                claim(
                                    mint,
                                    "payment",
                                    &(
                                        binding.method,
                                        &binding.unit,
                                        &binding.backend.implementation,
                                        version,
                                    ),
                                ),
                                claim(mint, "wallet", &(&wallet.id, &wallet.version)),
                            ]);
                            claims.extend(behavioral(wallet));
                            if *storage == StorageBackend::Postgres {
                                claims.extend(behavioral(preferred(self.catalog, "postgresql")?));
                            }
                            let baseline = mint.support_lifecycle == SupportLifecycle::Preferred
                                && lightning.support_lifecycle == SupportLifecycle::Preferred
                                && wallet.support_lifecycle == SupportLifecycle::Preferred;
                            self.push(
                                &format!(
                                    "{}-{}-{}",
                                    mint.id,
                                    lightning.id,
                                    if *storage == StorageBackend::Postgres {
                                        "postgres"
                                    } else {
                                        "sqlite"
                                    }
                                ),
                                Scenario::Mint {
                                    configuration: Box::new(MintRoundtrip {
                                        mint: component(mint)?,
                                        wallet: component(wallet)?,
                                        lightning: component(lightning)?,
                                        storage: if *storage == StorageBackend::Postgres {
                                            "postgres"
                                        } else {
                                            "sqlite"
                                        }
                                        .into(),
                                    }),
                                },
                                components,
                                claims,
                                baseline,
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep the explicit catalog-to-scenario mapping together for review"
    )]
    fn build(mut self) -> Result<Vec<Case>> {
        let mut images: BTreeMap<&str, Vec<&CatalogEntry>> = BTreeMap::new();
        for entry in self.catalog.entries.iter().filter(|entry| selected(entry)) {
            images.entry(&entry.image).or_default().push(entry);
        }
        for entries in images.values() {
            self.push(
                &format!("image-{}", entries[0].id),
                Scenario::Image {
                    component: component(entries[0])?,
                },
                entries
                    .iter()
                    .map(|entry| component(entry))
                    .collect::<Result<_>>()?,
                entries
                    .iter()
                    .map(|entry| claim(entry, "image", &entry.image))
                    .collect(),
                true,
            );
        }
        for entry in self
            .catalog
            .entries
            .iter()
            .filter(|entry| entry.support_lifecycle.is_supported())
        {
            match entry.id.as_str() {
                "bitcoin-core" | "nutshell-wallet" | "postgresql" | "redis" => {}
                "keycloak" => self.gate(
                    "keycloak",
                    &[entry],
                    &["postgresql"],
                    behavioral(entry),
                    true,
                )?,
                "lnd" | "cln" => {
                    let bitcoin = preferred(self.catalog, "bitcoin-core")?;
                    let mut claims = behavioral(entry);
                    claims.extend(behavioral(bitcoin));
                    self.push(
                        &format!("lightning-{}", entry.id),
                        Scenario::Lightning {
                            component: component(entry)?,
                        },
                        vec![component(entry)?, component(bitcoin)?],
                        claims,
                        true,
                    );
                }
                "cdk" | "nutshell" => self.mint(entry)?,
                "cdk-ldk" | "cdk-bdk" => {
                    for storage in &entry.support_matrix.storage {
                        let gate = match (entry.id.as_str(), storage) {
                            ("cdk-ldk", StorageBackend::Sqlite) => "cdk-ldk",
                            ("cdk-ldk", StorageBackend::Postgres) => "cdk-ldk-postgres",
                            ("cdk-bdk", StorageBackend::Sqlite) => "cdk-bdk",
                            ("cdk-bdk", StorageBackend::Postgres) => "cdk-bdk-postgres",
                            _ => bail!("unmapped embedded backend/storage"),
                        };
                        if entry.id == "cdk-bdk" {
                            let mut dependencies = vec!["bitcoin-core"];
                            let stress_gate = if *storage == StorageBackend::Postgres {
                                dependencies.push("postgresql");
                                "cdk-bdk-postgres-stress"
                            } else {
                                "cdk-bdk-stress"
                            };
                            self.gate(
                                stress_gate,
                                &[entry],
                                &dependencies,
                                BTreeSet::new(),
                                false,
                            )?;
                        }
                        let mut claims = BTreeSet::from([
                            claim(entry, "behavior", &entry.id),
                            claim(entry, "storage", storage),
                            claim(
                                entry,
                                "authentication",
                                &AuthenticationMode::Unauthenticated,
                            ),
                        ]);
                        for binding in &entry.support_matrix.embedded_payment_bindings {
                            claims.insert(claim(entry, "embedded_payment", binding));
                        }
                        let mut others = if entry.id == "cdk-ldk" {
                            vec!["bitcoin-core", "cln", "nutshell-wallet"]
                        } else {
                            vec!["bitcoin-core"]
                        };
                        if *storage == StorageBackend::Postgres {
                            others.push("postgresql");
                        }
                        let components = others
                            .iter()
                            .map(|name| component(preferred(self.catalog, name)?))
                            .collect::<Result<Vec<_>>>()?;
                        claims.extend(dependencies(entry, &components));
                        if entry.support_matrix.compatible_wallet_adapters.is_empty() {
                            self.gate(gate, &[entry], &others, claims.clone(), false)?;
                        }
                        // Each declared wallet pairing must execute a real round trip.
                        for wallet in &entry.support_matrix.compatible_wallet_adapters {
                            for version in &wallet.versions {
                                let wallet_entry = crate::planner::entry(
                                    self.catalog,
                                    &wallet.implementation,
                                    version,
                                )?;
                                let mut variant_claims = claims.clone();
                                variant_claims.insert(claim(
                                    entry,
                                    "wallet",
                                    &(&wallet.implementation, version),
                                ));
                                self.gate(
                                    gate,
                                    &[entry, wallet_entry],
                                    &others,
                                    variant_claims,
                                    true,
                                )?;
                            }
                        }
                    }
                }
                "cdk-cli-wallet" => self.gate(
                    "cdk-wallet",
                    &[entry],
                    &["bitcoin-core", "lnd", "cdk"],
                    behavioral(entry),
                    true,
                )?,
                "workspace" => self.gate(
                    "workspace-persistence",
                    &[entry],
                    &[],
                    behavioral(entry),
                    true,
                )?,
                other => bail!("supported implementation has no behavioral qualification: {other}"),
            }
        }
        for mint in self
            .catalog
            .entries
            .iter()
            .filter(|entry| entry.id == "nutshell" && entry.support_lifecycle.is_supported())
        {
            let identity = preferred(self.catalog, "keycloak")?;
            let database = preferred(self.catalog, "postgresql")?;
            let mut auth_claims = BTreeSet::new();
            for auth in &mint.support_matrix.authentication {
                if *auth != AuthenticationMode::Unauthenticated {
                    auth_claims.insert(claim(mint, "authentication", auth));
                }
            }
            let authenticating = !auth_claims.is_empty();
            auth_claims.extend(behavioral(identity));
            auth_claims.extend(dependencies(mint, &[component(identity)?]));
            auth_claims.extend(dependencies(identity, &[component(database)?]));
            if authenticating {
                self.gate(
                    "nutshell-oidc",
                    &[mint, identity, database],
                    &["bitcoin-core", "lnd"],
                    auth_claims,
                    true,
                )?;
            }
            let cache = preferred(self.catalog, "redis")?;
            let mut cache_claims = behavioral(cache);
            cache_claims.extend(dependencies(mint, &[component(cache)?]));
            self.gate(
                "cross-implementation-wallet",
                &[mint, cache],
                &["bitcoin-core", "lnd", "cdk", "nutshell-wallet"],
                cache_claims,
                mint.support_lifecycle == SupportLifecycle::Preferred,
            )?;
        }
        let mint = preferred(self.catalog, "cdk")?;
        let processor = self
            .catalog
            .entries
            .iter()
            .find(|entry| entry.id == "cdk-ldk-server-processor")
            .context("missing processor")?;
        let node = self
            .catalog
            .entries
            .iter()
            .find(|entry| entry.id == "ldk-server")
            .context("missing node")?;
        let mut claims = behavioral(processor);
        claims.extend(behavioral(node));
        let backend = vec![component(processor)?];
        claims.extend(dependencies(mint, &backend));
        for binding in &mint.support_matrix.payment_bindings {
            if binding.backend.implementation == processor.id {
                for version in &binding.backend.versions {
                    claims.insert(claim(
                        mint,
                        "payment",
                        &(binding.method, &binding.unit, &processor.id, version),
                    ));
                }
            }
        }
        self.gate(
            "ldk-server-processor",
            &[mint, processor, node],
            &["bitcoin-core", "cdk-cli-wallet"],
            claims,
            false,
        )?;
        for (gate, components, baseline) in [
            ("smoke", vec!["bitcoin-core"], true),
            ("runtime-lifecycle", vec!["bitcoin-core"], true),
            (
                "native-exec",
                vec!["bitcoin-core", "lnd", "cdk", "nutshell-wallet"],
                true,
            ),
            (
                "cashu-double-spend",
                vec!["bitcoin-core", "lnd", "cdk", "nutshell", "cdk-cli-wallet"],
                false,
            ),
            (
                "failed-melt",
                vec!["bitcoin-core", "lnd", "nutshell", "nutshell-wallet"],
                false,
            ),
            (
                "quote-composition",
                vec!["bitcoin-core", "lnd", "cdk", "nutshell-wallet"],
                false,
            ),
            (
                "controller-recovery",
                vec!["bitcoin-core", "lnd", "cln", "cdk", "nutshell-wallet"],
                false,
            ),
        ] {
            self.gate(gate, &[], &components, BTreeSet::new(), baseline)?;
        }
        self.cases.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(self.cases)
    }
}

/// Generate exact obligations and executable cases for the selected suite.
/// Full adds upstream stress tests; compatibility retains all catalog claims.
///
/// # Errors
/// Rejects invalid identities and catalog claims without an explicit scenario.
pub fn plan(identity: Identity, mode: Mode) -> Result<Plan> {
    identity.validate()?;
    let mut plan = Plan {
        format_version: 2,
        identity,
        mode,
        catalog_digests: BTreeMap::new(),
        obligations: BTreeMap::new(),
        cases: vec![],
    };
    for platform in ["linux/amd64", "linux/arm64"] {
        let catalog = catalog(platform)?;
        let required: BTreeSet<_> = catalog
            .entries
            .iter()
            .filter(|entry| selected(entry))
            .flat_map(obligations)
            .collect();
        let cases = Builder {
            catalog: &catalog,
            platform,
            mode,
            cases: vec![],
        }
        .build()?;
        let covered: BTreeSet<_> = cases
            .iter()
            .flat_map(|case| case.claims.iter().cloned())
            .collect();
        let missing: Vec<_> = required.difference(&covered).collect();
        ensure!(
            missing.is_empty(),
            "unmapped qualification obligations for {platform}: {missing:?}"
        );
        ensure!(
            covered.is_subset(&required),
            "qualification claims undeclared support"
        );
        if mode != Mode::Documentation {
            let scheduled: BTreeSet<_> = cases
                .iter()
                .filter(|case| case.required)
                .flat_map(|case| case.claims.iter().cloned())
                .collect();
            ensure!(
                scheduled == required,
                "scheduled suite omits catalog compatibility claims"
            );
        }
        plan.catalog_digests
            .insert(platform.into(), digest_json(&catalog));
        plan.obligations.insert(platform.into(), required);
        plan.cases.extend(cases);
    }
    ensure!(
        plan.cases
            .iter()
            .map(|case| &case.id)
            .collect::<BTreeSet<_>>()
            .len()
            == plan.cases.len(),
        "duplicate qualification case identity"
    );
    Ok(plan)
}
