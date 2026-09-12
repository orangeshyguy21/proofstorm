#!/usr/bin/env bash
# Build/dispatch only. Rust owns isolated setup, reports, and verified teardown.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"
for variable in "${!PROOFSTORM_@}" "${!K3D_@}" "${!HELM_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset KUBECONFIG KUBERNETES_MASTER CARGO_BUILD_TARGET
export CARGO_TARGET_DIR="$root/target/check"
export PROOFSTORM_WEB_DIST="$root/target/check/no-web-assets"
runner=(cargo run --quiet --locked -p proofstorm-acceptance --bin proofstorm-acceptance --)
if [[ ${1:-} == --cleanup ]]; then
  exec "${runner[@]}" "$@"
fi
for argument in "$@"; do
  if [[ "$argument" == --bundle || "$argument" == --bundle=* ]]; then
    exec "${runner[@]}" --root "$root" "$@"
  fi
done
exec "${runner[@]}" --checkout-home "$root/.proofstorm-dev/state" --root "$root" "$@"
