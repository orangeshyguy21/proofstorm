#!/usr/bin/env bash
set -euo pipefail
umask 077
export HOME=/wallet
driver=/opt/proofstorm/driver
daemon_pid=''
phase=certificates
cleanup() {
    local result=$?
    if [[ -n "$daemon_pid" ]]; then
        kill "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
    if ((result != 0)); then printf 'Nutshell native contract failed: %s\n' "$phase" >&2; fi
}
trap cleanup EXIT
[[ $(id -u) == 1000 ]]
[[ $(mint --version) == 'Nutshell, version 0.20.3' ]]
tls=/management-client/tls
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 1 -subj /CN=contract-ca \
    -keyout "$tls/ca.key" -out "$tls/ca.pem" >/dev/null 2>&1
for identity in server client; do
    openssl req -new -newkey rsa:2048 -nodes -subj "/CN=$identity" \
        -keyout "$tls/$identity.key" -out "$tls/$identity.csr" >/dev/null 2>&1
    if [[ "$identity" == server ]]; then
        printf 'subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"$tls/extensions"
    else
        printf 'extendedKeyUsage=clientAuth\n' >"$tls/extensions"
    fi
    openssl x509 -req -in "$tls/$identity.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" \
        -CAcreateserial -days 1 -sha256 -extfile "$tls/extensions" -out "$tls/$identity.pem" >/dev/null 2>&1
done
export MINT_RPC_SERVER_ENABLE=true MINT_RPC_SERVER_MUTUAL_TLS=true MINT_RPC_SERVER_ADDR=127.0.0.1
export MINT_RPC_SERVER_CA="$tls/ca.pem" MINT_RPC_SERVER_CERT="$tls/server.pem" MINT_RPC_SERVER_KEY="$tls/server.key"
export MINT_BACKEND_BOLT11_SAT=FakeWallet MINT_PRIVATE_KEY=disposable-contract-mint-seed
export MINT_DATABASE=/app/data/contract-mint MINT_AUTH_DATABASE=/app/data/contract-auth
export MINT_LISTEN_HOST=0.0.0.0 MINT_LISTEN_PORT=3338 MINT_INFO_NAME=contract-mint MINT_INFO_DESCRIPTION=contract-description
export MINT_INPUT_FEE_PPK=101 MINT_QUOTE_TTL=900 MELT_QUOTE_TTL=600
export MINT_MAX_MINT_BOLT11_SAT=500000 MINT_MAX_MELT_BOLT11_SAT=200000 MINT_MAX_BALANCE=1000000
export MINT_RATE_LIMIT=true MINT_RATE_LIMIT_PROXY_TRUST=false MINT_GLOBAL_RATE_LIMIT_PER_MINUTE=3 MINT_TRANSACTION_RATE_LIMIT_PER_MINUTE=2
export LIGHTNING_FEE_PERCENT=1 LIGHTNING_RESERVE_FEE_MIN=2000 MINT_LND_REST_ENDPOINT=http://unused:8080
export LOG_LEVEL=WARNING
phase=startup
mint >/wallet/mint.log 2>&1 &
daemon_pid=$!
for ((attempt=0; attempt<100; attempt++)); do
    if "$driver" ready nutshell http://127.0.0.1:3338/v1/info >/dev/null; then break; fi
    kill -0 "$daemon_pid"
    sleep 0.25
done
"$driver" ready nutshell http://127.0.0.1:3338/v1/info
phase=settings
"$driver" nutshell settings
phase=management-authentication
for identity in server missing; do
    if "$driver" management "$identity" https://127.0.0.1:8086 "$tls" >/dev/null; then exit 1; fi
done
if "$driver" management plaintext http://127.0.0.1:8086 "$tls" >/dev/null; then exit 1; fi
phase=probe-quota-isolation
started=$SECONDS
for ((attempt=0; attempt<100; attempt++)); do
    "$driver" ready nutshell http://127.0.0.1:3338/v1/info
    "$driver" tcp 127.0.0.1 3338
done
# 127.0.0.2 is deliberately outside the pinned mint's exact 127.0.0.1 exemption.
# This exercises real Uvicorn/SlowAPI/ledger startup without an external network.
for expected in 200 200 200 429; do
    actual=$(curl --noproxy '*' --interface 127.0.0.2 --silent --max-time 3 --output /dev/null --write-out '%{http_code}' http://127.0.0.1:3338/v1/info)
    [[ "$actual" == "$expected" ]]
done
for expected in 200 200 429; do
    actual=$(curl --noproxy '*' --interface 127.0.0.3 --silent --max-time 3 --output /dev/null --write-out '%{http_code}' \
        --header 'content-type: application/json' --data '{"unit":"sat","amount":1}' http://127.0.0.1:3338/v1/mint/quote/bolt11)
    [[ "$actual" == "$expected" ]]
done
((SECONDS - started < 60))
"$driver" ready nutshell http://127.0.0.1:3338/v1/info
printf '{"nutshell_native_readiness":true,"management_mtls":true,"readiness_checks":100,"tcp_checks":100,"global_quota_remaining":3,"transaction_quota_remaining":2}\n'
