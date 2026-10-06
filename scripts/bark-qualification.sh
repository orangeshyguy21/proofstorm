#!/usr/bin/env bash
# Read-only native qualification of retained Bark candidates. CI checkout only.
set -Eeuo pipefail
umask 077
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
[[ $# == 5 ]] || { echo 'Usage: bark-qualification.sh PLATFORM BUILD_REVISION BUILD_ATTEMPT ARTIFACTS NEW_EXTERNAL_WORK' >&2; exit 2; }
platform=$1 revision=$2 attempt=$3 artifacts=$4
case "$platform:$(uname -s)/$(uname -m)" in
  linux/amd64:Linux/x86_64|linux/arm64:Linux/aarch64) ;;
  *) echo 'Managed qualification requires the matching native Linux runner' >&2; exit 1 ;;
esac
# Staging intentionally changes embedded pins and rebuilds the CLI/controller.
# Refuse to register or replace artifacts in any developer installation.
[[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_ENVIRONMENT:-} == github-hosted && ! -e "$root/.proofstorm-dev" ]] || {
  echo 'Use a fresh GitHub-hosted checkout for candidate qualification' >&2; exit 1;
}
[[ -z $(git -C "$root" status --porcelain) ]] || { echo 'Qualification source must start clean' >&2; exit 1; }
work=$(cd -- "$(dirname -- "$5")" && pwd -P)/$(basename -- "$5")
[[ "$work" != "$root" && "$work" != "$root/"* && ! -e "$work" && ! -L "$work" ]] || exit 2
mkdir "$work" "$work/public"
cd "$root"
stage=restore container='' owner="bark-cache-$(openssl rand -hex 12)"
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
export CARGO_TARGET_DIR="$root/target/check"
cargo build --locked -p proofstorm-xtask
helper="$CARGO_TARGET_DIR/debug/proofstorm-xtask"
"$helper" catalog-image bark-restore "$root" "$platform" "$revision" "$attempt" "$artifacts" "$work/candidates"
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
for repository in cdk-bark-processor bark-server cln-hold; do
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
  target="$cache/bark-candidate/$repository:qualified"
  docker tag "$image" "$target"
  timeout 300 docker push "$target" >/dev/null
  timeout 30 docker buildx imagetools inspect --raw "$target" > "$candidate/manifest.json"
  digest=$(sha256sum "$candidate/manifest.json" | cut -d' ' -f1)
  config=$(jq -er '.config.digest|select(test("^sha256:[a-f0-9]{64}$"))' "$candidate/manifest.json")
  curl -q --noproxy '*' --fail --silent --show-error --max-time 30 \
    "http://$cache/v2/bark-candidate/$repository/blobs/$config" > "$candidate/config.json"
  GODEBUG=http2client=0 timeout 300 docker buildx imagetools create --prefer-index=false \
    --tag "$cache/images/$digest:prepared" "${target%:*}@sha256:$digest"
done
stage=build
"$helper" catalog-image bark-stage "$root" "$platform" "$work/candidates" "$work/public/staged"
cp "$work/public/staged/bark_images.json" "$root/crates/proofstorm-core/src/bark_images.json"
git diff -- crates/proofstorm-core/src/bark_images.json > "$work/public/catalog.patch"
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
"$runner" --image-cache-inputs bark-processor > "$work/public/cache-inputs.json"
stage=cache
while IFS= read -r image; do
  digest=${image##*@sha256:}
  cached=$(qualification_cache_ref "$cache" "$image")
  case "$image" in
    ghcr.io/orangeshyguy21/proofstorm/cdk-bark-processor@*|ghcr.io/orangeshyguy21/proofstorm/bark-server@*|ghcr.io/orangeshyguy21/proofstorm/cln-hold@*) ;;
    *) GODEBUG=http2client=0 timeout 900 docker buildx imagetools create --prefer-index=false --tag "$cache/images/$digest:prepared" "$image" ;;
  esac
  timeout 30 docker buildx imagetools inspect --raw "$cached" > "$work/manifest.json"
  [[ "$(sha256sum "$work/manifest.json" | cut -d' ' -f1)" == "$digest" ]] || exit 1
done < <(jq -er '.[].source' "$work/public/cache-inputs.json")
stage=managed-gate
"$runner" --root "$root" --checkout-home "$root/.proofstorm-dev/state" \
  --qualification-image-cache "$cache" --work-dir "$work/run" --timeout 3600 bark-processor
stage=evidence
"$helper" catalog-image bark-evidence "$work/public/staged" "$work/run" "$work/public/qualification.json"
stage=complete
