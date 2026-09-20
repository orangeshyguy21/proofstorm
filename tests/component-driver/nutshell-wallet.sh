#!/usr/bin/env bash
# Native CLI regression for the default wallet in separately stored components.
# Run only in a disposable Nutshell container with writable /wallet and no network.
set -euo pipefail
umask 077
stage=startup
daemon_pid=''
cleanup() {
    local result=$?
    if [[ -n "$daemon_pid" ]]; then
        kill "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
    if ((result != 0)); then printf 'Nutshell CLI wallet failed: %s\n' "$stage" >&2; fi
}
trap cleanup EXIT
scratch=$(mktemp -d /wallet/cli-contract.XXXXXX)
mkdir "$scratch/alice" "$scratch/bob"
export HOME="$scratch" CASHU_DIR="$scratch/.cashu" TOR=false LOG_LEVEL=ERROR
export MINT_BACKEND_BOLT11_SAT=FakeWallet MINT_PRIVATE_KEY=disposable-cli-contract-seed
export MINT_DATABASE="$scratch/mint" MINT_AUTH_DATABASE="$scratch/auth"
export MINT_LISTEN_HOST=127.0.0.1 MINT_LISTEN_PORT=3338
export MINT_INPUT_FEE_PPK=0 MINT_RATE_LIMIT=false MINT_RPC_SERVER_ENABLE=false
export FAKEWALLET_BRR=true FAKEWALLET_DELAY_INCOMING_PAYMENT=0 FAKEWALLET_DELAY_OUTGOING_PAYMENT=0
timeout -k 2 110 mint >"$scratch/mint.log" 2>&1 &
daemon_pid=$!
for ((attempt=0; attempt<100; attempt++)); do
    if curl --noproxy '*' --fail --silent --max-time 1 http://127.0.0.1:3338/v1/info >"$scratch/info.json"; then break; fi
    kill -0 "$daemon_pid"
    sleep 0.2
done
curl --noproxy '*' --fail --silent --max-time 1 http://127.0.0.1:3338/v1/info >"$scratch/info.json"

cli() {
    local component=$1
    shift
    HOME="$scratch/$component" CASHU_DIR="$scratch/$component/.cashu" PROOFSTORM_WALLET="$component" \
        timeout -k 2 30 cashu -h http://127.0.0.1:3338 -u sat -w wallet -t -y "$@"
}
balance() {
    cli "$1" balance >"$scratch/$1-balance.log" 2>&1
    sed -n 's/^Balance: \([0-9][0-9]*\) sat$/\1/p' "$scratch/$1-balance.log"
}
send() {
    cli "$1" send "$2" >"$scratch/send.log" 2>&1
    token=$(grep -Eo 'cashu[AB][A-Za-z0-9_=-]+' "$scratch/send.log")
    [[ $(printf '%s\n' "$token" | wc -l) -eq 1 && -n "$token" ]]
}

stage=alice-funding
cli alice invoice 1000 --no-check >"$scratch/invoice.log" 2>&1
quote_id=$(sed -n 's/.*--id \([^[:space:]]*\).*/\1/p' "$scratch/invoice.log")
[[ -n "$quote_id" && "$quote_id" != *$'\n'* ]]
# Claim with the installed CLI, then inspect the native database without mutation.
cli alice invoice 1000 --id "$quote_id" >"$scratch/claim-native.log" 2>&1
HOME="$scratch/alice" PROOFSTORM_WALLET=alice PROOFSTORM_MINT=mint \
    PROOFSTORM_EXPECTED_MINT_URL=http://127.0.0.1:3338 PROOFSTORM_MINT_QUOTE_ID="$quote_id" \
    PROOFSTORM_OBSERVATION_ROLE=claim_receive \
    timeout -k 2 35 /opt/proofstorm/driver quote observe-receive >"$scratch/claim.log" 2>&1
grep -q '"wallet_id":"alice"' "$scratch/claim.log"
grep -q '"state":"ISSUED"' "$scratch/claim.log"
grep -q '"amount_sat":1000' "$scratch/claim.log"
[[ $(balance alice) == 1000 ]]
stage=bob-isolation
[[ $(balance bob) == 0 ]]
stage=alice-send
send alice 250
[[ $(balance alice) == 750 ]]
stage=bob-receive
cli bob receive "$token" >"$scratch/bob-receive.log" 2>&1
grep -q '^Received 250 sat$' "$scratch/bob-receive.log"
grep -q '^Balance: 250 sat$' "$scratch/bob-receive.log"
stage=bob-fresh-balance
[[ $(balance bob) == 250 ]]
cli bob balance --verbose >"$scratch/bob-verbose.log" 2>&1
grep -q '^Balance: 250 sat (pending: 0 sat)' "$scratch/bob-verbose.log"
cli bob wallets >"$scratch/bob-wallets.log" 2>&1
grep -q '^Wallet: wallet[[:space:]]*Balance: 250 sat (available: 250 sat)' "$scratch/bob-wallets.log"
stage=bob-spend
send bob 50
stage=alice-receive
cli alice receive "$token" >"$scratch/alice-receive.log" 2>&1
stage=conservation
[[ $(balance alice) == 800 && $(balance bob) == 200 ]]
printf '{"nutshell_cli_wallet":true,"internal_name":"wallet","received_sat":250,"returned_sat":50,"alice_balance_sat":800,"bob_balance_sat":200,"lightning_backend":"FakeWallet"}\n'
