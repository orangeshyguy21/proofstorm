#!/usr/bin/env bash
# Mac CI uses a native compiler, not Docker; runtime acceptance is a separate gate.
set -Eeuo pipefail
stage=arguments
trap 'printf "Mac bundle check failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
work='' controller=''
usage() { printf 'Usage: just release-ci-macos --work-dir NEW_EXTERNAL_DIRECTORY --controller-receipt FILE\n'; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-dir|--controller-receipt)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in --work-dir) work=$2 ;; --controller-receipt) controller=$2 ;; esac
      shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$work" && -n "$controller" ]] || { usage >&2; exit 2; }
[[ $(uname -s)/$(uname -m) == Darwin/arm64 ]] || { printf 'Run on native Apple Silicon macOS\n' >&2; exit 1; }
work="$(cd -- "$(dirname -- "$work")" && pwd -P)/$(basename -- "$work")"
case "$work" in "$root"|"$root/"*) printf 'Choose a work directory outside the checkout\n' >&2; exit 2 ;; esac
[[ ! -e "$work" && ! -L "$work" ]] || { printf 'Work directory must be new\n' >&2; exit 2; }
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do [[ -z "$variable" ]] || unset "$variable"; done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
mkdir "$work"
cd "$root"
stage='optimized native bundle build'
bash scripts/release-build.sh --work-dir "$work/build" --output "$work/artifacts" --controller-receipt "$controller" 2>&1 | tee "$work/build.log"
stage='build outputs'
[[ -s "$work/build/result.json" && ! -L "$work/build/result.json" ]] || exit 1
cp "$work/build/result.json" "$work/artifacts/build-report.json"
set -- "$work/artifacts/"proofstorm-*-macos-arm64.tar.gz
[[ $# == 1 && -f "$1" && ! -L "$1" ]] || { printf 'Expected exactly one Mac bundle\n' >&2; exit 1; }
archive=$1
for file in "$archive.sha256" "$work/artifacts/build-report.json" "$work/build/source/install.sh"; do
  [[ -s "$file" && ! -L "$file" ]] || { printf 'Missing/invalid Mac build output: %s\n' "$file" >&2; exit 1; }
done
stage='relocated CLI and MCP verification'
CARGO_TARGET_DIR="$work/verifier" cargo build --quiet --locked -p proofstorm-xtask
"$work/verifier/debug/proofstorm-xtask" release-smoke "$archive" "$work/relocated" --deny-source "$root" --deny-source "$work/build/source"
stage='isolated install and reinstall'
bash scripts/macos-install-smoke.sh --archive "$archive" --installer "$work/build/source/install.sh" \
  --snapshot "$work/build/source" --work-dir "$work/install" 2>&1 | tee "$work/install.log"
stage='artifact collection'
for file in "$work/relocated/smoke-report.json" "$work/install/install-smoke-report.json"; do [[ -s "$file" && ! -L "$file" ]] || exit 1; done
mkdir "$work/bundle"
cp "$archive" "$archive.sha256" "$work/artifacts/build-report.json" "$work/relocated/smoke-report.json" "$work/install/install-smoke-report.json" "$work/bundle/"
cp "$work/build/source/install.sh" "$work/bundle/install.sh"
printf 'Mac bundle and isolated installer checks passed: %s/bundle\nRuntime setup and native GUI acceptance remain untested. Nothing was published.\n' "$work"
