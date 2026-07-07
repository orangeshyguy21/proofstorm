#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib/wallet.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/wallet.sh"

require_n_wallets

log "smoke: fund ${N_WALLETS} wallet(s), self-swap ${SWAP_AMOUNT} ${UNIT}, print balances"

failed=0
for ((i = 1; i <= N_WALLETS; i++)); do
  log "wallet-${i}: mint ${FUND_AMOUNT}"
  if ! wallet_mint "$i" "${FUND_AMOUNT}"; then
    log "wallet-${i}: mint FAILED"
    failed=$((failed + 1))
  fi
done

if (( failed > 0 )); then
  die "smoke failed: ${failed} wallet(s) could not fund"
fi

conservation_check "$(conservation_expected_total)" "after fund"

fund_snapshot="$(wallet_population_total_sat)"
log "post-fund population total: ${fund_snapshot} ${UNIT}"

for ((i = 1; i <= N_WALLETS; i++)); do
  log "wallet-${i}: self-swap ${SWAP_AMOUNT}"
  if ! wallet_self_swap "$i" "${SWAP_AMOUNT}"; then
    log "wallet-${i}: self-swap FAILED"
    failed=$((failed + 1))
    continue
  fi
  log "wallet-${i}: ok"
done

printf '\n'
"${PROOFSTORM_ROOT}/scripts/balances.sh"

if (( failed > 0 )); then
  die "smoke failed for ${failed} wallet(s)"
fi

if (( SWAP_AMOUNT > 0 )); then
  conservation_check_after_swap "${fund_snapshot}"
else
  conservation_check "${fund_snapshot}" "after smoke"
fi

log "smoke passed"
