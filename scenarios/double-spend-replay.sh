#!/usr/bin/env bash
# SPEC A1 — Replay double-spend.
#
# Attack: redeem the same token (same proofs) twice, serially.
# Oracle:  the mint accepts the first redemption and REJECTS the second;
#          the wallet's spendable balance does not increase on the replay
#          (no value conservation violation → no "stolen" funds).
#
# Fully wallet-driven (cdk-cli); no raw crypto. Runs against MINT=cdk|nutshell.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/attack.sh"

FUND="${ATTACK_FUND:-64}"
SEND="${ATTACK_SEND:-32}"
W0=/root/adv/replay

log "SPEC A1 replay double-spend vs MINT=${MINT} (${MINT_URL})"

adv_reset "${W0}"
adv_fund "${W0}" "${FUND}"

log "send ${SEND} ${UNIT} → token T"
token="$(adv "${W0}" send --mint-url "${MINT_URL}" -a "${SEND}" 2>&1 \
  | grep -oE 'cashu[AB][A-Za-z0-9+/_=-]+' | head -1 || true)"
[[ -n "$token" ]] || die "send produced no token"

log "first redeem of T (must succeed)"
adv "${W0}" receive "${token}" >/dev/null || die "first receive failed unexpectedly"
b1="$(adv_balance_sat "${W0}")"
log "balance after first redeem: ${b1} ${UNIT}"

# The security-critical step: the exact same token must not be redeemable again.
assert_fails "replay redeem of already-spent token T" \
  adv "${W0}" receive "${token}"

b2="$(adv_balance_sat "${W0}")"
assert_eq "no inflation on replay (balance unchanged)" "${b2}" "${b1}"

log "A1 PASSED: ${MINT} mint rejected the replayed proofs; no funds created"
