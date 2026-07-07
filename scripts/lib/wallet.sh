# Shared helpers for proofstorm host scripts.
# Source from scripts that live under scripts/.

PROOFSTORM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Preserve caller overrides (e.g. `WALLET_IMPL=nutshell make smoke`) before
# sourcing `.env`, which would otherwise reset them to WALLET_IMPL=cdk.
_proofstorm_saved_env=()
for _var in N_WALLETS FUND_AMOUNT WALLET_IMPL MINT_IMPL UNIT MINT_HOST_PORT \
  SWAP_AMOUNT CONSERVATION_EXPECTED CONSERVATION_TOLERANCE; do
  if [[ -n "${!_var+x}" ]]; then
    _proofstorm_saved_env+=("${_var}=${!_var}")
  fi
done

if [[ -f "${PROOFSTORM_ROOT}/.env" ]]; then
  # shellcheck disable=SC1091
  set -a
  source "${PROOFSTORM_ROOT}/.env"
  set +a
fi

for _entry in "${_proofstorm_saved_env[@]}"; do
  export "${_entry?}"
done
unset _var _entry _proofstorm_saved_env

N_WALLETS="${N_WALLETS:-3}"
FUND_AMOUNT="${FUND_AMOUNT:-100}"
WALLET_IMPL="${WALLET_IMPL:-cdk}"
UNIT="${UNIT:-sat}"
MINT_URL="${MINT_URL:-http://mint:3338}"
MINT_HOST_PORT="${MINT_HOST_PORT:-3338}"
MINT_HOST_URL="${MINT_HOST_URL:-http://127.0.0.1:${MINT_HOST_PORT}}"
SWAP_AMOUNT="${SWAP_AMOUNT:-1}"

# A running stack records its resolved impl/size at `make up`. That file is the
# source of truth for driving commands (fund/balances/smoke/check/watch), so you
# can't accidentally drive a nutshell stack as cdk. Set PROOFSTORM_USE_STATE=0
# to bypass. To switch impl: `make down` then `make up` with the new WALLET_IMPL.
PROOFSTORM_STATE_FILE="${PROOFSTORM_ROOT}/.proofstorm-active"
if [[ "${PROOFSTORM_USE_STATE:-1}" == "1" && -f "${PROOFSTORM_STATE_FILE}" ]]; then
  _req_impl="${WALLET_IMPL}"
  _active_impl=""
  _active_n=""
  while IFS='=' read -r _k _v; do
    case "${_k}" in
      WALLET_IMPL) _active_impl="${_v}" ;;
      N_WALLETS) _active_n="${_v}" ;;
    esac
  done < "${PROOFSTORM_STATE_FILE}"
  if [[ -n "${_active_impl}" ]]; then
    if [[ "${_req_impl}" != "${_active_impl}" ]]; then
      printf '[proofstorm] note: running stack is %s; using it (requested %s). down+up to switch.\n' \
        "${_active_impl}" "${_req_impl}" >&2
    fi
    WALLET_IMPL="${_active_impl}"
  fi
  [[ -n "${_active_n}" ]] && N_WALLETS="${_active_n}"
  unset _req_impl _active_impl _active_n _k _v
fi

MAX_WALLETS=10

log() {
  printf '[proofstorm] %s\n' "$*"
}

die() {
  printf '[proofstorm] error: %s\n' "$*" >&2
  exit 1
}

require_n_wallets() {
  if ! [[ "${N_WALLETS}" =~ ^[0-9]+$ ]] || (( N_WALLETS < 1 || N_WALLETS > MAX_WALLETS )); then
    die "N_WALLETS must be 1..${MAX_WALLETS} (got ${N_WALLETS})"
  fi
}

wallet_services() {
  require_n_wallets
  local i
  for ((i = 1; i <= N_WALLETS; i++)); do
    printf 'wallet-%s ' "$i"
  done
}

wallet_container() {
  local id="$1"
  printf 'proofstorm-wallet-%s' "$id"
}

nutshell_wallet_name() {
  local id="$1"
  printf 'wallet-%s' "$id"
}

# Run a wallet CLI command inside wallet container $id.
# Usage: wallet_exec <id> <cli-args...>
wallet_exec() {
  local id="$1"
  shift
  local container
  container="$(wallet_container "$id")"

  case "${WALLET_IMPL}" in
    cdk)
      docker exec -i "${container}" cdk-cli --unit "${UNIT}" "$@"
      ;;
    nutshell)
      docker exec -i "${container}" cashu \
        --host "${MINT_URL}" \
        --unit "${UNIT}" \
        --wallet "$(nutshell_wallet_name "$id")" \
        --tests \
        "$@"
      ;;
    *)
      die "unknown WALLET_IMPL=${WALLET_IMPL}"
      ;;
  esac
}

# Mint amount into wallet $id (FakeWallet auto-settles).
wallet_mint() {
  local id="$1"
  local amount="${2:-$FUND_AMOUNT}"
  case "${WALLET_IMPL}" in
    cdk)
      wallet_exec "$id" mint "${MINT_URL}" "${amount}"
      ;;
    nutshell)
      wallet_exec "$id" invoice "${amount}"
      ;;
    *)
      die "wallet_mint: unsupported WALLET_IMPL=${WALLET_IMPL}"
      ;;
  esac
}

wallet_balance() {
  local id="$1"
  case "${WALLET_IMPL}" in
    cdk)
      wallet_exec "$id" balance
      ;;
    nutshell)
      # balance does not need --tests; wallet_exec always passes it anyway.
      wallet_exec "$id" balance
      ;;
    *)
      die "wallet_balance: unsupported WALLET_IMPL=${WALLET_IMPL}"
      ;;
  esac
}

