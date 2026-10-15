use std::{fs, process::Command};

const RECIPE: &str =
    include_str!("../../proofstorm-core/src/candidate_profiles/cdk-mint-v1.Dockerfile");

#[test]
fn candidate_mint_prepare_writes_the_exact_cargo_recipe() {
    let profile = proofstorm_core::candidate_build_profile("cdk").unwrap();
    assert_eq!(profile.version, 10);
    assert_eq!(profile.id, "cdk-mint-source");
    assert_eq!(profile.dockerfile, "Proofstorm.candidate.Dockerfile");
    // Run the saved preparation against a scratch workspace: the quoted heredoc
    // must reproduce the recipe byte for byte, including its line continuations.
    let directory = tempfile::tempdir().unwrap();
    let workspace = format!("{}/", directory.path().display());
    let output = Command::new("sh")
        .args(["-c", &profile.prepare.replace("/workspace/", &workspace)])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = fs::read_to_string(directory.path().join(&profile.dockerfile)).unwrap();
    assert_eq!(written.trim_end(), RECIPE.trim_end());
}

#[test]
fn candidate_mint_recipe_builds_both_binaries_like_cdk_releases() {
    // Complete frozen workspace, lockfile-pinned, both binaries in one Cargo
    // command with upstream's release-static profile and no Nix dev shell.
    assert!(RECIPE.contains("COPY . ."));
    assert!(RECIPE.contains("cargo build --locked --profile release-static --jobs 1"));
    assert!(
        RECIPE.contains("--features cdk-mintd/ldk-node,cdk-mintd/postgres,cdk-mintd/prometheus")
    );
    assert!(RECIPE.contains("--bin cdk-mintd --bin cdk-mint-cli"));
    // Memory-bounded: LTO off and parallel codegen, other profile settings kept.
    assert!(RECIPE.contains(
        "ENV CARGO_PROFILE_RELEASE_STATIC_LTO=off CARGO_PROFILE_RELEASE_STATIC_CODEGEN_UNITS=16"
    ));
    assert!(!RECIPE.contains("nix"));
    assert!(RECIPE.contains("FROM alpine:3.21@sha256:"));
    assert!(RECIPE.contains(
        "RUN cdk-mintd --version && cdk-mint-cli --version && command -v wget >/dev/null"
    ));
}
