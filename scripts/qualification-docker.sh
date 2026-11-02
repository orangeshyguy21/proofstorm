#!/usr/bin/env bash
# Sourced by qualification tooling. Keep the selected local engine and Buildx,
# but never inherit registry logins or auto-discover a desktop credential helper.
qualification_docker_config() {
  local directory=$1 endpoint plugin
  endpoint=$(docker context inspect --format '{{json .Endpoints.docker}}' | jq -er '.Host')
  [[ "$endpoint" == unix:///* ]] || { echo 'Qualification requires a local Docker socket' >&2; return 1; }
  plugin=$(docker info --format '{{json .ClientInfo.Plugins}}' | jq -er '.[]|select(.Name=="buildx")|.Path')
  [[ "$plugin" == /* && -f "$plugin" ]] || return 1
  mkdir -p "$directory/cli-plugins"
  chmod 700 "$directory" "$directory/cli-plugins"
  ln -s "$plugin" "$directory/cli-plugins/docker-buildx"
  printf '{"auths":{"127.0.0.1":{}}}\n' > "$directory/config.json"
  chmod 600 "$directory/config.json"
  export DOCKER_HOST="$endpoint" DOCKER_CONFIG="$directory"
  unset DOCKER_CONTEXT DOCKER_AUTH_CONFIG DOCKER_TLS DOCKER_TLS_VERIFY DOCKER_CERT_PATH DOCKER_CUSTOM_HEADERS BUILDX_CONFIG BUILDX_BUILDER
}

qualification_cache_endpoint() {
  [[ "$1" =~ ^127\.0\.0\.1:([1-9][0-9]{0,4})$ ]] && (( 10#${BASH_REMATCH[1]} <= 65535 ))
}

# Whether ENGINE, docker info's OSType/Architecture, runs PLATFORM natively.
qualification_native_engine() {
  case "$1:$2" in
    linux/amd64:linux/x86_64|linux/amd64:linux/amd64|linux/arm64:linux/aarch64|linux/arm64:linux/arm64) ;;
    *) return 1 ;;
  esac
}

# Start the created registry CONTAINER and print its single loopback endpoint
# once it serves /v2/.
qualification_registry_start() {
  local container=$1 port endpoint
  docker start "$container" >/dev/null || return
  if ! port=$(docker inspect --format '{{json .NetworkSettings.Ports}}' "$container" | jq -er '.["5000/tcp"]|select(length==1 and .[0].HostIp=="127.0.0.1")|.[0].HostPort'); then
    docker logs "$container" >&2
    echo 'Cache registry did not expose its single loopback port' >&2
    return 1
  fi
  endpoint="127.0.0.1:$port"
  qualification_cache_endpoint "$endpoint" || return
  for _ in {1..30}; do
    if curl -q --noproxy '*' --fail --silent --max-time 2 "http://$endpoint/v2/" >/dev/null; then
      echo "$endpoint"
      return
    fi
    sleep 1
  done
  return 1
}

qualification_cache_ref() {
  local endpoint=$1 source=$2 sha=${2##*@sha256:}
  qualification_cache_endpoint "$endpoint" || return 1
  [[ "$source" == *@sha256:* && "$sha" =~ ^[a-f0-9]{64}$ ]] || return 1
  printf '%s/images/%s@sha256:%s\n' "$endpoint" "$sha" "$sha"
}
