#!/usr/bin/env bash
# The shared local/CI entrypoint. Checks never install tools or start a runtime.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

mode=${1:-all}
if [[ $# -gt 1 ]]; then
  printf 'Usage: bash scripts/check.sh [all|quick|rust|fmt|shell|clippy|test]\n' >&2
  exit 2
fi

require() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'Missing %s. See scripts/CHECKS.md for check prerequisites.\n' "$1" >&2
    exit 1
  }
}

# Do not replace binaries registered by just dev-build or embed stale GUI assets.
# These are host-code checks, not a distributable GUI build.
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$root/target/check"}
export PROOFSTORM_WEB_DIST="$root/target/check/no-web-assets"
unset PROOFSTORM_REQUIRE_WEB_ASSETS PROOFSTORM_BUILD_REVISION PROOFSTORM_BUILD_SOURCE_SHA256
unset PROOFSTORM_HOME PROOFSTORM_KUBECONFIG

check_fmt() {
  require cargo
  require just
  printf '\nChecking just recipes\n'
  just --summary >/dev/null
  bash scripts/test-just.sh
  bash scripts/test-ci-linux-bundle.sh
  printf '\nChecking Rust formatting\n'
  cargo fmt --all -- --check
}

check_shell() {
  require shellcheck
  printf '\nChecking shell syntax\n'
  # Include new, untracked scripts locally; omit ignored build/vendor directories.
  # Read into an array first so a failed Git invocation cannot silently pass.
  local file
  local files=()
  local listing
  listing=$(git ls-files --cached --others --exclude-standard -- '*.sh')
  while IFS= read -r file; do
    [[ -n "$file" && -f "$file" ]] || continue
    files+=("$file")
  done <<< "$listing"
  for file in "${files[@]}"; do
    case "$(head -n 1 "$file")" in
      '#!/bin/sh'|'#!/usr/bin/env sh') sh -n "$file" ;;
      *) bash -n "$file" ;;
    esac
  done
  printf '\nLinting installer and check tooling\n'
  # Legacy lab/scenario scripts get syntax checks above. Expand strict lint as
  # those workflows are formalized; do not globally suppress their diagnostics.
  shellcheck --external-sources install.sh tools/install-trunk.sh tools/install-host-tools.sh scripts/check.sh scripts/test-just.sh scripts/develop.sh scripts/test-develop.sh scripts/release-build.sh scripts/test-release-build.sh scripts/ci-linux-bundle.sh scripts/test-ci-linux-bundle.sh
}

check_clippy() {
  require cargo
  printf '\nChecking Rust lints\n'
  cargo clippy --locked --workspace --all-targets -- -D warnings
}

check_test() {
  require cargo
  printf '\nRunning hermetic workspace tests\n'
  # Finish the other test binaries after a failure so CI reports all broken suites.
  cargo test --locked --workspace --all-targets --no-fail-fast
}

case "$mode" in
  all) check_fmt; check_shell; check_clippy; check_test ;;
  quick) check_fmt; check_shell ;;
  rust) check_clippy; check_test ;;
  fmt) check_fmt ;;
  shell) check_shell ;;
  clippy) check_clippy ;;
  test) check_test ;;
  *) printf 'Unknown check: %s\n' "$mode" >&2; exit 2 ;;
esac
