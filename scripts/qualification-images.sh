#!/usr/bin/env bash
# Called only after Rust validates the complete plan against this checkout.
# Public receipts contain identities/assertions; command output stays private.
set -Eeuo pipefail
umask 077
[[ $# == 2 ]] || exit 2
case_file=$1 work=$2
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
platform=$(jq -er '.platform' "$case_file")
case "$platform:$(uname -s)/$(uname -m)" in
  linux/amd64:Linux/x86_64|linux/arm64:Linux/aarch64) ;;
  *) echo 'A native Linux machine is required' >&2; exit 1 ;;
esac
# An empty configuration deliberately bypasses all ambient registry credentials.
export DOCKER_CONFIG="$work/anonymous-docker"
mkdir "$DOCKER_CONFIG"
printf '{}\n' > "$DOCKER_CONFIG/config.json"
printf '{}\n' > "$work/images.json"
while IFS= read -r source; do
  timeout 90 docker buildx imagetools inspect --raw "$source" > "$work/manifest.json"
  digest=${source##*@}
  if jq -e '.manifests' "$work/manifest.json" >/dev/null; then
    digest=$(jq -er --arg arch "${platform#linux/}" '[.manifests[]|select(.platform.os=="linux" and .platform.architecture==$arch)]|select(length==1)|.[0].digest' "$work/manifest.json")
  fi
  [[ "$digest" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
  selected="${source%@*}@$digest"
  timeout 90 docker buildx imagetools inspect --raw "$selected" > "$work/selected-manifest.json"
  config=$(jq -er '.config.digest' "$work/selected-manifest.json")
  [[ "$config" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
  timeout 300 docker pull --platform "$platform" "$selected" > "$work/pull.log" 2>&1
  timeout 30 docker image inspect "$selected" > "$work/image-inspect.json"
  jq -e --arg arch "${platform#linux/}" --arg config "$config" --arg manifest "$digest" \
    '.[0]|.Os=="linux" and .Architecture==$arch and (.Id==$config or .Id==$manifest or .Descriptor.annotations["config.digest"]==$config)' \
    "$work/image-inspect.json" >/dev/null
  jq --arg source "$source" --arg manifest "$digest" --arg config "$config" \
    '. + {($source):{manifest:$manifest,config:$config}}' "$work/images.json" > "$work/images-next.json"
  mv "$work/images-next.json" "$work/images.json"
done < <(jq -r '[.components[].source]|unique[]' "$case_file")

kind=$(jq -er '.scenario.kind' "$case_file")
if [[ "$kind" == image ]]; then
  implementation=$(jq -er '.scenario.component.implementation' "$case_file")
  version=$(jq -er '.scenario.component.version' "$case_file")
  source=$(jq -er '.scenario.component.source' "$case_file")
  digest=$(jq -er --arg source "$source" '.[$source].manifest' "$work/images.json")
  selected="${source%@*}@$digest"
  case "$implementation" in
    bitcoin-core) probe='bitcoind -nosettings --version | head -n 1'; expected="Bitcoin Core daemon version v$version.0 bitcoind" ;;
    lnd) probe='lnd --version'; expected="lnd version $version commit=v$version" ;;
    cln) probe='lightningd --version'; expected="v$version" ;;
    cdk|cdk-ldk|cdk-bdk) probe='cdk-mint-cli --version && cdk-mintd --version'; expected=$(printf 'cdk-mint-rpc %s\ncdk-mintd %s' "$version" "$version") ;;
    cdk-cli-wallet) probe='cdk-cli --version'; expected="cdk-cli $version" ;;
    nutshell|nutshell-wallet) probe='cashu --help >/dev/null && mint-cli --help >/dev/null && mint --version'; expected="Nutshell, version $version" ;;
    postgresql) probe='postgres --version'; expected="postgres (PostgreSQL) $version" ;;
    redis) probe='redis-server --version'; expected="v=$version" ;;
    keycloak) probe='/opt/keycloak/bin/kc.sh --version'; expected="Keycloak $version" ;;
    workspace) probe='test -x /bin/sh; command -v sha256sum >/dev/null; command -v sleep >/dev/null; echo workspace-commands'; expected=workspace-commands ;;
    ldk-server) probe='ldk-server --version && ldk-server-cli --version'; expected=$(printf 'ldk-server 0.1.0\nldk-server-cli 0.1.0') ;;
    cdk-ldk-server-processor) probe='test -x /usr/local/bin/cdk-payment-processor-ldk-server && cat /usr/local/share/processor-revision'; expected=fe468cad486157683eddbc0df4ff87ba71b6c0a3 ;;
    *) echo 'Missing catalog probe' >&2; exit 1 ;;
  esac
  name="qualification-probe-$(openssl rand -hex 12)"
  cleanup() {
    result=$?
    trap - EXIT
    if docker inspect "$name" >/dev/null 2>&1; then
      owner=$(docker inspect --format '{{index .Config.Labels "dev.proofstorm.qualification"}}' "$name")
      [[ "$owner" == "$name" ]] || exit 1
      timeout 30 docker rm -f "$name" >/dev/null || exit 1
    fi
    exit "$result"
  }
  trap cleanup EXIT
  timeout 120 docker run --rm --name "$name" --label "dev.proofstorm.qualification=$name" \
    --platform "$platform" --network none --read-only --cap-drop ALL \
    --security-opt no-new-privileges --memory 512m --cpus 1 --pids-limit 128 \
    --entrypoint sh "$selected" -ec "$probe" > "$work/probe.stdout" 2> "$work/probe.stderr"
  output=$(cat "$work/probe.stdout")
  case "$implementation" in
    redis) [[ "$output" == "Redis server $expected "* ]] ;;
    keycloak) [[ "$output" == "$expected"* ]] ;;
    *) [[ "$output" == "$expected" ]] ;;
  esac
elif [[ "$kind" == lightning ]]; then
  jq '{bitcoin:[.components[]|select(.implementation=="bitcoin-core")|{version,image:.source}],lightning:[.scenario.component|{implementation,version,image:.source}]}' \
    "$case_file" > "$work/lightning-input.json"
  bash "$root/tests/component-compat/bitcoin-lightning.sh" "$work/lightning-input.json" "$platform" "$work/lightning"
  jq -e '.passed==true and .expected_cases==1 and .completed_cases==1 and all(.cases[]; .passed==true and .cleanup_verified==true)' "$work/lightning/result.json" >/dev/null
fi
