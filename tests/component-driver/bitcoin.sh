#!/bin/sh
# Disposable regtest process restart; no network, external state, or real funds.
set -eu
test "$(id -u)" = 1000
bitcoind --version
cleanup() { bitcoin-cli -regtest stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
start() {
    bitcoind -regtest -daemon -server -listen=0 -fallbackfee=0.0002 >/dev/null
    bitcoin-cli -regtest -rpcwait -rpcwaittimeout=30 getblockcount >/dev/null
}
start
bitcoin-cli -regtest createwallet contract >/dev/null
address=$(bitcoin-cli -regtest -rpcwallet=contract getnewaddress)
bitcoin-cli -regtest -rpcwallet=contract generatetoaddress 101 "$address" >/dev/null
test "$(bitcoin-cli -regtest getblockcount)" = 101
bitcoin-cli -regtest stop >/dev/null
# Wait for the daemon to release its lock before the same datadir is reopened.
attempt=0
while bitcoin-cli -regtest getblockcount >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 100
    sleep 0.1
done
attempt=0
until bitcoind -regtest -daemon -server -listen=0 -fallbackfee=0.0002 >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 100
    sleep 0.1
done
test "$(bitcoin-cli -regtest -rpcwait -rpcwaittimeout=30 getblockcount)" = 101
bitcoin-cli -regtest loadwallet contract >/dev/null
bitcoin-cli -regtest -rpcwallet=contract getwalletinfo >/dev/null
printf '{"bitcoin_regtest":true,"mined_blocks":101,"restart_persistence":true}\n'
