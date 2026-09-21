//! Execute one catalog-planned scenario through the ordinary owned runner.
mod mint;
mod workspace;

use anyhow::{Context, Result, bail, ensure};
use proofstorm_qualification::{Case, Scenario};
use serde_json::Value;

use crate::GateContext;

#[derive(Clone)]
pub(crate) struct Observer {
    case: Case,
    seen: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<(String, String)>>>,
}

impl Observer {
    pub(crate) fn new(case: Case) -> Self {
        Self {
            case,
            seen: std::sync::Arc::default(),
        }
    }

    pub(crate) fn document(&self, document: &mut Value) -> Result<()> {
        select_versions(&self.case, document)?;
        let mut seen = self
            .seen
            .lock()
            .map_err(|_| anyhow::anyhow!("qualification observations poisoned"))?;
        for component in document["components"]
            .as_array()
            .context("qualification fixture components missing")?
        {
            let implementation = component["implementation"]
                .as_str()
                .context("missing implementation")?;
            let version = component["version"].as_str().context("missing version")?;
            ensure!(
                self.case
                    .components
                    .iter()
                    .any(|selected| selected.implementation == implementation
                        && selected.version == version),
                "fixture used unplanned {implementation}@{version}"
            );
            seen.insert((implementation.into(), version.into()));
        }
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<()> {
        let expected = self
            .case
            .components
            .iter()
            .map(|component| (component.implementation.clone(), component.version.clone()))
            .collect();
        ensure!(
            *self
                .seen
                .lock()
                .map_err(|_| anyhow::anyhow!("qualification observations poisoned"))?
                == expected,
            "scenario did not instantiate every planned component version"
        );
        Ok(())
    }
}

pub fn require_native(platform: &str) -> Result<()> {
    let machine = std::process::Command::new("uname").arg("-m").output()?;
    ensure!(machine.status.success(), "cannot identify native machine");
    let machine = std::str::from_utf8(&machine.stdout)?.trim();
    ensure!(
        std::env::consts::OS == "linux"
            && matches!(
                (platform, machine),
                ("linux/amd64", "x86_64") | ("linux/arm64", "aarch64")
            ),
        "qualification requires native {platform}; emulation is not accepted"
    );
    Ok(())
}

pub fn select_versions(case: &Case, document: &mut Value) -> Result<()> {
    let Scenario::Gate { versions, .. } = &case.scenario else {
        return Ok(());
    };
    let catalog = proofstorm_qualification::catalog(&case.platform)?;
    for component in document["components"]
        .as_array_mut()
        .context("qualification fixture has no components")?
    {
        let implementation = component["implementation"]
            .as_str()
            .context("component implementation missing")?;
        if let Some(version) = versions.get(implementation) {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == implementation && entry.version == *version)
                .context("unavailable qualification version")?;
            component["version"] = version.clone().into();
            component["config_version"] = entry.config_version.clone().into();
        }
    }
    Ok(())
}

pub fn run(context: &GateContext) -> Result<()> {
    let case = context
        .qualification
        .as_ref()
        .context("missing qualification case")?;
    match &case.scenario {
        Scenario::Mint { configuration } => mint::run(context, configuration),
        Scenario::Gate { name, .. } if name == "workspace-persistence" => workspace::run(context),
        Scenario::Gate { name, .. } => {
            ensure!(name != "qualification", "recursive qualification gate");
            ensure!(
                !name.starts_with("cdk-bdk")
                    || !case.claims.iter().any(|claim| claim.contains(":wallet:")),
                "CDK BDK qualification cannot certify the advertised Nutshell wallet pairing: the current wallet adapter supports BOLT11 funding, while this mint is on-chain-only"
            );
            crate::gates::run(name, context)
        }
        _ => bail!("image and standalone Lightning qualification do not require a managed runtime"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_planned_native_platform_can_bootstrap_a_checkout_runtime() {
        use proofstorm_core::tool_pins::{LINUX_AMD64, LINUX_ARM64, Pins};
        let plan = proofstorm_qualification::plan(
            proofstorm_qualification::Identity {
                revision: "a".repeat(40),
                run_id: "0".into(),
                attempt: 1,
            },
            true,
        )
        .unwrap();
        let platforms: std::collections::BTreeSet<_> = plan
            .cases
            .iter()
            .map(|case| case.platform.as_str())
            .collect();
        for platform in platforms {
            let target = match platform {
                "linux/amd64" => LINUX_AMD64,
                "linux/arm64" => LINUX_ARM64,
                _ => panic!("qualification runner has no host target"),
            };
            let arch = proofstorm_app::platform::container_arch_for(target).unwrap();
            assert_eq!(format!("linux/{arch}"), platform);
            Pins::parse(
                target,
                proofstorm_app::platform::bootstrap_pins_for(target).unwrap(),
            )
            .unwrap();
        }
    }
}
