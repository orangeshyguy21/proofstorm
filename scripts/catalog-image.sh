#!/usr/bin/env bash
# Maintainer-only catalog builds/copies. No setup, catalog edits, or automatic pushes.
set -Eeuo pipefail
stage=arguments
trap 'printf "Catalog image stopped during %s (line %s, status %s). Retain the work directory; uploads may need inspection.\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mode=${1:-help}; [[ $# == 0 ]] || shift
usage() {
  printf '%s\n' 'Usage: just catalog-image list' \
    '       just catalog-image build RECIPE linux/amd64|linux/arm64 NEW_EXTERNAL_WORK' \
    '       just catalog-image prepare-copy PINNED_SOURCE PLATFORM NEW_EXTERNAL_WORK' \
    '       just catalog-image publish WORK --confirm-namespace ghcr.io/orangeshyguy21/proofstorm' \
    '       just catalog-image verify-work WORK' \
    '       just catalog-image verify PUBLISHED_IMAGE PLATFORM NEW_REPORT'
}
case "$mode" in
  help|-h|--help) usage; exit 0 ;;
  list) [[ $# == 0 ]] || exit 2 ;;
  build|prepare-copy|verify) [[ $# == 3 ]] || { usage >&2; exit 2; } ;;
  publish) [[ $# == 3 && "$2" == --confirm-namespace && "$3" == ghcr.io/orangeshyguy21/proofstorm ]] || { usage >&2; exit 2; } ;;
  verify-work) [[ $# == 1 ]] || exit 2 ;;
  *) usage >&2; exit 2 ;;
esac
for variable in "${!PROOFSTORM_@}" "${!K3D_@}"; do [[ -z "$variable" ]] || unset "$variable"; done
unset CARGO_BUILD_TARGET
stage='maintainer checks'
(cd "$root"; CARGO_TARGET_DIR="$root/target/check" cargo build --quiet --locked -p proofstorm-xtask)
helper="$root/target/check/debug/proofstorm-xtask"
case "$mode" in
  list) exec "$helper" catalog-image list ;;
  verify) exec "$helper" catalog-image verify "$@" ;;
  build|prepare-copy)
    stage='source preparation'
    prepare=prepare; [[ "$mode" != prepare-copy ]] || prepare='prepare-copy'
    "$helper" catalog-image "$prepare" "$root" "$3" "$1" "$2"
    work=$(cd "$3" && pwd -P)
    if [[ "$mode" == prepare-copy ]]; then printf 'Copy source verified; nothing published. Receipt: %s/image.json\n' "$work"; exit 0; fi ;;
  *) work=$(cd "$1" && pwd -P) ;;
esac
plan_file=$(mktemp)
trap 'rm -f -- "$plan_file"' EXIT
fields() {
  "$helper" catalog-image fields "$work" > "$plan_file"
  plan=()
  while IFS= read -r -d '' field; do plan+=("$field"); done < "$plan_file"
  [[ ${#plan[@]} == 9 ]] || exit 1
  kind=${plan[0]} tag=${plan[1]} platform=${plan[2]} source=${plan[3]}
}
fields
if [[ "$mode" == build ]]; then
  stage='image build'
  args=(buildx build --platform "$platform" --provenance=false --load --file "${plan[4]}" --tag "$tag" --label "dev.proofstorm.source-sha256=${plan[8]}")
  [[ -z ${plan[6]} ]] || args+=(--build-arg "MINT_IMAGE=${plan[6]}")
  printf 'Building catalog image for %s...\n' "$platform"
  "$helper" release-run 3600 docker "${args[@]}" "${plan[5]}"
fi
if [[ "$kind" == build && "$mode" != verify-work ]]; then
  stage='immutable image identity and offline native probes'
  "$helper" release-run 30 docker image inspect "$tag" > "$work/inspect.json"
  image_id=$("$helper" catalog-image inspect "$work")
  "$helper" release-run 60 docker run --rm --platform "$platform" --network none --read-only --cap-drop ALL \
    --security-opt no-new-privileges --memory 256m --cpus 1 --pids-limit 128 --entrypoint sh "$image_id" -ec "${plan[7]}" > "$work/probe.stdout"
  "$helper" catalog-image local "$work"
  fields
fi
if [[ "$mode" == build ]]; then printf 'Image verified; nothing published. Receipt: %s/image.json\n' "$work"; exit 0; fi
if [[ "$mode" == publish ]]; then
  stage='publication authorization and preflight'
  "$helper" catalog-image authorize "$work" "$3"
  stage='GHCR publication'
  if [[ "$kind" == build ]]; then
    "$helper" release-run 30 docker tag "$source" "$tag"
    "$helper" release-run 900 docker push "$tag"
  else
    "$helper" release-run 900 docker buildx imagetools create --prefer-index=false --tag "$tag" "$source"
  fi
  "$helper" catalog-image uploaded "$work"
fi
stage='published digest and anonymous layer verification'
"$helper" catalog-image recheck "$work"
"$helper" release-run 60 docker buildx imagetools inspect "$tag" --format '{{json .Manifest}}' > "$work/published.json"
"$helper" catalog-image published "$work"
printf 'Publication verified: %s/image.json. Catalog and release pins were not changed.\n' "$work"
