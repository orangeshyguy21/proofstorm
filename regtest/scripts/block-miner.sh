#!/bin/sh
# proofstorm regtest perpetual block miner (in-container sidecar).
#
# Mines one block every MINE_INTERVAL_SECONDS against this stack's `bitcoind`,
# to the `default` wallet that fund-topology.sh creates. Adapted from
# orchard/e2e/docker/scripts/block-miner.sh. Regtest only.
#
# Env: MINE_INTERVAL_SECONDS (default 30), BTC_RPC_USER, BTC_RPC_PASS.

set -eu

INTERVAL="${MINE_INTERVAL_SECONDS:-30}"

log() { printf '[block-miner] %s\n' "$*"; }

bcli() {
    bitcoin-cli -regtest \
        -rpcconnect=bitcoind -rpcport=18443 \
        -rpcuser="${BTC_RPC_USER:-polar}" -rpcpassword="${BTC_RPC_PASS:-polar}" \
        -rpcwallet=default \
        "$@"
}

# fund-topology.sh creates the `default` wallet and mines the initial blocks
# before the Makefile starts this service, so getnewaddress should succeed
# quickly — but retry in case of a race on `regtest-up`.
ADDR=""
tries=60
while [ "$tries" -gt 0 ]; do
    if ADDR=$(bcli getnewaddress 2>/dev/null) && [ -n "$ADDR" ]; then
        break
    fi
    tries=$((tries - 1))
    sleep 1
done

if [ -z "$ADDR" ]; then
    log "could not obtain mining address from bitcoind default wallet"
    exit 1
fi

log "mining 1 block every ${INTERVAL}s to ${ADDR}"

while true; do
    if bcli generatetoaddress 1 "$ADDR" >/dev/null 2>&1; then
        log "mined block, height=$(bcli getblockcount 2>/dev/null || echo '?')"
    else
        log "mine failed (bitcoind unreachable?); will retry"
    fi
    sleep "$INTERVAL"
done
