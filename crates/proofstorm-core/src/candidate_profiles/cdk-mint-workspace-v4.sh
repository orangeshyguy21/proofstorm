set -eu
recipe=$1
prepared="${recipe}.proofstorm-workspace"
trap 'rm -f "$prepared"' EXIT

# Preserve the upstream daemon flags and runtime while supplying its complete
# workspace. The cross-stage copy makes the management build finish before Nix
# starts; otherwise BuildKit runs both memory-heavy builds at the same time.
awk '
    /^RUN nix develop .*--command cargo build (--locked )?--release --bin cdk-mintd([[:space:]]|$)/ {
        print "COPY . ."
        print "COPY --from=proofstorm-management-client /src/target/release/cdk-mint-cli /tmp/proofstorm-cdk-mint-cli"
        sub(/--command cargo build (--locked )?--release/, "--command cargo build --locked --release --jobs 2")
        patched++
    }
    { print }
    END { if (patched != 1) exit 42 }
' "$recipe" > "$prepared" || {
    printf '%s\n' 'Unsupported CDK mint Dockerfile: expected one Nix cargo build for cdk-mintd.' >&2
    exit 1
}
mv "$prepared" "$recipe"
