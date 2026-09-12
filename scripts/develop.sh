#!/usr/bin/env bash
# Build orchestration only. Rust owns metadata, snapshots, and safe launcher writes.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"

mode=build
target_args=()
usage() {
  printf '%s\n' 'Usage: bash scripts/develop.sh [--shell|--watch-web|--web-only] [--target-dir PATH]'
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --shell|--watch-web|--web-only)
      [[ "$mode" == build || "$mode" == "$1" ]] || { usage >&2; exit 2; }
      mode=$1; shift ;;
    --target-dir)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      target_args=("$2"); shift 2 ;;
    --target-dir=*)
      [[ -n "${1#*=}" ]] || { usage >&2; exit 2; }
      target_args=("${1#*=}"); shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
if [[ "$mode" == --shell && ! -t 0 ]]; then
  printf 'just dev needs an interactive terminal; use just dev-build in automation\n' >&2
  exit 2
fi

# Never inherit a release runtime, another checkout, or cross-compilation target.
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
export NO_COLOR=true

# Maintainer bootstrap is separate from registered host binaries and their cache.
printf 'Preparing checkout build tools\n'
CARGO_TARGET_DIR="$root/target/maintainer" cargo build --quiet --locked -p proofstorm-xtask
helper="$root/target/maintainer/debug/proofstorm-xtask"
if [[ ${#target_args[@]} -gt 0 ]]; then
  target=$("$helper" prepare "$root" "${target_args[0]}")
else
  target=$("$helper" prepare "$root")
fi
work="$root/.proofstorm-dev"
export CARGO_TARGET_DIR="$target" PROOFSTORM_WEB_DIST="$work/web"
trunk="$root/.tools/bin/trunk"
[[ -x "$trunk" ]] || { printf 'Run just web-tools to install the pinned web builder\n' >&2; exit 1; }
web_args=(--release --locked --config "$root/crates/proofstorm-web/Trunk.toml" --dist "$work/web")
if [[ "$mode" == --watch-web ]]; then
  [[ -f "$work/state/checkout-artifacts.json" && ! -L "$work/state/checkout-artifacts.json" ]] || {
    printf 'Run just dev-build first\n' >&2; exit 1;
  }
  printf 'Watching web assets for the managed GUI. Refresh its browser tab after a build.\n'
  exec "$trunk" watch "${web_args[@]}"
fi
printf 'Building checkout assets (no release archive or runtime changes)\n'
"$trunk" build "${web_args[@]}"
if [[ "$mode" == --web-only ]]; then
  printf 'Web assets rebuilt. Refresh the managed GUI tab.\n'
  exit 0
fi
export PROOFSTORM_REQUIRE_WEB_ASSETS=1
cargo build --locked -p proofstorm-app -p proofstorm-mcp --bins
cargo build --locked -p proofstorm-kube --example export_crds
resources=$("$helper" resources "$root")
"$target/debug/proofstorm" --home "$work/state" internal checkout-register --source "$root" \
  --resources "$resources" --mcp "$target/debug/proofstorm-mcp" --web-dist "$work/web" > /dev/null
"$helper" launchers "$root"
printf '\nCheckout ready: %s/bin/proofstorm\n' "$work"
printf '%s\n' 'Commands: setup, doctor, gui, up, agent open.' \
  'Runtime unchanged. Restart an existing GUI: gui stop, then gui.'
if [[ "$mode" == --shell ]]; then
  printf 'Development shell selected. Run proofstorm setup first. Exit returns to your normal shell.\n'
  exec "$helper" shell "$root"
fi