extract_token() {
  grep -oE 'cashu[AB][A-Za-z0-9+/_=-]+' | head -1 || true
}

# Self-send swap: send amount and receive the token back (exercises swap path).
wallet_self_swap() {
  local id="$1"
  local amount="${2:-$SWAP_AMOUNT}"
  local out token
  case "${WALLET_IMPL}" in
    cdk)
      out="$(wallet_exec "$id" send --mint-url "${MINT_URL}" -a "${amount}" 2>&1)" || return 1
      token="$(printf '%s\n' "$out" | extract_token)"
      [[ -n "$token" ]] || {
        printf '%s\n' "$out" >&2
        return 1
      }
      wallet_exec "$id" receive "$token"
      ;;
    nutshell)
      # Offline send avoids an extra mint swap on send; receive still swaps in.
      out="$(wallet_exec "$id" send "${amount}" --offline 2>&1)" || return 1
      token="$(printf '%s\n' "$out" | extract_token)"
      [[ -n "$token" ]] || {
        printf '%s\n' "$out" >&2
        return 1
      }
      wallet_exec "$id" receive "$token"
      ;;
    *)
      die "wallet_self_swap: unsupported WALLET_IMPL=${WALLET_IMPL}"
      ;;
  esac
}

# Return spendable balance (integer sats) for wallet $id on stdout.
# A successful command with no parseable amount means an empty wallet (0).
# cdk-cli prints nothing for a fresh wallet; nutshell always prints "Balance: 0 sat".
wallet_balance_sat() {
  local id="$1"
  local out amount
  out="$(wallet_balance "$id" 2>&1)" || return 1

  case "${WALLET_IMPL}" in
    cdk)
      amount="$(printf '%s\n' "$out" \
        | grep -E '^Total balance across all wallets:' \
        | grep -oE '[0-9]+' \
        | head -1 || true)"
      ;;
    nutshell)
      amount="$(printf '%s\n' "$out" \
        | grep -E '^Balance:' \
        | awk '{print $2}' \
        | head -1 || true)"
      ;;
    *)
      die "wallet_balance_sat: unsupported WALLET_IMPL=${WALLET_IMPL}"
      ;;
  esac

  # Empty/unparseable output on success = 0-balance wallet.
  [[ -n "$amount" ]] || amount=0
  printf '%s\n' "$amount"
}

# Sum spendable balances across wallet-1..N; prints total on stdout.
wallet_population_total_sat() {
  require_n_wallets
  local i amount total=0
  for ((i = 1; i <= N_WALLETS; i++)); do
    amount="$(wallet_balance_sat "$i")" || return 1
    total=$((total + amount))
  done
  printf '%s\n' "$total"
}

conservation_expected_total() {
  if [[ -n "${CONSERVATION_EXPECTED:-}" ]]; then
    printf '%s\n' "${CONSERVATION_EXPECTED}"
    return
  fi
  printf '%s\n' "$((N_WALLETS * FUND_AMOUNT))"
}

# Max allowed sat loss across the population after self-swap (vs post-fund snapshot).
# Nutshell may burn ~1 sat per wallet on receive swap with default proof sizes.
conservation_swap_tolerance() {
  if [[ -n "${CONSERVATION_SWAP_TOLERANCE:-}" ]]; then
    printf '%s\n' "${CONSERVATION_SWAP_TOLERANCE}"
    return
  fi
  if (( SWAP_AMOUNT == 0 )); then
    printf '0\n'
    return
  fi
  case "${WALLET_IMPL}" in
    nutshell) printf '%s\n' "${N_WALLETS}" ;;
    *) printf '0\n' ;;
  esac
}

# Args: expected_total [label]
conservation_check() {
  local expected="${1:-$(conservation_expected_total)}"
  local label="${2:-population}"
  local actual tolerance delta

  actual="$(wallet_population_total_sat)"
  tolerance="${CONSERVATION_TOLERANCE:-0}"

  log "conservation check: ${label} (impl=${WALLET_IMPL}, unit=${UNIT})"
  log "  wallets: ${N_WALLETS}"
  log "  expected total: ${expected} ${UNIT}"
  log "  actual total:   ${actual} ${UNIT}"
  log "  tolerance:      ${tolerance} ${UNIT}"

  delta=$((actual - expected))
  if (( delta < 0 )); then
    delta=$((-delta))
  fi

  if (( delta > tolerance )); then
    die "conservation violated (${label}): expected ${expected}, got ${actual} (delta ${delta})"
  fi

  log "conservation ok (${label})"
}

# After self-swaps: no inflation; loss within swap tolerance vs post-fund snapshot.
conservation_check_after_swap() {
  local fund_snapshot="$1"
  local actual swap_delta tolerance

  actual="$(wallet_population_total_sat)"
  tolerance="$(conservation_swap_tolerance)"

  log "conservation check: after swap (impl=${WALLET_IMPL}, unit=${UNIT})"
  log "  post-fund total:  ${fund_snapshot} ${UNIT}"
  log "  actual total:     ${actual} ${UNIT}"
  log "  swap tolerance:   ${tolerance} ${UNIT}"

  if (( actual > fund_snapshot )); then
    die "conservation violated (after swap): inflation detected (${actual} > ${fund_snapshot})"
  fi

  swap_delta=$((fund_snapshot - actual))
  if (( swap_delta > tolerance )); then
    die "conservation violated (after swap): lost ${swap_delta} ${UNIT} (tolerance ${tolerance})"
  fi

  if (( swap_delta > 0 )); then
    log "conservation ok (after swap, ${swap_delta} ${UNIT} swap cost within tolerance)"
  else
    log "conservation ok (after swap, lossless)"
  fi
}
