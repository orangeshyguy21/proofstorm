#!/bin/sh
# Record a finite series alongside other tasks, using the shared cell network.
set -eu
rpc_url=${BITCOIN_RPC_URL:-http://proofstorm:proofstorm-regtest-only@chain:18443/}
samples=${SAMPLES:-60}
interval=${INTERVAL_SECONDS:-5}
i=0
while [ "$i" -lt "$samples" ]; do
    reply=$(wget -qO- -T 10 --post-data='{"jsonrpc":"1.0","id":"workspace-recorder","method":"getblockcount","params":[]}' "$rpc_url")
    printf '%s\n' "$reply" | grep -Eq '"error"[[:space:]]*:[[:space:]]*null' || exit 1
    printf '{"observed_at":%s,"rpc":%s}\n' "$(date +%s)" "$reply" >> "$PROOFSTORM_OUTPUT/heights.jsonl"
    printf 'Recorded sample %s\n' "$i"
    i=$((i + 1))
    sleep "$interval"
done
