#!/usr/bin/env bash
# Release command orchestration only; Rust owns snapshots, metadata and packaging.
set -Eeuo pipefail
stage=arguments
trap 'printf "Release build failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
source_dir=$root
work='' output='' target='' trunk='' provenance=''
development=false debug=false json_output=false
usage() {
  printf '%s\n' 'Usage: just release-build --work-dir NEW_DIRECTORY --output DIRECTORY [--source DIRECTORY] [--target-dir DIRECTORY] [--trunk FILE] [--development] [--debug] [--json]' \
    'Transported snapshots: add --provenance SOURCE_JSON; the full source fingerprint is verified before use.'
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source|--work-dir|--output|--target-dir|--trunk|--provenance)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in
        --source) source_dir=$2 ;; --work-dir) work=$2 ;; --output) output=$2 ;;
        --target-dir) target=$2 ;; --trunk) trunk=$2 ;; --provenance) provenance=$2 ;;
      esac
      shift 2 ;;
    --development) development=true; shift ;;
    --debug) debug=true; shift ;;
    --json) json_output=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$work" && -n "$output" ]] || { usage >&2; exit 2; }
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
export NO_COLOR=true

# This disposable bootstrap never changes checkout targets or registered binaries.
stage='maintainer tools'
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-release-build.XXXXXXXX")
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
printf 'Preparing release build tools\n' >&2
(cd "$root"; CARGO_TARGET_DIR="$scratch/target" cargo build --locked --manifest-path "$root/Cargo.toml" -p proofstorm-xtask) >&2
helper="$scratch/target/debug/proofstorm-xtask"
stage='source snapshot and build plan'
"$helper" release-prepare "$source_dir" "$work" "$output" "$target" "$trunk" "$provenance" "$development" "$debug" > "$scratch/plan"
# NUL-delimited fields preserve paths literally. Never eval source-derived text.
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 8 ]] || { printf 'Invalid release build plan\n' >&2; exit 1; }
snapshot=${plan[0]} work=${plan[1]} output=${plan[2]} target=${plan[3]} trunk=${plan[4]}
export CARGO_TARGET_DIR="$target" PROOFSTORM_WEB_DIST="$snapshot/crates/proofstorm-web/dist" \
  PROOFSTORM_BUILD_REVISION="${plan[5]}" PROOFSTORM_BUILD_SOURCE_SHA256="${plan[6]}"
expected_target=${plan[7]}
stage='web assets'
printf 'Building web assets from the isolated source snapshot\n' >&2
(cd "$snapshot/crates/proofstorm-web"; "$trunk" build --release --locked) >&2
stage='host binaries'
printf 'Building CLI and MCP with required embedded assets\n' >&2
export PROOFSTORM_REQUIRE_WEB_ASSETS=1
profile=release
host_args=(build --locked -p proofstorm-app -p proofstorm-mcp --bins)
if [[ "$debug" == true ]]; then profile=debug; else host_args+=(--release); fi
(cd "$snapshot"; cargo "${host_args[@]}") >&2
stage='CRD generation'
(cd "$snapshot"; cargo run --locked -p proofstorm-kube --example export_crds -- "$snapshot/charts/proofstorm/crds") >&2
stage='host metadata'
"$target/$profile/proofstorm" release-info > "$scratch/host-info.json"
"$helper" release-host-check "$scratch/host-info.json" "$expected_target"
stage='bundle packaging'
printf 'Packaging and verifying the release bundle\n' >&2
package_args=(release-package "$snapshot" "$target/$profile" "$work/source.json" "$output" --json)
[[ "$development" == false ]] || package_args+=(--development)
"$helper" "${package_args[@]}" > "$work/result.json.pending"
mv "$work/result.json.pending" "$work/result.json"
if [[ "$json_output" == true ]]; then cat "$work/result.json"; else printf 'Bundle ready in %s\nBuild report: %s/result.json\nRelease readiness remains unverified.\n' "$output" "$work"; fi
