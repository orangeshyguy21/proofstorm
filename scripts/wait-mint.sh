#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib/wallet.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/wallet.sh"

TIMEOUT_SECS="${1:-90}"
deadline=$((SECONDS + TIMEOUT_SECS))

log "waiting for mint at ${MINT_HOST_URL}/v1/info (timeout ${TIMEOUT_SECS}s)"

while (( SECONDS < deadline )); do
  if curl -fsS "${MINT_HOST_URL}/v1/info" >/dev/null 2>&1; then
    log "mint is ready"
    exit 0
  fi
  # Prefer container health if curl from host fails (port not published yet).
  if docker exec proofstorm-mint wget -q -O - http://127.0.0.1:3338/v1/info >/dev/null 2>&1; then
    log "mint is ready (in-container)"
    exit 0
  fi
  sleep 1
done

die "mint not ready after ${TIMEOUT_SECS}s"
