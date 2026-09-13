#!/usr/bin/env bash
# Verify the image-only contract's dispatch and fail-closed inputs without Docker.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/tests/fixtures" "$fixture/release" "$fixture/crates/proofstorm-kube/tests/golden" "$scratch/bin"
cp "$root/tests/cdk18-config-contract.sh" "$fixture/tests/"
cp -R "$root/tests/fixtures/cdk-mint-secrets" "$fixture/tests/fixtures/"
cp "$root/release/ghcr.json" "$fixture/release/"
for name in cdk cdk-cln-cell cdk-bdk cdk-postgres-cell cdk-ldk; do
  cp "$root/crates/proofstorm-kube/tests/golden/$name.json" "$fixture/crates/proofstorm-kube/tests/golden/"
done
export CONFIG_TRACE="$scratch/trace"
cat > "$scratch/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2 $3 $4" == 'run --rm --network none' ]]
printf 'CALL\n' >> "$CONFIG_TRACE"
printf '<%s>\n' "$@" >> "$CONFIG_TRACE"
if [[ "$*" == *'--entrypoint sh'* ]]; then cat >> "$CONFIG_TRACE"; fi
exit "${FAKE_DOCKER_EXIT:-0}"
SH
chmod +x "$scratch/bin/docker"
export PATH="$scratch/bin:$PATH"
bash "$fixture/tests/cdk18-config-contract.sh" > "$scratch/output"
[[ $(grep -c '^CALL$' "$CONFIG_TRACE") == 6 ]]
[[ $(grep -c '<ghcr.io/orangeshyguy21/proofstorm/cdk' "$CONFIG_TRACE") == 6 ]]
if grep -q 'localhost:5111\|mintd.regtest.toml\|docker/mint/mintd.toml' "$CONFIG_TRACE"; then
  echo 'CDK check still depends on a retired fixture' >&2; exit 1
fi
grep -q 'persistent-state' "$CONFIG_TRACE"
grep -q 'invalid configuration was accepted' "$CONFIG_TRACE"
grep -q 'All generated CDK' "$scratch/output"

: > "$CONFIG_TRACE"
if FAKE_DOCKER_EXIT=42 bash "$fixture/tests/cdk18-config-contract.sh" > "$scratch/output" 2>&1; then
  echo 'CDK image check ignored container failure' >&2; exit 1
fi
[[ $(grep -c '^CALL$' "$CONFIG_TRACE") == 1 ]]
if grep -q 'All generated CDK' "$scratch/output"; then
  echo 'CDK check reported success after a container failure' >&2; exit 1
fi

for mutation in image namespace; do
  : > "$CONFIG_TRACE"
  if [[ "$mutation" == image ]]; then
    file="$fixture/crates/proofstorm-kube/tests/golden/cdk.json"
    jq '(.resources.deployments[].spec.template.spec.initContainers[] | select(.name == "initialize-config") | .image) = "untrusted:latest"' \
      "$file" > "$scratch/bad.json"
    cp "$scratch/bad.json" "$file"
  else
    cp "$root/crates/proofstorm-kube/tests/golden/cdk.json" "$fixture/crates/proofstorm-kube/tests/golden/"
    jq '.namespace = "unapproved.invalid/other"' "$fixture/release/ghcr.json" > "$scratch/bad.json"
    cp "$scratch/bad.json" "$fixture/release/ghcr.json"
  fi
  if bash "$fixture/tests/cdk18-config-contract.sh" > "$scratch/output" 2>&1; then
    echo "CDK image check accepted invalid $mutation" >&2; exit 1
  fi
  [[ ! -s "$CONFIG_TRACE" ]]
done
echo 'CDK config image dispatch checks passed'
