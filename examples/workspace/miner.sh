#!/bin/sh
# Uses the built-in BusyBox runtime. MINING_ADDRESS must be a regtest address.
set -eu
: "${MINING_ADDRESS:?Set MINING_ADDRESS to a regtest mining address}"
case "$MINING_ADDRESS" in *[!a-zA-Z0-9]*) printf '%s\n' 'Invalid mining address' >&2; exit 1 ;; esac
rpc_url=${BITCOIN_RPC_URL:-http://proofstorm:proofstorm-regtest-only@chain:18443/}
interval=${INTERVAL_SECONDS:-30}
while :; do
    if [ ! -e "$PROOFSTORM_WORKSPACE/data/mining-paused" ]; then
        reply=$(wget -qO- -T 10 --post-data="{\"jsonrpc\":\"1.0\",\"id\":\"workspace-miner\",\"method\":\"generatetoaddress\",\"params\":[1,\"$MINING_ADDRESS\"]}" "$rpc_url")
        printf '%s\n' "$reply"
        printf '%s\n' "$reply" | grep -Eq '"error"[[:space:]]*:[[:space:]]*null' || exit 1
    fi
    sleep "$interval"
done
