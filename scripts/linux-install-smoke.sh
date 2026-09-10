#!/usr/bin/env bash
# Bash owns Docker sequencing; Rust checks inputs, deadlines, and JSON receipts.
set -Eeuo pipefail
stage=arguments
trap 'printf "Linux installer check failed during %s (line %s, status %s)\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
archive='' installer='' work='' development=false
usage() {
  printf 'Usage: just release-install-linux --archive FILE --installer FILE --work-dir NEW_EXTERNAL_DIRECTORY [--development]\n'
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive|--installer|--work-dir)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in --archive) archive=$2 ;; --installer) installer=$2 ;; --work-dir) work=$2 ;; esac
      shift 2 ;;
    --development) development=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$archive" && -n "$installer" && -n "$work" ]] || { usage >&2; exit 2; }
for tool in cargo docker; do
  command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }
done
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-linux-install.XXXXXXXX")
scratch=$(cd "$scratch" && pwd -P)
created=false
cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$created" == true ]]; then
    "$helper" release-run 30 docker logs "$name" > "$work/install.log" 2>&1 || printf 'Could not save container logs\n' >&2
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
printf 'Preparing installer verification tools\n'
(cd "$root"; CARGO_TARGET_DIR="$scratch/target" cargo build --locked --manifest-path "$root/Cargo.toml" -p proofstorm-xtask)
helper="$scratch/target/debug/proofstorm-xtask"
stage='checked public inputs'
"$helper" linux-install-prepare "$root" "$archive" "$installer" "$work" "$development" > "$scratch/plan"
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 4 ]] || { printf 'Invalid installer test plan\n' >&2; exit 1; }
work=${plan[0]} name=${plan[1]} tag=${plan[2]} archive_name=${plan[3]}
stage='source-free input image'
"$helper" release-run 180 docker buildx build --platform linux/amd64 --load --tag "$tag" "$work"
install_check=$(< "$root/scripts/linux-install-check.sh")
stage='isolated test container'
"$helper" release-run 30 docker create --name "$name" --platform linux/amd64 \
  --user 1000:1000 --network none --read-only --tmpfs /tmp:rw,exec,nosuid,nodev,size=768m \
  --cpus 2 --memory 1g --pids-limit 128 --cap-drop ALL --security-opt no-new-privileges \
  "$tag" sh -c "$install_check" install-check "$archive_name" "$development"
created=true
stage='install and reinstall'
printf 'Testing install and reinstall in source-free Debian, with networking disabled\n'
"$helper" release-run 180 docker start --attach "$name"
# Docker attach success alone is not proof of worker success.
status=$("$helper" release-run 30 docker inspect --format '{{.State.ExitCode}}' "$name")
stage='verification receipt'
"$helper" linux-install-finish "$work" "$status"
printf 'Source-free installer test passed: %s/install-smoke-report.json\n' "$work"
