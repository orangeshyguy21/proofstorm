#!/bin/sh
# Uses Bitcoin's CLI inside the chain component through the scoped control bridge.
set -eu
: "${MINING_ADDRESS:?Set MINING_ADDRESS to a regtest mining address}"
case "$MINING_ADDRESS" in *[!a-zA-Z0-9]*) printf '%s\n' 'Invalid mining address' >&2; exit 1 ;; esac
blocks=${BLOCKS:-10}
case "$blocks" in ''|*[!0-9]*) printf '%s\n' 'BLOCKS must be a positive integer' >&2; exit 1 ;; esac
[ "$blocks" -gt 0 ] && [ "$blocks" -le 4096 ]
iteration=1
while [ "$iteration" -le "$blocks" ]; do
    "$PROOFSTORM_CONTROL" workspace call "{\"call_id\":\"mine-$iteration\",\"component\":\"chain\",\"command\":{\"argv\":[\"bitcoin-cli\",\"-regtest\",\"-rpcconnect=127.0.0.1\",\"-rpcport=18443\",\"-rpcuser=proofstorm\",\"-rpcpassword=proofstorm-regtest-only\",\"generatetoaddress\",\"1\",\"$MINING_ADDRESS\"],\"timeout_seconds\":15,\"output\":{\"mode\":\"public\"}}}"
    iteration=$((iteration + 1))
    if [ "$iteration" -le "$blocks" ]; then sleep "${INTERVAL_SECONDS:-30}"; fi
done
