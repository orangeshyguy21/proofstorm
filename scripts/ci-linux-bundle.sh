#!/usr/bin/env bash
# Shared local/CI orchestration. Container transport remains in linux_container.py.
set -Eeuo pipefail
stage=arguments
trap 'printf "Linux bundle check failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
work='' debug=false
usage() {
  printf 'Usage: just release-ci-linux --work-dir NEW_EXTERNAL_DIRECTORY [--debug]\n'
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-dir) [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }; work=$2; shift 2 ;;
    --debug) debug=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$work" ]] || { usage >&2; exit 2; }
# Resolve the existing parent, including macOS /tmp aliases, before path checks.
work="$(cd -- "$(dirname -- "$work")" && pwd -P)/$(basename -- "$work")"
case "$work" in "$root"|"$root/"*) printf 'Choose a work directory outside the checkout\n' >&2; exit 2 ;; esac
[[ ! -e "$work" && ! -L "$work" ]] || { printf 'Work directory must be new\n' >&2; exit 2; }
for tool in python3 docker; do
  command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }
done
mkdir -- "$work"
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
cd "$root"
stage='Linux build and relocation'
printf 'Building Linux bundle in the isolated Debian toolchain\n'
args=(build --work-dir "$work/build")
[[ "$debug" == false ]] || args+=(--debug)
python3 -B scripts/linux_container.py "${args[@]}" 2>&1 | tee "$work/build.log"
stage='build outputs'
# Exactly one normal-channel archive; do not guess or silently select a stale one.
set -- "$work/build/artifacts/"proofstorm-*-x86_64-unknown-linux-gnu.tar.gz
[[ $# == 1 && -f "$1" && ! -L "$1" ]] || { printf 'Expected exactly one Linux bundle\n' >&2; exit 1; }
archive=$1
for file in "$archive.sha256" "$work/build/artifacts/install.sh" \
  "$work/build/artifacts/build-report.json" "$work/build/artifacts/smoke-report.json"; do
  [[ -s "$file" && ! -L "$file" ]] || { printf 'Missing/invalid build output: %s\n' "$file" >&2; exit 1; }
done
stage='source-free install and reinstall'
printf 'Checking install and reinstall without source, build tools, or networking\n'
python3 -B scripts/linux_container.py smoke --archive "$archive" \
  --installer "$work/build/artifacts/install.sh" --work-dir "$work/install" 2>&1 | tee "$work/install.log"
stage='artifact collection'
receipt="$work/install/install-smoke-report.json"
[[ -s "$receipt" && ! -L "$receipt" ]] || { printf 'Installer success report is missing\n' >&2; exit 1; }
# Only these public build outputs are uploadable. Never collect source or caches.
mkdir -- "$work/bundle"
cp -- "$archive" "$archive.sha256" "$work/build/artifacts/install.sh" \
  "$work/build/artifacts/build-report.json" "$work/build/artifacts/smoke-report.json" \
  "$receipt" "$work/bundle/"
printf 'Linux bundle checks passed. Artifacts: %s/bundle\nNo release, container image, or public installer was published. Runtime setup remains untested.\n' "$work"
