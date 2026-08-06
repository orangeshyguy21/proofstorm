#!/usr/bin/env bash
# SPEC D1 — Mint-quote flood (denial-of-service).
#
# Attack: create mint quotes (POST /v1/mint/quote/bolt11) as fast as possible.
#         Each quote makes the mint mint an LN invoice via its backend — an
#         unbounded, unauthenticated resource-creation vector.
# Oracle: an HONEST client keeps getting served. A concurrent liveness prober
#         hits /v1/info throughout the flood; the mint must answer within the
#         SLA the whole time. Rejecting the flood but starving honest clients
#         still counts as a DoS (oracle violated).
#
# Runs from the host against the published mint port. MINT=cdk|nutshell.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/attack.sh"

FLOOD_COUNT="${FLOOD_COUNT:-500}"
FLOOD_CONCURRENCY="${FLOOD_CONCURRENCY:-32}"
PROBE_TIMEOUT="${PROBE_TIMEOUT:-2}"      # honest /v1/info must answer within Ns
PROBE_INTERVAL="${PROBE_INTERVAL:-0.5}"
LIVENESS_MAX_FAILS="${LIVENESS_MAX_FAILS:-0}"

log "SPEC D1 mint-quote flood vs MINT=${MINT} (${MINT_HOST_URL})"
log "flood: ${FLOOD_COUNT} quotes @ concurrency ${FLOOD_CONCURRENCY}"

curl -fsS "${MINT_HOST_URL}/v1/info" >/dev/null 2>&1 \
  || die "mint not reachable at ${MINT_HOST_URL} — is the regtest stack up?"

# ---- concurrent honest-client liveness prober ----
probe_fails="$(mktemp)"; echo 0 > "${probe_fails}"
stop_probe="$(mktemp)"
(
  while [[ ! -f "${stop_probe}" ]]; do
    if ! curl -fsS --max-time "${PROBE_TIMEOUT}" "${MINT_HOST_URL}/v1/info" >/dev/null 2>&1; then
      n="$(cat "${probe_fails}")"; echo $((n + 1)) > "${probe_fails}"
    fi
    sleep "${PROBE_INTERVAL}"
  done
) &
probe_pid=$!

# ---- the flood ----
flood_one() {
  curl -fsS --max-time 5 -X POST \
    -H 'content-type: application/json' \
    -d '{"amount":1,"unit":"sat"}' \
    "${MINT_HOST_URL}/v1/mint/quote/bolt11" >/dev/null 2>&1 || true
}
export -f flood_one
export MINT_HOST_URL

start="$(date +%s)"
seq "${FLOOD_COUNT}" | xargs -P "${FLOOD_CONCURRENCY}" -I{} bash -c 'flood_one'
elapsed=$(( $(date +%s) - start ))
log "flood done in ${elapsed}s"

# ---- stop prober, evaluate oracle ----
touch "${stop_probe}"
wait "${probe_pid}" 2>/dev/null || true
fails="$(cat "${probe_fails}")"
rm -f "${probe_fails}" "${stop_probe}"

log "honest-client liveness failures during flood: ${fails} (max allowed ${LIVENESS_MAX_FAILS})"
if (( fails > LIVENESS_MAX_FAILS )); then
  die "ORACLE VIOLATED: honest /v1/info was starved ${fails} time(s) during the flood"
fi

curl -fsS --max-time "${PROBE_TIMEOUT}" "${MINT_HOST_URL}/v1/info" >/dev/null 2>&1 \
  || die "ORACLE VIOLATED: mint not responsive after flood"

pass "mint stayed live for honest clients throughout and after the flood"
log "D1 PASSED: ${MINT} mint withstood the quote flood"
