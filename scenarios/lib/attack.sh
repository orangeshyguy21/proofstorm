# Shared helpers for proofstorm adversarial scenarios (SPEC.md Phase 6).
# Source from scenario scripts under scenarios/. Targets the regtest stack
# (compose.regtest.yml) via `docker exec` into the adversary + LND containers.
#
# Selection: MINT=cdk|nutshell picks the mint under attack. Because the two
# mints share one regtest chain and their LN nodes are channel peers, the
# "payer" node (which settles the mint's bolt11 to fund the adversary) is the
# *other* mint's backend node.

set -euo pipefail

PROOFSTORM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Regtest env (BTC creds, host ports) — optional, defaults below.
if [[ -f "${PROOFSTORM_ROOT}/regtest/env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "${PROOFSTORM_ROOT}/regtest/env"
  set +a
fi

MINT="${MINT:-cdk}"
UNIT="${UNIT:-sat}"

ADV="${RT_ADVERSARY:-proofstorm-rt-adversary}"
LND_A="${RT_LND_A:-proofstorm-rt-lnd-a}"
LND_B="${RT_LND_B:-proofstorm-rt-lnd-b}"

log()  { printf '[attack] %s\n' "$*"; }
die()  { printf '[attack] error: %s\n' "$*" >&2; exit 1; }
pass() { printf '[attack] PASS: %s\n' "$*"; }

case "${MINT}" in
  cdk)
    MINT_SVC="cdk-mintd"
    MINT_HOST_PORT="${CDK_MINT_HOST_PORT:-3338}"
    PAYER="${LND_B}"      # cdk-mintd's backend is lnd-a → pay from lnd-b
    ;;
  nutshell)
    MINT_SVC="nutshell"
    MINT_HOST_PORT="${NUTSHELL_MINT_HOST_PORT:-3339}"
    PAYER="${LND_A}"      # nutshell's backend is lnd-b → pay from lnd-a
    ;;
  *)
    die "MINT must be cdk or nutshell (got ${MINT})"
    ;;
esac

MINT_URL="http://${MINT_SVC}:3338"                 # in-network (from adversary)
MINT_HOST_URL="http://127.0.0.1:${MINT_HOST_PORT}" # from host (DoS probes)

# Run cdk-cli inside the adversary container against an explicit work dir, so a
# scenario can hold several independent wallet states (required for a real
# concurrent double-spend: two independent clients submitting the same proofs).
adv() {
  local workdir="$1"; shift
  docker exec -i "${ADV}" cdk-cli --work-dir "${workdir}" --unit "${UNIT}" "$@"
}

lnc() {  # lnc <container> <lncli args...>
  local c="$1"; shift
  docker exec -i "$c" lncli --lnddir=/home/lnd/.lnd --network=regtest "$@"
}

# Spendable balance (integer sat) for a given work dir. cdk-cli prints nothing
# for a fresh/empty wallet, which we treat as 0 (matches scripts/lib/wallet.sh).
adv_balance_sat() {
  local workdir="$1" out amount
  out="$(adv "${workdir}" balance 2>&1)" || return 1
  amount="$(printf '%s\n' "$out" \
    | grep -E '^Total balance across all wallets:' \
    | grep -oE '[0-9]+' | head -1 || true)"
  [[ -n "$amount" ]] || amount=0
  printf '%s\n' "$amount"
}

# Fund a work dir by minting `amount` and settling the mint's bolt11 from the
# peer LN node. cdk-cli `mint` fetches a quote, prints the invoice, then polls
# until paid and redeems; we pay it out-of-band from PAYER.
adv_fund() {
  local workdir="$1" amount="$2"
  local tmp bolt11 tries pid
  tmp="$(mktemp)"
  log "funding ${workdir} with ${amount} ${UNIT} at ${MINT_URL}"

  adv "${workdir}" mint "${MINT_URL}" "${amount}" > "${tmp}" 2>&1 &
  pid=$!

  bolt11=""
  tries=60
  while (( tries > 0 )); do
    bolt11="$(grep -oE 'lnbcrt[0-9a-z]+' "${tmp}" 2>/dev/null | head -1 || true)"
    [[ -n "$bolt11" ]] && break
    sleep 0.5
    tries=$((tries - 1))
  done
  if [[ -z "$bolt11" ]]; then
    sed 's/^/    /' "${tmp}" >&2 || true
    kill "$pid" 2>/dev/null || true
    rm -f "${tmp}"
    die "mint produced no bolt11 (LN backend wired? topology funded?)"
  fi

  log "paying mint invoice from ${PAYER}"
  lnc "${PAYER}" payinvoice --force "${bolt11}" >/dev/null 2>&1 \
    || { rm -f "${tmp}"; die "payer could not settle the mint invoice (channel liquidity?)"; }

  if ! wait "$pid"; then
    sed 's/^/    /' "${tmp}" >&2 || true
    rm -f "${tmp}"
    die "cdk-cli mint did not redeem after payment"
  fi
  rm -f "${tmp}"
  log "funded ${workdir}: $(adv_balance_sat "${workdir}") ${UNIT}"
}

# Reset a work dir to empty state (fresh wallet DB + seed).
adv_reset() {
  local workdir="$1"
  docker exec -i "${ADV}" sh -c "rm -rf '${workdir}' && mkdir -p '${workdir}'"
}

# Assert a command FAILS (non-zero). Used as the core security oracle: the mint
# must reject the attack. Prints the (unexpected) output on failure.
assert_fails() {
  local label="$1"; shift
  local out
  if out="$("$@" 2>&1)"; then
    printf '%s\n' "$out" | sed 's/^/    /' >&2
    die "ORACLE VIOLATED: '${label}' SUCCEEDED but must be rejected by the mint"
  fi
  pass "${label} rejected by mint (as required)"
}

# Assert integer equality with a labelled message.
assert_eq() {
  local label="$1" got="$2" want="$3"
  if [[ "$got" != "$want" ]]; then
    die "ORACLE VIOLATED: ${label}: got ${got}, want ${want}"
  fi
  pass "${label}: ${got} == ${want}"
}
