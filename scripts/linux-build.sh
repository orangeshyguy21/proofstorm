#!/usr/bin/env bash
# Host-side Docker orchestration; Rust owns source snapshots and run metadata.
set -Eeuo pipefail
stage=arguments
trap 'printf "Linux build failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
source_dir=$root work='' development=false debug=false
usage() {
  printf 'Usage: just release-build-linux --work-dir NEW_EXTERNAL_DIRECTORY [--source DIRECTORY] [--development] [--debug]\n'
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-dir|--source)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in --work-dir) work=$2 ;; --source) source_dir=$2 ;; esac
      shift 2 ;;
    --development) development=true; shift ;;
    --debug) debug=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$work" ]] || { usage >&2; exit 2; }
for tool in cargo docker git; do
  command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }
done
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-linux-host.XXXXXXXX")
scratch=$(cd "$scratch" && pwd -P)
created=false
cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$created" == true ]]; then
    "$helper" release-run 30 docker logs "$name" > "$work/build.log" 2>&1 || printf 'Could not save build container logs\n' >&2
    if "$helper" release-run 30 docker stop --timeout 10 "$name"; then
      "$helper" release-run 30 docker rm "$name" || printf 'Could not remove test container: %s\n' "$name" >&2
    else
      printf 'Cleanup incomplete. Stop this test container with: docker stop --timeout 10 %s\n' "$name" >&2
    fi
  fi
  rm -rf -- "$scratch"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
stage='maintainer tools'
printf 'Preparing Linux source verification tools\n'
(cd "$root"; CARGO_TARGET_DIR="$scratch/target" cargo build --locked --manifest-path "$root/Cargo.toml" -p proofstorm-xtask)
helper="$scratch/target/debug/proofstorm-xtask"
stage='source snapshot'
"$helper" linux-build-prepare "$source_dir" "$work" "$development" "$debug" > "$scratch/plan"
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 3 ]] || { printf 'Invalid Linux build plan\n' >&2; exit 1; }
work=${plan[0]} name=${plan[1]} tag=${plan[2]}
stage='isolated toolchain'
printf 'Preparing the isolated Linux toolchain (first use downloads build tools)\n'
"$helper" release-run 900 docker buildx build --platform linux/amd64 --load --tag "$tag" "$work/toolchain"
stage='build container'
"$helper" release-run 30 docker create --name "$name" --platform linux/amd64 \
  --cpus 2 --memory 3g --pids-limit 512 --cap-drop ALL --security-opt no-new-privileges \
  "$tag" bash /input/source/scripts/linux-build-worker.sh
created=true
stage='source transport'
"$helper" release-run 180 docker cp "$work/input" "$name:/input"
stage='Linux build and relocation'
printf 'Building in Linux storage: 2 CPUs / 3 GiB; no host mounts\n'
"$helper" release-run 3600 docker start --attach "$name"
status=$("$helper" release-run 30 docker inspect --format '{{.State.ExitCode}}' "$name")
[[ "$status" == 0 ]] || { printf 'Linux worker exited with %s; see container output\n' "$status" >&2; exit 1; }
stage='artifact export'
"$helper" release-run 180 docker cp "$name:/artifacts" "$work/artifacts"
printf 'Linux artifacts and verification reports: %s/artifacts\n' "$work"
