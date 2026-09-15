set -eu
recipe=$1
prepared="${recipe}.proofstorm-workspace"
trap 'rm -f "$prepared"' EXIT

# The baseline Dockerfiles copy only crates, Cargo.toml and flake.nix. The full
# workspace also needs bindings, both lockfiles and the pinned Rust toolchain.
# Keep the upstream build flags and runtime stage; reject an unknown build shape.
awk '
    /^RUN nix develop .*--command cargo build (--locked )?--release --bin cdk-mintd([[:space:]]|$)/ {
        print "COPY . ."
        sub(/--command cargo build (--locked )?--release/, "--command cargo build --locked --release")
        patched++
    }
    { print }
    END { if (patched != 1) exit 42 }
' "$recipe" > "$prepared" || {
    printf '%s\n' 'Unsupported CDK mint Dockerfile: expected one Nix cargo build for cdk-mintd.' >&2
    exit 1
}
mv "$prepared" "$recipe"
