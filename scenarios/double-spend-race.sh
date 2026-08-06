#!/usr/bin/env bash
# SPEC A2 — Concurrent double-spend (distributed race).
#
# Attack: two INDEPENDENT wallet states redeem the same token (same proofs) at
#         the same instant, over the wire. This is the gap the upstream suites
#         cannot cover: their race tests run in one process against one in-memory
#         Ledger; here two separate cdk-cli clients race the deployed mint over
#         HTTP, exercising the real DB locking + request path.
# Oracle:  the mint accepts EXACTLY ONE redemption. Two successes == the mint
#          double-spent (funds created from nothing) == ORACLE VIOLATED.
#
# Uses two work dirs so each receive is a genuinely independent client that
# submits the same input proofs concurrently.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/attack.sh"

FUND="${ATTACK_FUND:-64}"
SEND="${ATTACK_SEND:-32}"
W0=/root/adv/race-src
W1=/root/adv/race-a
W2=/root/adv/race-b

log "SPEC A2 concurrent double-spend vs MINT=${MINT} (${MINT_URL})"

adv_reset "${W0}"; adv_reset "${W1}"; adv_reset "${W2}"
adv_fund "${W0}" "${FUND}"

log "send ${SEND} ${UNIT} → token T"
token="$(adv "${W0}" send --mint-url "${MINT_URL}" -a "${SEND}" 2>&1 \
  | grep -oE 'cashu[AB][A-Za-z0-9+/_=-]+' | head -1 || true)"
[[ -n "$token" ]] || die "send produced no token"

# Fire both redemptions concurrently. Each is a fresh, untrusted wallet, so it
# must be told to accept the token's (untrusted) mint.
rc1="$(mktemp)"; rc2="$(mktemp)"
log "racing two independent redemptions of the SAME proofs"
( adv "${W1}" receive --allow-untrusted "${token}" >/dev/null 2>&1; echo $? > "${rc1}" ) &
p1=$!
( adv "${W2}" receive --allow-untrusted "${token}" >/dev/null 2>&1; echo $? > "${rc2}" ) &
p2=$!
wait "$p1" "$p2"

e1="$(cat "${rc1}")"; e2="$(cat "${rc2}")"
rm -f "${rc1}" "${rc2}"
successes=0
[[ "$e1" == "0" ]] && successes=$((successes + 1))
[[ "$e2" == "0" ]] && successes=$((successes + 1))
log "redeem exit codes: W1=${e1} W2=${e2} → ${successes} succeeded"

if (( successes >= 2 )); then
  b1="$(adv_balance_sat "${W1}")"; b2="$(adv_balance_sat "${W2}")"
  log "W1 balance=${b1} W2 balance=${b2} (both credited from one set of proofs)"
  die "ORACLE VIOLATED: mint double-spent — both concurrent redemptions succeeded"
fi
if (( successes == 0 )); then
  die "INCONCLUSIVE: both redemptions failed (transient/liquidity issue, not a security result)"
fi

pass "exactly one concurrent redemption succeeded"
log "A2 PASSED: ${MINT} mint serialized the race; no funds created"
