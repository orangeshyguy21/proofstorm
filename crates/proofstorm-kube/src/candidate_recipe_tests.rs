use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

const PREPARE: &str =
    include_str!("../../proofstorm-core/src/candidate_profiles/cdk-mint-workspace-v6.sh");

fn prepare(input: &str) -> (std::process::Output, String) {
    let directory = tempfile::tempdir().unwrap();
    let recipe = directory.path().join("candidate Dockerfile");
    fs::write(&recipe, input).unwrap();
    let mut child = Command::new("sh")
        .args(["-s", "--"])
        .arg(&recipe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(PREPARE.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (output, fs::read_to_string(recipe).unwrap())
}

#[test]
fn candidate_mint_builds_copy_complete_workspace_and_keep_upstream_features() {
    // Reproduces the selective COPY instructions at the frozen CDK baseline,
    // which omitted workspace bindings and both dependency lockfiles.
    // Both upstream Dockerfile feature layouts patch into the one CDK recipe.
    for features in [
        "--features postgres --features prometheus",
        "--features ldk-node --features prometheus --features postgres",
    ] {
        let original = format!(
            "FROM nixos/nix:latest AS builder\nWORKDIR /usr/src/app\nCOPY flake.nix ./flake.nix\nCOPY Cargo.toml ./Cargo.toml\nCOPY crates ./crates\nRUN nix develop --extra-experimental-features flakes --command cargo build --release --bin cdk-mintd {features}\nFROM debian:trixie-slim\nCMD [\"cdk-mintd\"]\n"
        );
        let (output, patched) = prepare(&original);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let compile = patched.find("RUN nix develop").unwrap();
        assert!(patched[..compile].ends_with("COPY . .\nCOPY --from=proofstorm-management-client /src/target/release/cdk-mint-cli /tmp/proofstorm-cdk-mint-cli\n"));
        assert!(patched.contains(&format!(
            "cargo build --locked --release --jobs 1 --features ldk-node,postgres --bin cdk-mintd {features}"
        )));
        assert!(patched.ends_with("FROM debian:trixie-slim\nCMD [\"cdk-mintd\"]\n"));
        let profile = proofstorm_core::candidate_build_profile("cdk").unwrap();
        assert_eq!(profile.version, 9);
        assert_eq!(profile.id, "cdk-mint-source");
        assert_eq!(profile.dockerfile, "Dockerfile.ldk-node");
        assert!(profile.prepare.contains(PREPARE));
        assert!(
            profile
                .prepare
                .contains("cargo build --locked --release --jobs 2 --bin cdk-mint-cli")
        );
        assert!(
            profile
                .prepare
                .contains("apt-get install -y --no-install-recommends wget ca-certificates")
        );
        assert!(profile.prepare.contains(
            "RUN cdk-mintd --version && cdk-mint-cli --version && wget --version >/dev/null"
        ));
    }
}

#[test]
fn candidate_mint_unknown_build_shapes_fail_without_rewriting_source() {
    for input in [
        "FROM rust\nRUN cargo build\n",
        "FROM nixos/nix\n",
        "RUN nix develop --command cargo build --release --bin cdk-mintd --no-default-features\n",
    ] {
        let (output, after) = prepare(input);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Unsupported CDK mint Dockerfile")
        );
        assert_eq!(after, input);
    }
}
