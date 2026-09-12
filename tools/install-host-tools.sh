#!/usr/bin/env bash
# Thin maintainer dispatch. Rust shares reviewed pin validation with setup.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mode=${1:-install}
[[ $# == 0 ]] || shift
case "$mode" in
  install) [[ $# == 0 ]] || exit 2 ;;
  resolve) [[ $# == 2 ]] || { printf 'Usage: just tool-pins TARGET NEW_OUTPUT\n' >&2; exit 2; } ;;
  *) printf 'Expected install or resolve\n' >&2; exit 2 ;;
esac
cd "$root"
unset CARGO_BUILD_TARGET
export CARGO_TARGET_DIR="$root/target/check"
cargo build --quiet --locked -p proofstorm-xtask
exec "$CARGO_TARGET_DIR/debug/proofstorm-xtask" host-tools "$mode" "$root" "$@"
