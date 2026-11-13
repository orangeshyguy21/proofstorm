#!/usr/bin/env bash
# Maintainer-only: copy every pinned third-party image into Proofstorm's GHCR
# mirror by exact digest, then verify each one anonymously on both platforms.
# Installs, cells and CI pull these digests from the mirror, never Docker Hub.
#   publish  Copy missing digests. Uses your Docker login for the one-time
#            upstream read and the GHCR write; rerun to resume after a failure.
#   verify   Check every digest anonymously and write release/upstream-mirror.json.
set -Eeuo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mode=${1:-help}
mirror=ghcr.io/osygy/proofstorm/upstream
usage() { printf '%s\n' 'Usage: just upstream-mirror publish' '       just upstream-mirror verify NEW_EXTERNAL_WORK'; }
case "$mode" in
  publish) [[ $# == 1 ]] || { usage >&2; exit 2; } ;;
  verify) [[ $# == 2 ]] || { usage >&2; exit 2; } ;;
  *) usage; [[ "$mode" == help ]]; exit ;;
esac
(cd "$root"; CARGO_TARGET_DIR="$root/target/check" cargo build --quiet --locked -p proofstorm-xtask)
helper="$root/target/check/debug/proofstorm-xtask"
origins=()
while IFS= read -r origin; do origins+=("$origin"); done < <("$helper" catalog-image upstream-list)
digest_of() { timeout 60 docker buildx imagetools inspect "$1" --format '{{json .Manifest}}' 2>/dev/null | jq -er '.digest'; }

if [[ "$mode" == publish ]]; then
  for origin in "${origins[@]}"; do
    digest=${origin#*@}
    if [[ "$(digest_of "$mirror@$digest" || true)" == "$digest" ]]; then
      printf 'present  %s\n' "$origin"; continue
    fi
    tag="$mirror:upload-$(openssl rand -hex 16)"
    timeout 900 docker buildx imagetools create --tag "$tag" "$origin" >/dev/null
    observed=$(digest_of "$mirror@$digest") || { echo "Mirror of $origin is missing after upload" >&2; exit 1; }
    [[ "$observed" == "$digest" ]] || { echo "Mirror digest $observed differs from $origin" >&2; exit 1; }
    printf 'copied   %s\n' "$origin"
  done
  echo 'Published. Make the GHCR package "proofstorm/upstream" public once, then run verify.'
  exit 0
fi

work=$2
[[ "$work" == /* && ! -e "$work" && "$work" != "$root"* ]] || { echo 'Work directory must be new, absolute and outside the checkout' >&2; exit 2; }
mkdir -p "$work"
: > "$work/images.jsonl"
for origin in "${origins[@]}"; do
  digest=${origin#*@}
  for platform in linux/amd64 linux/arm64; do
    report="$work/${digest#sha256:}-${platform#linux/}.json"
    bash "$root/scripts/catalog-image.sh" verify "$mirror@$digest" "$platform" "$report" >/dev/null
    jq -e '.anonymous_verified == true' "$report" >/dev/null || { echo "Anonymous verification failed: $origin $platform" >&2; exit 1; }
  done
  jq -cn --arg origin "$origin" --arg image "$mirror@$digest" \
    '{origin:$origin,image:$image,anonymous_verified:{"linux/amd64":true,"linux/arm64":true}}' >> "$work/images.jsonl"
done
jq -s --arg repository "$mirror" --arg date "$(date -u +%F)" \
  '{format_version:1,date:$date,repository:$repository,images:(sort_by(.origin))}' \
  "$work/images.jsonl" > "$root/release/upstream-mirror.json"
printf 'Verified %s images anonymously; wrote release/upstream-mirror.json\n' "${#origins[@]}"
