#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib/wallet.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/wallet.sh"

require_n_wallets

log "balances for ${N_WALLETS} wallet(s)"

for ((i = 1; i <= N_WALLETS; i++)); do
  printf '\n=== wallet-%s (%s) ===\n' "$i" "$(wallet_container "$i")"
  wallet_balance "$i" || log "wallet-${i}: balance FAILED"
done

printf '\n'
