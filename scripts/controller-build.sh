#!/usr/bin/env bash
# Alpha controller build/publication. No deployment, mutable release tags, or Python.
set -Eeuo pipefail
stage=arguments
trap 'printf "Controller stopped during %s (line %s, status %s). Published images, if any, remain for inspection; no release was created.\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mode=${1:-help}; [[ $# == 0 ]] || shift
work='' namespace='' platform=''
usage() { printf 'Usage: bash scripts/controller-build.sh build --work-dir NEW_EXTERNAL_DIRECTORY [--platform linux/amd64|linux/arm64]\n       bash scripts/controller-build.sh publish --work-dir DIRECTORY --confirm-namespace ghcr.io/orangeshyguy21/proofstorm\nBuild defaults to linux/amd64. Publication uses the recorded build platform.\n'; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-dir|--confirm-namespace|--platform)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in --work-dir) work=$2 ;; --confirm-namespace) namespace=$2 ;; --platform) platform=$2 ;; esac
      shift 2 ;;
    *) usage >&2; exit 2 ;;
  esac
done
case "$mode" in
  help|--help|-h) usage; exit 0 ;;
  build) [[ -n "$work" && -z "$namespace" ]] || { usage >&2; exit 2; }; platform=${platform:-linux/amd64}; [[ "$platform" == linux/amd64 || "$platform" == linux/arm64 ]] || { usage >&2; exit 2; } ;;
  publish) [[ -n "$work" && -z "$platform" && "$namespace" == ghcr.io/orangeshyguy21/proofstorm ]] || { usage >&2; exit 2; } ;;
  *) usage >&2; exit 2 ;;
esac
for tool in cargo docker curl; do command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }; done
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do [[ -z "$variable" ]] || unset "$variable"; done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
stage='maintainer verifier'
(cd "$root"; CARGO_TARGET_DIR="$root/target/check" cargo build --quiet --locked -p proofstorm-xtask)
helper="$root/target/check/debug/proofstorm-xtask"
if [[ "$mode" == build ]]; then
  stage='clean source snapshot'
  plan_file=$(mktemp "${TMPDIR:-/tmp}/proofstorm-controller-plan.XXXXXXXX")
  trap 'rm -f -- "$plan_file"' EXIT
  "$helper" release-controller prepare "$root" "$work" "$platform" > "$plan_file"
  plan=()
  while IFS= read -r -d '' field; do plan+=("$field"); done < "$plan_file"
  [[ ${#plan[@]} == 3 ]] || exit 1
  work=${plan[0]} tag=${plan[1]} source_sha=${plan[2]}
  stage='controller and execution helper build'
  printf 'Building the matching %s controller and execution helper...\n' "$platform"
  "$helper" release-run 3600 docker buildx build --platform "$platform" --provenance=false --load \
    --build-arg CARGO_BUILD_JOBS=2 --build-arg "PROOFSTORM_CONTROLLER_SOURCE_SHA256=$source_sha" \
    --file "$work/source/Dockerfile.proofstormd" --tag "$tag" "$work/source"
else
  work=$(cd "$work" && pwd -P)
  tag=$("$helper" release-controller tag "$work")
  platform=$("$helper" release-controller platform "$work")
fi
stage='non-root metadata and helper verification'
"$helper" release-run 30 docker image inspect "$tag" > "$work/inspect.json"
image_id=$("$helper" release-run 30 docker image inspect --format '{{.Id}}' "$tag")
[[ "$image_id" =~ ^sha256:[0-9a-f]{64}$ ]] || exit 1
probe=(run --rm --platform "$platform" --network none --read-only --cap-drop ALL
  --security-opt no-new-privileges --memory 128m --cpus 1 --pids-limit 128)
"$helper" release-run 60 docker "${probe[@]}" "$image_id" --release-info > "$work/metadata.json"
# Preserve the helper's exact stderr/exit code, including its expected error path.
"$helper" release-controller helper "$work" "$image_id"
"$helper" release-controller local "$work"
if [[ "$mode" == build ]]; then
  printf 'Controller build and startup checks passed. No image was published.\n'
  exit 0
fi
stage='GHCR publication'
printf 'Publishing verified controller to GHCR...\n'
"$helper" release-run 900 docker push "$tag"
"$helper" release-run 60 docker buildx imagetools inspect "$tag" --format '{{json .Manifest}}' > "$work/published.json"
stage='anonymous digest, platform, identity and layer availability verification'
"$helper" release-controller published "$work"
printf 'Controller publication verified. Exact bundle input: %s/controller.json\n' "$work"
