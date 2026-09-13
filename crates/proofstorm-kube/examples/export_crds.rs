use std::{fs, path::PathBuf};

use kube::CustomResourceExt;
use proofstorm_kube::{ProofstormCandidateBuild, ProofstormCell, ProofstormCellAction};

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("charts/proofstorm/crds"), PathBuf::from);
    fs::create_dir_all(&output)?;
    fs::write(
        output.join("proofstorm.dev_proofstormcells.yaml"),
        serde_yaml::to_string(&ProofstormCell::crd())?,
    )?;
    fs::write(
        output.join("proofstorm.dev_proofstormcellactions.yaml"),
        serde_yaml::to_string(&ProofstormCellAction::crd())?,
    )?;
    fs::write(
        output.join("proofstorm.dev_proofstormcandidatebuilds.yaml"),
        serde_yaml::to_string(&ProofstormCandidateBuild::crd())?,
    )?;
    Ok(())
}
