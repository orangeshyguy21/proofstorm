#!/usr/bin/env bash
# Read-only native qualification of a payment stack's retained candidates.
# CI checkout only, or --local: a disposable clone on a maintainer's native host.
set -Eeuo pipefail
umask 077
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
usage='Usage: stack-qualification.sh bark|spark [--local] PLATFORM BUILD_REVISION BUILD_ATTEMPT ARTIFACTS NEW_EXTERNAL_WORK'
stack=${1:-}
[[ $# == 0 ]] || shift
case "$stack" in
  bark|spark) ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
local_run=
if [[ ${1:-} == --local ]]; then local_run=1; shift; fi
[[ $# == 5 ]] || { echo "$usage" >&2; exit 2; }
platform=$1 revision=$2 attempt=$3 artifacts=$4
case "$platform:$(uname -s)/$(uname -m)" in
  linux/amd64:Linux/x86_64|linux/arm64:Linux/aarch64) ;;
  # Docker Desktop's Linux engine on Apple silicon runs ARM64 natively.
  linux/arm64:Darwin/arm64) [[ -n "$local_run" ]] || {
    echo 'Managed qualification requires the matching native Linux runner; use --local on this Mac' >&2; exit 1;
  } ;;
  *) echo 'Managed qualification requires the matching native Linux runner' >&2; exit 1 ;;
esac
if [[ -z "$local_run" ]]; then
  # Staging intentionally changes embedded pins and rebuilds the CLI/controller.
  # Refuse to register or replace artifacts in any developer installation.
  [[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_ENVIRONMENT:-} == github-hosted && ! -e "$root/.proofstorm-dev" ]] || {
    echo 'Use a fresh GitHub-hosted checkout for candidate qualification' >&2; exit 1;
  }
