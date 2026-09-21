#!/usr/bin/env bash
# Build matching artifacts once per native architecture. No registry publishing.
set -euo pipefail
[[ $# == 2 ]] || exit 2
platform=$1 output=$2
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"
case "$platform:$(uname -s)/$(uname -m)" in
  linux/amd64:Linux/x86_64|linux/arm64:Linux/aarch64) ;;
  *) exit 1 ;;
esac
mkdir -p "$output"
just dev-build
CARGO_TARGET_DIR="$root/target/check" cargo build --locked -p proofstorm-acceptance -p proofstorm-qualification -p proofstorm-xtask
resources=$(jq -er '.resources' .proofstorm-dev/state/checkout-artifacts.json)
sha=$(jq -er '.sha256' "$resources/controller-source.json")
docker buildx build --platform "$platform" --load --provenance=false \
  --build-arg CARGO_BUILD_JOBS=2 --build-arg "PROOFSTORM_CONTROLLER_SOURCE_SHA256=$sha" \
  --file "$resources/controller-source/Dockerfile.proofstormd" \
  --tag "proofstorm-checkout-source:$sha" "$resources/controller-source"
docker image save "proofstorm-checkout-source:$sha" --output "$output/controller.tar"
# Preserve the registration's absolute checkout path and executable permissions.
# Hosted runners use the same workspace path for every job in this repository.
tar -cf "$output/host.tar" .proofstorm-dev/owner.json .proofstorm-dev/build.json \
  .proofstorm-dev/state/checkout-artifacts.json .proofstorm-dev/resources .proofstorm-dev/web \
  .proofstorm-dev/target/debug/proofstorm .proofstorm-dev/target/debug/proofstorm-mcp \
  target/check/debug/proofstorm-acceptance target/check/debug/proofstorm-qualification target/check/debug/proofstorm-xtask
(cd "$output" && sha256sum host.tar controller.tar > SHA256SUMS)
