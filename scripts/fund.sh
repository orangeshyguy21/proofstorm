#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib/wallet.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/wallet.sh"

require_n_wallets

log "funding ${N_WALLETS} wallet(s) with ${FUND_AMOUNT} ${UNIT} each (impl=${WALLET_IMPL})"

failed=0
for ((i = 1; i <= N_WALLETS; i++)); do
  log "wallet-${i}: mint ${FUND_AMOUNT}"
  if wallet_mint "$i" "${FUND_AMOUNT}"; then
    log "wallet-${i}: funded"
  else
    log "wallet-${i}: fund FAILED"
    failed=$((failed + 1))
  fi
done

if (( failed > 0 )); then
  die "${failed} wallet(s) failed to fund"
fi

log "funding complete"
