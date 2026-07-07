#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib/wallet.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/wallet.sh"

require_n_wallets

INTERVAL="${WATCH_INTERVAL:-2}"
ONCE="${WATCH_ONCE:-0}"

bar() {
  # bar <value> <max> <width>
  local value="$1" max="$2" width="$3" filled i out=""
  if (( max <= 0 )); then max=1; fi
  filled=$(( value * width / max ))
  (( filled > width )) && filled=$width
  (( filled < 0 )) && filled=0
  for ((i = 0; i < filled; i++)); do out+="#"; done
  for ((i = filled; i < width; i++)); do out+="."; done
  printf '%s' "$out"
}

render() {
  local expected total=0 i amount status container state
  expected="$(conservation_expected_total)"

  # Find a sensible bar scale: the larger of FUND_AMOUNT and current max balance.
  local max="$FUND_AMOUNT"

  # Collect balances first (so the table renders in one pass).
  local -a bal=() st=()
  for ((i = 1; i <= N_WALLETS; i++)); do
    container="$(wallet_container "$i")"
    state="$(docker inspect -f '{{.State.Status}}' "$container" 2>/dev/null || echo "down")"
    if [[ "$state" != "running" ]]; then
      bal[i]=-1
      st[i]="$state"
      continue
    fi
    if amount="$(wallet_balance_sat "$i" 2>/dev/null)"; then
      bal[i]="$amount"
      st[i]="up"
      total=$((total + amount))
      (( amount > max )) && max=$amount
    else
      bal[i]=-1
      st[i]="err"
    fi
  done

  clear 2>/dev/null || printf '\033[2J\033[H'
  printf 'proofstorm  —  live population  (impl=%s  unit=%s  N=%s)\n' \
    "$WALLET_IMPL" "$UNIT" "$N_WALLETS"
  printf 'mint: %s   refresh: %ss   %s\n\n' \
    "$MINT_HOST_URL" "$INTERVAL" "$(date '+%H:%M:%S')"

  printf '%-10s %8s  %-*s %s\n' "WALLET" "BAL" 24 "BALANCE" "STATE"
  printf '%-10s %8s  %-*s %s\n' "------" "---" 24 "-------" "-----"
  for ((i = 1; i <= N_WALLETS; i++)); do
    if (( bal[i] < 0 )); then
      printf '%-10s %8s  %-*s %s\n' "wallet-$i" "-" 24 "$(bar 0 "$max" 24)" "${st[i]}"
    else
      printf '%-10s %8s  %-*s %s\n' \
        "wallet-$i" "${bal[i]}" 24 "$(bar "${bal[i]}" "$max" 24)" "${st[i]}"
    fi
  done

  # Conservation summary line.
  local delta status_word
  delta=$((total - expected))
  if (( delta == 0 )); then
    status_word="OK (conserved)"
  elif (( delta < 0 )); then
    status_word="LOSS $((-delta)) ${UNIT}"
  else
    status_word="INFLATION +${delta} ${UNIT}"
  fi
  printf '\n%-10s %8s  expected %s   %s\n' "TOTAL" "$total" "$expected" "$status_word"

  if (( ONCE == 0 )); then
    printf '\n(ctrl-c to exit)\n'
  fi
}

if (( ONCE == 1 )); then
  render
  exit 0
fi

while true; do
  render
  sleep "$INTERVAL"
done
