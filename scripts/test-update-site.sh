#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
source "$root/scripts/update-site.sh"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/result"
test_fixture=$scratch
printf '#!/bin/sh\nprintf install\n' >"$scratch/expected-installer"
if ! command -v sha256sum >/dev/null; then
  sha256sum() { shasum -a 256 "$@"; }
fi
digest=$(sha256sum "$scratch/expected-installer")
digest=${digest%% *}
bytes=$(wc -c <"$scratch/expected-installer" | tr -d '[:space:]')
test_mime=text/plain
metadata() {
  jq -n --arg tag "$1" --arg digest "$digest" --argjson bytes "$bytes" \
    '{repository:"orangeshyguy21/proofstorm",channel:"alpha",tag:$tag,installer:{bytes:$bytes,sha256:$digest}}' >"$scratch/expected-metadata"
}
# Model the two public downloads without sending any deployment request.
curl() {
  local destination='' url='' value
  while (($#)); do
    value=$1
    case "$value" in
      -o) destination=$2; shift 2 ;;
      https://*) url=$value; shift ;;
      *) shift ;;
    esac
  done
  case "$url" in
    https://site.example/release.json\?*) cp "$test_fixture/expected-metadata" "$destination" ;;
    https://site.example/install\?*) cp "$test_fixture/expected-installer" "$destination"; printf '%s' "$test_mime" ;;
    'https://api.github.com/repos/orangeshyguy21/proofstorm/releases?per_page=100&page=1') cp "$test_fixture/releases" "$destination" ;;
    https://api.cloudflare.com/client/v4/pages/webhooks/deploy_hooks/test-only)
      printf 'deployment\n' >>"$test_fixture/deployments"; printf 202 ;;
    *) echo 'Unexpected test request' >&2; return 1 ;;
  esac
}
for tag in v0.1.0-alpha.0 v1.10.3-alpha.12; do site_version "$tag" alpha; done
site_version v1.0.0 stable
for tag in v01.0.0-alpha.1 v1.0.0-alpha.01 v1.0.0 v1.0.0-beta.1 'v1.0.0-alpha.1;echo'; do
  if site_version "$tag" alpha; then echo 'Invalid channel/version accepted' >&2; exit 1; fi
done
metadata v1.2.0-alpha.10
site_verify_installer https://site.example alpha v1.2.0-alpha.9 "$scratch/result"
if site_verify_installer https://site.example alpha v1.2.0-alpha.11 "$scratch/result"; then exit 1; fi
metadata v1.10.0-alpha.1
site_verify_installer https://site.example alpha v1.2.0-alpha.10 "$scratch/result"
test_mime=text/html
if site_verify_installer https://site.example alpha v1.2.0-alpha.10 "$scratch/result"; then exit 1; fi
test_mime=text/plain
printf changed >>"$scratch/expected-installer"
if site_verify_installer https://site.example alpha v1.2.0-alpha.10 "$scratch/result"; then exit 1; fi
printf '#!/bin/sh\nprintf install\n' >"$scratch/expected-installer"
export RELEASE_CHANNEL=alpha EXPECTED_TAG=v1.2.0-alpha.9 SITE_URL=https://site.example
export DEPLOY_HOOK=https://api.cloudflare.com/client/v4/pages/webhooks/deploy_hooks/test-only GH_TOKEN=test-only
jq -n '["v1.2.0-alpha.9", "v1.2.0-alpha.10"] | map({tag_name:.,draft:false,prerelease:true,published_at:"2026-09-13"})' >"$scratch/releases"
site_update
[[ ! -e "$scratch/deployments" ]]
EXPECTED_TAG=v1.2.0-alpha.8
if site_update >/dev/null 2>&1; then echo 'Missing release accepted' >&2; exit 1; fi
[[ ! -e "$scratch/deployments" ]]
EXPECTED_TAG=v1.2.0-alpha.10
metadata "$EXPECTED_TAG"
site_update
[[ $(cat "$scratch/deployments") == deployment ]]
printf '{"error":"invalid release page"}' >"$scratch/releases"
if site_update >/dev/null 2>&1; then echo 'Invalid release page accepted' >&2; exit 1; fi
[[ $(cat "$scratch/deployments") == deployment ]]
printf 'Website release ordering and installer integrity checks passed\n'
