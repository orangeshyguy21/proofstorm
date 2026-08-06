#!/usr/bin/env bash
# proofstorm regtest topology setup — runs on the HOST, drives containers via
# `docker exec` (proofstorm's control-plane style). Mines the initial chain,
# funds lnd-a + lnd-b on-chain, and opens a balanced lnd-a ⇄ lnd-b channel so
# both mints have outbound liquidity for melt.
#
# Idempotent: safe to re-run against an already-funded topology.
#
# Env (see regtest/env): BTC_RPC_USER, BTC_RPC_PASS, CHANNEL_CAPACITY_SAT,
# CHANNEL_PUSH_SAT. Container names are the compose defaults.

set -euo pipefail

BTC_RPC_USER="${BTC_RPC_USER:-polar}"
BTC_RPC_PASS="${BTC_RPC_PASS:-polar}"
CHANNEL_CAPACITY_SAT="${CHANNEL_CAPACITY_SAT:-10000000}"
CHANNEL_PUSH_SAT="${CHANNEL_PUSH_SAT:-5000000}"

BITCOIND="${RT_BITCOIND:-proofstorm-rt-bitcoind}"
LND_A="${RT_LND_A:-proofstorm-rt-lnd-a}"
LND_B="${RT_LND_B:-proofstorm-rt-lnd-b}"

log() { printf '[regtest-setup] %s\n' "$*"; }
die() { printf '[regtest-setup] error: %s\n' "$*" >&2; exit 1; }

bcli() {
  docker exec -i "${BITCOIND}" bitcoin-cli -regtest \
    -rpcuser="${BTC_RPC_USER}" -rpcpassword="${BTC_RPC_PASS}" "$@"
}
bcli_w() {  # against the `default` wallet
  docker exec -i "${BITCOIND}" bitcoin-cli -regtest \
    -rpcuser="${BTC_RPC_USER}" -rpcpassword="${BTC_RPC_PASS}" -rpcwallet=default "$@"
}
lnc() {  # lnc <container> <lncli args...>
  local c="$1"; shift
  docker exec -i "$c" lncli --lnddir=/home/lnd/.lnd --network=regtest "$@"
}

wait_for() {
  local desc="$1"; shift
  local tries=90
  while (( tries > 0 )); do
    if "$@" >/dev/null 2>&1; then
      log "ready: ${desc}"
      return 0
    fi
    tries=$((tries - 1))
    sleep 1
  done
  die "timeout waiting for ${desc}"
}

# ---- 1. bitcoind + wallet + initial coins ----
wait_for "bitcoind rpc" bcli getblockchaininfo
if ! bcli listwallets | jq -e '.[] | select(. == "default")' >/dev/null 2>&1; then
  log "creating bitcoind 'default' wallet"
  bcli createwallet default >/dev/null
fi

MINER_ADDR="$(bcli_w getnewaddress)"
log "mining 101 blocks to ${MINER_ADDR}"
bcli_w generatetoaddress 101 "${MINER_ADDR}" >/dev/null
log "chain height: $(bcli_w getblockcount)"

# ---- 2. fund both LND nodes on-chain ----
wait_for "lnd-a getinfo" lnc "${LND_A}" getinfo
wait_for "lnd-b getinfo" lnc "${LND_B}" getinfo

for pair in "${LND_A}" "${LND_B}"; do
  addr="$(lnc "$pair" newaddress p2wkh | jq -r '.address')"
  [[ -n "$addr" && "$addr" != "null" ]] || die "no address from ${pair}"
  log "funding ${pair} at ${addr} (10 BTC)"
  bcli_w sendtoaddress "${addr}" 10 >/dev/null
done

log "mining 6 confirmations"
bcli_w generatetoaddress 6 "${MINER_ADDR}" >/dev/null

lnd_confirmed_ge() {  # lnd_confirmed_ge <container> <min_sat>
  local bal
  bal="$(lnc "$1" walletbalance | jq -r '.confirmed_balance')"
  [[ -n "$bal" && "$bal" != "null" ]] && (( bal >= $2 ))
}
wait_for "lnd-a confirmed ≥ 9 BTC" lnd_confirmed_ge "${LND_A}" 900000000
wait_for "lnd-b confirmed ≥ 9 BTC" lnd_confirmed_ge "${LND_B}" 900000000

# ---- 3. peer + open a balanced channel lnd-a ⇄ lnd-b ----
B_PK="$(lnc "${LND_B}" getinfo | jq -r '.identity_pubkey')"
[[ -n "$B_PK" && "$B_PK" != "null" ]] || die "no pubkey from lnd-b"
log "lnd-b pubkey: ${B_PK}"

if lnc "${LND_A}" listpeers | jq -e --arg pk "$B_PK" '.peers[]? | select(.pub_key == $pk)' >/dev/null 2>&1; then
  log "lnd-a already peered with lnd-b"
else
  log "peering lnd-a → lnd-b"
  lnc "${LND_A}" connect "${B_PK}@lnd-b:9735" >/dev/null 2>&1 || \
    log "connect returned non-zero (already connected?) — continuing"
fi

channel_exists() {
  lnc "${LND_A}" listchannels | \
    jq -e --arg pk "$B_PK" '.channels[]? | select(.remote_pubkey == $pk)' >/dev/null 2>&1
}
if channel_exists; then
  log "channel lnd-a → lnd-b already open"
else
  log "opening channel lnd-a → lnd-b (cap ${CHANNEL_CAPACITY_SAT}, push ${CHANNEL_PUSH_SAT})"
  lnc "${LND_A}" openchannel --node_key="${B_PK}" \
    --local_amt="${CHANNEL_CAPACITY_SAT}" --push_amt="${CHANNEL_PUSH_SAT}" >/dev/null
fi

log "mining 6 confirmations for channel funding"
bcli_w generatetoaddress 6 "${MINER_ADDR}" >/dev/null

channel_active() {
  local n
  n="$(lnc "$1" listchannels | jq -r '[.channels[]? | select(.active == true)] | length')"
  [[ -n "$n" && "$n" != "null" ]] && (( n >= 1 ))
}
wait_for "lnd-a channel active" channel_active "${LND_A}"
wait_for "lnd-b channel active" channel_active "${LND_B}"

# gossip settle
sleep 3
log "DONE — regtest topology ready (channel active, both mints have liquidity)"
