#!/usr/bin/env bash
# Container worker: no Docker socket, Python, publication, or runtime setup.
set -Eeuo pipefail
stage=arguments
trap 'printf "Linux build worker failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
input=/input work=/build/job output=/artifacts
while [[ $# -gt 0 ]]; do
  case "$1" in
    --input|--work-dir|--output)
      [[ $# -ge 2 && -n "$2" ]] || exit 2
      case "$1" in --input) input=$2 ;; --work-dir) work=$2 ;; --output) output=$2 ;; esac
      shift 2 ;;
    *) printf 'Usage: bash scripts/linux-build-worker.sh [--input DIRECTORY --work-dir NEW_DIRECTORY --output NEW_DIRECTORY]\n' >&2; exit 2 ;;
  esac
done
[[ "$(uname -s)/$(uname -m)" == Linux/x86_64 ]] || { printf 'Worker must run on Linux x86-64\n' >&2; exit 1; }
input=$(cd "$input" && pwd -P)
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-linux-worker.XXXXXXXX")
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
stage='maintainer tools'
printf 'Preparing Linux build verification tools\n'
(cd "$root"; CARGO_TARGET_DIR="$scratch/target" cargo build --locked --manifest-path "$root/Cargo.toml" -p proofstorm-xtask)
helper="$scratch/target/debug/proofstorm-xtask"
stage='transported source verification'
"$helper" release-worker-prepare "$input" "$work" "$output" > "$scratch/plan"
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 4 ]] || { printf 'Invalid Linux worker plan\n' >&2; exit 1; }
work=${plan[0]} output=${plan[1]} development=${plan[2]} debug=${plan[3]}
stage='pinned web tools'
printf 'Installing pinned web tools in the owned source copy\n'
sh "$work/source/tools/install-trunk.sh"
stage='release build'
args=(--source "$input/source" --provenance "$input/source.json" --work-dir "$work/release-build"
  --output "$output" --target-dir "$work/target" --trunk "$work/source/.tools/bin/trunk" --json)
[[ "$development" == false ]] || args+=(--development)
[[ "$debug" == false ]] || args+=(--debug)
# Rust checks the pristine transported snapshot again; downloads never modify it.
bash "$root/scripts/release-build.sh" "${args[@]}" > "$work/result.json"
stage='relocated CLI and MCP'
set -- "$output/"proofstorm-*-x86_64-unknown-linux-gnu.tar.gz
[[ $# == 1 && -f "$1" && ! -L "$1" ]] || { printf 'Expected exactly one Linux bundle\n' >&2; exit 1; }
"$helper" release-smoke "$1" "$work/relocated"
stage='verified artifact reports'
cp "$work/result.json" "$output/build-report.json"
cp "$work/relocated/smoke-report.json" "$output/smoke-report.json"
cp "$work/source/install.sh" "$output/install.sh"
printf 'Linux build and relocation checks passed\n'
