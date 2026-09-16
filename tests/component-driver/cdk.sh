#!/bin/sh
set -eu
if command -v python || command -v python3; then exit 1; fi
test "$(id -u)" = 1000
cdk-cli --version
/opt/proofstorm/driver --self-check
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
for wallet in alice bob; do
    cdk-cli --work-dir "$scratch/$wallet" --unit sat --non-interactive balance
    test -s "$scratch/$wallet/seed"
    test -s "$scratch/$wallet/cdk-cli.sqlite"
done
alice=$(sha256sum "$scratch/alice/seed" | cut -d ' ' -f 1)
bob=$(sha256sum "$scratch/bob/seed" | cut -d ' ' -f 1)
test "$alice" != "$bob"
cdk-cli --work-dir "$scratch/alice" --unit sat --non-interactive balance
test "$(sha256sum "$scratch/alice/seed" | cut -d ' ' -f 1)" = "$alice"
printf '{"cdk_cli_initialization":true,"wallet_seed_isolation":true,"reopen_preserves_identity":true}\n'
