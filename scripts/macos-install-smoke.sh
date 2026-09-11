#!/usr/bin/env bash
# Test a trusted native Mac bundle; never relax sandbox checks or use a development override.
set -Eeuo pipefail
stage=arguments
trap 'printf "Mac installer check failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
archive='' installer='' work='' snapshot=''
usage() { printf 'Usage: just release-install-macos --archive FILE --installer FILE --snapshot SOURCE_DIRECTORY --work-dir NEW_EXTERNAL_DIRECTORY\n'; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive|--installer|--snapshot|--work-dir)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in --archive) archive=$2 ;; --installer) installer=$2 ;; --snapshot) snapshot=$2 ;; --work-dir) work=$2 ;; esac
      shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$archive" && -n "$installer" && -n "$snapshot" && -n "$work" ]] || { usage >&2; exit 2; }
[[ $(uname -s)/$(uname -m) == Darwin/arm64 && -x /usr/bin/sandbox-exec ]] || { printf 'Native Apple Silicon macOS with sandbox-exec is required\n' >&2; exit 1; }
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do [[ -z "$variable" ]] || unset "$variable"; done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-mac-install.XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
stage='maintainer verifier'
(cd "$root"; CARGO_TARGET_DIR="$scratch/target" cargo build --quiet --locked -p proofstorm-xtask)
helper="$scratch/target/debug/proofstorm-xtask"
stage='checked installer inputs'
"$helper" macos-install prepare "$root" "$snapshot" "$archive" "$installer" "$work" > "$scratch/plan"
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 2 ]] || exit 1
work=${plan[0]} archive_name=${plan[1]}
cp "$root/scripts/macos-install-check.sh" "$work/worker.sh"
stage='source, network, compiler and write isolation'
"$helper" release-run 30 "$helper" macos-install isolation "$work"
stage='isolated install and reinstall'
printf 'Testing native Mac install and reinstall with source, compiler execution, and networking denied\n'
status=0
"$helper" release-run 180 /usr/bin/env -i HOME="$work/home" TMPDIR="$work/tmp" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
  PROOFSTORM_HOME="$work/runtime-must-not-exist" \
  /usr/bin/sandbox-exec -f "$work/isolation.sb" /bin/bash "$work/worker.sh" "$work" "$archive_name" || status=$?
stage='verification receipt'
"$helper" macos-install finish "$work" "$status"
printf 'Mac installer checks passed: %s/install-smoke-report.json\n' "$work"
