#!/usr/bin/env bash
# Copy public immutable image content once; restore it into a read-only,
# job-owned registry. No runtime state, credentials or public package publishing.
set -Eeuo pipefail
umask 077
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
# shellcheck source=scripts/qualification-docker.sh
source "$root/scripts/qualification-docker.sh"
qualifier="$root/target/check/debug/proofstorm-qualification"
mode=${1:-}; shift || exit 2

if [[ "$mode" == seed ]]; then
  [[ $# == 4 ]] || exit 2
  cache=$1 destination=$2 inputs=$3 work=$4
  qualification_cache_endpoint "$cache" && qualification_cache_endpoint "$destination"
  jq -e 'type=="array" and length>0 and all(.[]; (.image|type=="string") and (.source|type=="string"))' "$inputs" >/dev/null
  qualification_docker_config "$work/seed-docker"
  while IFS=$'\t' read -r image source; do
    [[ "$image" == proofstorm-registry.localhost:5000/*@sha256:* ]] || exit 1
    local_ref=${image#proofstorm-registry.localhost:5000/}
    repository=${local_ref%@*} sha=${local_ref##*@sha256:}
    [[ "$repository" =~ ^[a-z0-9][a-z0-9._/-]*$ && "$repository" != *..* && "$sha" =~ ^[a-f0-9]{64}$ ]] || exit 1
    [[ "$source" == *@sha256:"$sha" ]] || exit 1
    cached=$(qualification_cache_ref "$cache" "$source")
    # A missing cache entry fails here. There is deliberately no upstream retry.
    GODEBUG="${GODEBUG:+$GODEBUG,}http2client=0" timeout 300 docker buildx imagetools create --prefer-index=false \
      --tag "$destination/$repository:catalog-$sha" "$cached"
    observed=$(timeout 30 docker buildx imagetools inspect "$destination/$local_ref" --format '{{json .Manifest}}' | jq -er '.digest')
    [[ "$observed" == "sha256:$sha" ]] || exit 1
  done < <(jq -er '.[]|[.image,.source]|@tsv' "$inputs")
  exit 0
fi

[[ "$mode" == prepare && $# == 3 || "$mode" == with && $# -ge 4 ]] || exit 2
plan=$1 bundle=$2 work=$3; shift 3
[[ "$bundle" == /* && "$work" == /* && ! -e "$work" ]] || exit 2
mkdir -p "$work"
"$qualifier" images "$plan" > "$work/inputs.json"
qualification_docker_config "$work/docker"
owner="qualification-cache-$(openssl rand -hex 12)"
container=''
cleanup() {
  result=$?
  trap - EXIT
  if [[ -n "$container" ]]; then
    actual=$(docker inspect --format '{{index .Config.Labels "dev.proofstorm.qualification-cache"}}' "$container") || exit 1
    [[ "$actual" == "$owner" ]] || exit 1
    timeout 30 docker rm --force --volumes "$container" >/dev/null || exit 1
  fi
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

readonly=false
storage_args=(--read-only)
if [[ "$mode" == with ]]; then
  (cd "$bundle" && sha256sum --check --quiet SHA256SUMS)
  cmp "$bundle/inputs.json" "$work/inputs.json"
  [[ -d "$bundle/registry" && ! -L "$bundle/registry" ]] || exit 1
  readonly=true
  storage_args+=(--mount "type=bind,source=$bundle/registry,target=/var/lib/registry,readonly")
else
  [[ ! -e "$bundle" ]] || exit 2
  mkdir "$bundle"
  timeout 300 docker pull registry:2
fi
jq -n --argjson readonly "$readonly" '{version:0.1,log:{level:"error"},storage:{filesystem:{rootdirectory:"/var/lib/registry"},delete:{enabled:false},maintenance:{uploadpurging:{enabled:false},readonly:{enabled:$readonly}}},http:{addr:":5000"}}' > "$work/registry.json"
# registry:2 is supplied by the verified native build artifact, never pulled by
# consumers. Restores bind the immutable artifact directly, avoiding another
# multi-gigabyte copy per job. Preparation's anonymous volume is removed by ID.
container=$(docker create --pull=never --name "$owner" \
  --label "dev.proofstorm.qualification-cache=$owner" --publish 127.0.0.1::5000 \
  --tmpfs /tmp --mount "type=bind,source=$work/registry.json,target=/etc/docker/registry/config.yml,readonly" \
  "${storage_args[@]}" registry:2)
docker start "$container" >/dev/null
if ! port=$(docker inspect --format '{{json .NetworkSettings.Ports}}' "$container" | jq -er '.["5000/tcp"]|select(length==1 and .[0].HostIp=="127.0.0.1")|.[0].HostPort'); then
  docker logs "$container" >&2
  echo 'Cache registry did not expose its single loopback port' >&2
  exit 1
fi
cache="127.0.0.1:$port"
qualification_cache_endpoint "$cache"
ready=false
for _ in {1..30}; do
  if curl -q --noproxy '*' --fail --silent --max-time 2 "http://$cache/v2/" >/dev/null; then ready=true; break; fi
  sleep 1
done
[[ "$ready" == true ]] || exit 1

# Verify native manifests, configs and layer availability, as well as the full
# index digest. Buildx copies all index children, retaining every original pin.
while IFS= read -r source; do
  sha=${source##*@sha256:}
  cached=$(qualification_cache_ref "$cache" "$source")
  if [[ "$mode" == prepare ]]; then
    echo "Preparing $source"
    GODEBUG="${GODEBUG:+$GODEBUG,}http2client=0" timeout 900 docker buildx imagetools create --prefer-index=false --tag "$cache/images/$sha:prepared" "$source"
  fi
  timeout 30 docker buildx imagetools inspect --raw "$cached" > "$work/index.json"
  [[ "$(sha256sum "$work/index.json" | cut -d' ' -f1)" == "$sha" ]] || exit 1
  while IFS= read -r platform; do
    digest="sha256:$sha"
    if jq -e '.manifests' "$work/index.json" >/dev/null; then
      digest=$(jq -er --arg arch "${platform#linux/}" '[.manifests[]|select(.platform.os=="linux" and .platform.architecture==$arch)]|select(length==1)|.[0].digest' "$work/index.json")
    fi
    [[ "$digest" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
    timeout 30 docker buildx imagetools inspect --raw "${cached%@*}@$digest" > "$work/manifest.json"
    [[ "sha256:$(sha256sum "$work/manifest.json" | cut -d' ' -f1)" == "$digest" ]] || exit 1
    config=$(jq -er '.config.digest' "$work/manifest.json")
    [[ "$config" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
    curl -q --noproxy '*' --fail --silent --show-error --max-time 30 "http://$cache/v2/images/$sha/blobs/$config" > "$work/config.json"
    [[ "sha256:$(sha256sum "$work/config.json" | cut -d' ' -f1)" == "$config" ]] || exit 1
    jq -e --arg arch "${platform#linux/}" '.os=="linux" and .architecture==$arch' "$work/config.json" >/dev/null
    while IFS= read -r layer; do
      [[ "$layer" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
      curl -q --noproxy '*' --fail --silent --show-error --head --max-time 30 "http://$cache/v2/images/$sha/blobs/$layer" >/dev/null
    done < <(jq -er '.layers[].digest' "$work/manifest.json")
  done < <(jq -er --arg source "$source" '.images[$source][]' "$work/inputs.json")
done < <(jq -er '.images|keys[]' "$work/inputs.json")

if [[ "$mode" == prepare ]]; then
  docker stop "$container" >/dev/null
  docker cp "$container:/var/lib/registry" "$bundle/registry"
  cp "$work/inputs.json" "$bundle/inputs.json"
  (cd "$bundle" && find registry -type f -print | LC_ALL=C sort | xargs sha256sum > SHA256SUMS && sha256sum inputs.json >> SHA256SUMS)
else
  # This is an explicit scope around the command, not a product registry override.
  export QUALIFICATION_IMAGE_CACHE="$cache"
  "$@"
fi
