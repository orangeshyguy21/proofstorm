#!/usr/bin/env bash
# Release-event deployment and independent public installer verification.
set -euo pipefail

site_version() {
  local tag=$1 channel=$2
  [[ "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-alpha\.(0|[1-9][0-9]*))?$ ]] || return 1
  if [[ "$channel" == alpha ]]; then [[ -n "${BASH_REMATCH[4]}" ]]
  elif [[ "$channel" == stable ]]; then [[ -z "${BASH_REMATCH[4]}" ]]
  else return 1
  fi
}

site_verify_installer() {
  local origin=$1 channel=$2 expected=$3 scratch=$4 observed newest mime bytes digest
  curl -q --fail --silent --proto '=https' --proto-redir '=https' --location --max-time 20 --max-filesize 1048576 \
    -H 'Cache-Control: no-cache' "$origin/release.json?verify=$RANDOM-$RANDOM" -o "$scratch/metadata.json" || return 1
  jq -e --arg channel "$channel" '.repository == "orangeshyguy21/proofstorm" and .channel == $channel' "$scratch/metadata.json" >/dev/null || return 1
  observed=$(jq -er '.tag | select(type == "string")' "$scratch/metadata.json") || return 1
  site_version "$observed" "$channel" || return 1
  newest=$(printf '%s\n' "$expected" "$observed" | LC_ALL=C sort -V | tail -1)
  [[ "$newest" == "$observed" ]] || return 1
  mime=$(curl -q --fail --silent --proto '=https' --proto-redir '=https' --location --max-time 20 --max-filesize 1048576 \
    -H 'Cache-Control: no-cache' "$origin/install?verify=$RANDOM-$RANDOM" -o "$scratch/installer" -w '%{content_type}') || return 1
  [[ "${mime%%;*}" == text/plain ]] || return 1
  bytes=$(wc -c <"$scratch/installer" | tr -d '[:space:]')
  digest=$(sha256sum "$scratch/installer")
  digest=${digest%% *}
  jq -e --argjson bytes "$bytes" --arg digest "$digest" '.installer.bytes == $bytes and .installer.sha256 == $digest' "$scratch/metadata.json" >/dev/null || return 1
  printf 'Website and installer verified at %s\n' "$observed"
}

site_update() (
  local channel=${RELEASE_CHANNEL:?} expected=${EXPECTED_TAG:?} origin=${SITE_URL:?} hook=${DEPLOY_HOOK:?}
  local scratch page count tag newest deadline accepted
  origin=${origin%/}
  site_version "$expected" "$channel" || { echo 'Requested release does not belong to the configured channel.' >&2; return 1; }
  [[ "$origin" =~ ^https://[A-Za-z0-9.-]+(:[0-9]+)?$ ]] || { echo 'Configure SITE_URL as a public HTTPS origin.' >&2; return 1; }
  [[ "$hook" =~ ^https://api\.cloudflare\.com/client/v4/pages/webhooks/deploy_hooks/[A-Za-z0-9_-]+$ ]] || { echo 'Configure a Cloudflare Pages deploy hook.' >&2; return 1; }
  umask 077
  scratch=$(mktemp -d)
  # The function owns its subshell; locals remain available to its exit trap.
  trap 'rm -rf "$scratch"' EXIT
  : >"$scratch/candidates"
  for ((page=1; page<=100; page++)); do
    curl -q --fail --silent --show-error --proto '=https' --max-time 30 --max-filesize 16777216 \
      -H "Authorization: Bearer ${GH_TOKEN:?}" -H 'Accept: application/vnd.github+json' \
      "https://api.github.com/repos/orangeshyguy21/proofstorm/releases?per_page=100&page=$page" -o "$scratch/releases.json" || return 1
    count=$(jq -er 'if type == "array" then length else error("Invalid releases") end' "$scratch/releases.json") || return 1
    jq -r --arg channel "$channel" '.[] | select(.draft == false and (.published_at | type == "string" and length > 0) and .prerelease == ($channel == "alpha")) | .tag_name | select(type == "string")' "$scratch/releases.json" >"$scratch/page-tags" || return 1
    while IFS= read -r tag; do
      if site_version "$tag" "$channel"; then printf '%s\n' "$tag" >>"$scratch/candidates"; fi
    done <"$scratch/page-tags"
    if ((count<100)); then break; fi
  done
  ((page<=100)) || { echo 'Release pagination limit exceeded.' >&2; return 1; }
  grep -Fxq -- "$expected" "$scratch/candidates" || { echo 'Requested release is not published in this channel.' >&2; return 1; }
  newest=$(LC_ALL=C sort -V "$scratch/candidates" | tail -1)
  if [[ "$newest" != "$expected" ]]; then
    echo 'A newer release exists; this older event will not request a deployment.'
    return 0
  fi
  # Never report the secret hook URL through a transport error.
  accepted=$(curl -q --silent --proto '=https' --max-time 30 -X POST --data '' "$hook" -o /dev/null -w '%{http_code}' 2>/dev/null) || accepted=failed
  case "$accepted" in
    200|201|202) ;;
    *) echo 'Cloudflare did not accept the deploy hook. Check the saved secret and Pages project.' >&2; return 1 ;;
  esac
  echo 'Build requested. Waiting for a verified release in the same channel.'
  deadline=$((SECONDS+600))
  while ((SECONDS<deadline)); do
    if site_verify_installer "$origin" "$channel" "$expected" "$scratch"; then return 0; fi
    sleep 5
  done
  echo 'Website did not reach the expected verified release within 10 minutes. Check Pages deployment logs.' >&2
  return 1
)

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then site_update; fi
