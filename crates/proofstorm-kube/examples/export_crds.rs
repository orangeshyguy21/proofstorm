use std::{fs, path::PathBuf};

use kube::CustomResourceExt;
use proofstorm_kube::{ProofstormCandidateBuild, ProofstormLab, ProofstormLabAction};

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("charts/proofstorm/crds"), PathBuf::from);
    fs::create_dir_all(&output)?;
    fs::write(
        output.join("proofstorm.dev_proofstormlabs.yaml"),
        serde_yaml::to_string(&ProofstormLab::crd())?,
    )?;
    fs::write(
        output.join("proofstorm.dev_proofstormlabactions.yaml"),
        serde_yaml::to_string(&ProofstormLabAction::crd())?,
    )?;
    fs::write(
        output.join("proofstorm.dev_proofstormcandidatebuilds.yaml"),
        serde_yaml::to_string(&ProofstormCandidateBuild::crd())?,
    )?;
    Ok(())
}