fi
[[ -z $(git -C "$root" status --porcelain) ]] || { echo 'Qualification source must start clean' >&2; exit 1; }
work=$(cd -- "$(dirname -- "$5")" && pwd -P)/$(basename -- "$5")
[[ "$work" != "$root" && "$work" != "$root/"* && ! -e "$work" && ! -L "$work" ]] || exit 2
mkdir "$work" "$work/public"
stage=restore container='' owner="$stack-cache-$(openssl rand -hex 12)"
cleanup() {
  result=$?
  trap - EXIT
  if [[ -n "$container" ]]; then
    actual=$(docker inspect --format '{{index .Config.Labels "dev.proofstorm.qualification-cache"}}' "$container") || result=1
    if [[ "$actual" == "$owner" ]]; then
      timeout 30 docker rm --force --volumes "$container" >/dev/null || result=1
    else result=1; fi
  fi
  jq -n --arg stage "$stage" --argjson status "$result" \
    '{format_version:1,stage:$stage,exit_status:$status,published:false}' > "$work/public/status.json"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
# Docker Desktop's daemon runs in a VM and cannot reach a registry on the host's
# loopback, so a local run pushes the exported OCI layout from the host over the
# registry API instead. The manifest bytes, and therefore its digest, are unchanged.
push_layout() {
  local archive=$1 registry=$2 repository=$3 tag=$4 layout manifest media digest location separator
  local fetch=(curl -q --noproxy '*' --fail --silent --show-error)
  layout=$(mktemp -d "$work/layout.XXXXXX")
  tar -xf "$archive" -C "$layout"
  manifest=$(jq -er '.manifests|select(length==1)|.[0].digest|select(test("^sha256:[a-f0-9]{64}$"))|ltrimstr("sha256:")' "$layout/index.json")
  media=$(jq -er '.manifests[0].mediaType' "$layout/index.json")
  for digest in $(jq -er '.config.digest, .layers[].digest|select(test("^sha256:[a-f0-9]{64}$"))' "$layout/blobs/sha256/$manifest"); do
    if "${fetch[@]}" --max-time 30 --head --output /dev/null "http://$registry/v2/$repository/blobs/$digest" 2>/dev/null; then
      continue
    fi
    location=$("${fetch[@]}" --max-time 30 --request POST --dump-header - --output /dev/null \
      "http://$registry/v2/$repository/blobs/uploads/" | tr -d '\r' | sed -n 's/^[Ll]ocation: //p')
    [[ "$location" == http://* ]] || location="http://$registry$location"
    separator='?'
    [[ "$location" != *\?* ]] || separator='&'
    "${fetch[@]}" --max-time 900 --upload-file "$layout/blobs/sha256/${digest#sha256:}" \
      --header 'Content-Type: application/octet-stream' --output /dev/null "$location${separator}digest=$digest"
  done
  "${fetch[@]}" --max-time 60 --upload-file "$layout/blobs/sha256/$manifest" --header "Content-Type: $media" \
    --output /dev/null "http://$registry/v2/$repository/manifests/$tag"
  rm -rf "$layout"
}
checkout=$root
if [[ -n "$local_run" ]]; then
  # Stage pins and build in a disposable clone of this commit. This checkout,
  # its pins files and its .proofstorm-dev installation are only read.
  stage=checkout checkout="$work/checkout"
  head=$(git -C "$root" rev-parse HEAD)
  git clone --quiet --no-hardlinks --no-checkout -- "$root" "$checkout"
  git -C "$checkout" checkout --quiet --detach "$head"
  stage=restore
fi
cd "$checkout"
export CARGO_TARGET_DIR="$checkout/target/check"
cargo build --locked -p proofstorm-xtask
helper="$CARGO_TARGET_DIR/debug/proofstorm-xtask"
# The xtask stack table names the images, pins file and gate.
"$helper" catalog-image stack "$stack" > "$work/stack.json"
gate=$(jq -er '.gate' "$work/stack.json")
pins=$(jq -er '.pins' "$work/stack.json")
listing=$(jq -er '.images[].repository' "$work/stack.json")
repositories=()
while IFS= read -r repository; do repositories+=("$repository"); done <<< "$listing"
"$helper" catalog-image stack-restore ${local_run:+--local} "$stack" "$checkout" "$platform" "$revision" "$attempt" "$artifacts" "$work/candidates"
# shellcheck source=scripts/qualification-docker.sh
source "$root/scripts/qualification-docker.sh"
qualification_docker_config "$work/docker"
engine=$(timeout 30 docker info --format '{{.OSType}}/{{.Architecture}}')
case "$platform:$engine" in
  linux/amd64:linux/x86_64|linux/amd64:linux/amd64|linux/arm64:linux/aarch64|linux/arm64:linux/arm64) ;;
  *) echo 'Docker engine is not native to the selected runner' >&2; exit 1 ;;
esac
stage=native-probes
timeout 300 docker pull registry:2
container=$(docker create --name "$owner" --label "dev.proofstorm.qualification-cache=$owner" \
  --publish 127.0.0.1::5000 registry:2)
docker start "$container" >/dev/null
port=$(docker inspect --format '{{json .NetworkSettings.Ports}}' "$container" | jq -er '.["5000/tcp"]|select(length==1 and .[0].HostIp=="127.0.0.1")|.[0].HostPort')
cache="127.0.0.1:$port"
qualification_cache_endpoint "$cache"
ready=false
for _ in {1..30}; do
  if curl -q --noproxy '*' --fail --silent --max-time 2 "http://$cache/v2/" >/dev/null; then ready=true; break; fi
  sleep 1
done
[[ "$ready" == true ]] || exit 1
for repository in "${repositories[@]}"; do
  candidate="$work/candidates/$repository"
  artifact=$(jq -er '.artifact' "$candidate/handoff.json")
  timeout 300 docker load --input "$artifact/image.tar" >/dev/null
  "$helper" catalog-image fields "$candidate" > "$work/fields.bin"
  fields=()
  while IFS= read -r -d '' field; do fields+=("$field"); done < "$work/fields.bin"
  [[ ${#fields[@]} == 9 && "${fields[2]}" == "$platform" ]] || exit 1
  image=${fields[3]}
  timeout 30 docker image inspect "$image" > "$candidate/inspect.json"
  "$helper" catalog-image inspect "$candidate" >/dev/null
  timeout 60 docker run --rm --platform "$platform" --network none --read-only --cap-drop ALL \
    --security-opt no-new-privileges --memory 256m --cpus 1 --pids-limit 128 --entrypoint sh \
    "$image" -ec "${fields[7]}" > "$candidate/probe.stdout"
  "$helper" catalog-image local "$candidate"
  target="$cache/$stack-candidate/$repository:qualified"
  if [[ -n "$local_run" ]]; then
    push_layout "$artifact/image.tar" "$cache" "$stack-candidate/$repository" qualified
  else
    docker tag "$image" "$target"
    timeout 300 docker push "$target" >/dev/null
  fi
  timeout 30 docker buildx imagetools inspect --raw "$target" > "$candidate/manifest.json"
  digest=$(sha256sum "$candidate/manifest.json" | cut -d' ' -f1)
  config=$(jq -er '.config.digest|select(test("^sha256:[a-f0-9]{64}$"))' "$candidate/manifest.json")
  curl -q --noproxy '*' --fail --silent --show-error --max-time 30 \
    "http://$cache/v2/$stack-candidate/$repository/blobs/$config" > "$candidate/config.json"
  GODEBUG=http2client=0 timeout 300 docker buildx imagetools create --prefer-index=false \
    --tag "$cache/images/$digest:prepared" "${target%:*}@sha256:$digest"
done
stage=build
"$helper" catalog-image stack-stage ${local_run:+--local} "$stack" "$checkout" "$platform" "$work/candidates" "$work/public/staged"
staged="$work/public/staged/$(basename -- "$pins")"
cp "$staged" "$checkout/$pins"
git diff -- "$pins" > "$work/public/catalog.patch"
just dev-build
cargo build --locked -p proofstorm-acceptance
resources=$(jq -er '.resources' .proofstorm-dev/state/checkout-artifacts.json)
sha=$(jq -er '.sha256' "$resources/controller-source.json")
cp "$resources/controller-source.json" "$work/public/controller-source.json"
docker buildx build --platform "$platform" --load --provenance=false \
  --build-arg CARGO_BUILD_JOBS=2 --build-arg "PROOFSTORM_CONTROLLER_SOURCE_SHA256=$sha" \
  --file "$resources/controller-source/Dockerfile.proofstormd" \
  --tag "proofstorm-checkout-source:$sha" "$resources/controller-source"
runner="$CARGO_TARGET_DIR/debug/proofstorm-acceptance"
"$runner" --image-cache-inputs "$gate" > "$work/public/cache-inputs.json"
stage=cache
while IFS=$'\t' read -r image source; do
  digest=${source##*@sha256:}
  cached=$(qualification_cache_ref "$cache" "$source")
  # Staged candidates are already in the cache; copy only the gate's other images.
  if ! jq -e --arg platform "$platform" --arg image "$image" \
      '.[$platform] | any(.[]; . == $image)' "$staged" >/dev/null; then
    GODEBUG=http2client=0 timeout 900 docker buildx imagetools create --prefer-index=false --tag "$cache/images/$digest:prepared" "$source"
  fi
  timeout 30 docker buildx imagetools inspect --raw "$cached" > "$work/manifest.json"
  [[ "$(sha256sum "$work/manifest.json" | cut -d' ' -f1)" == "$digest" ]] || exit 1
done < <(jq -er '.[]|[.image,.source]|@tsv' "$work/public/cache-inputs.json")
stage=managed-gate
"$runner" --root "$checkout" --checkout-home "$checkout/.proofstorm-dev/state" \
  --qualification-image-cache "$cache" --work-dir "$work/run" --timeout 3600 "$gate"
stage=evidence
"$helper" catalog-image stack-evidence "$stack" "$work/public/staged" "$work/run" "$work/public/qualification.json"
stage=complete
