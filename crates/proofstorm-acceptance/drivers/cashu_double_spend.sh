#!/bin/sh
# Runs only inside the disposable gate's funded CDK wallet component.
# Tokens and command logs never leave its private volume; stdout is a numeric receipt.
set -eu
umask 077
stage=initialization
report_failure() {
    status=$?
    if [ "$status" -ne 0 ]; then
        printf 'proof-spend fixture failed at %s (exit %s)\n' "$stage" "$status" >&2
    fi
}
trap report_failure 0
scratch=$(mktemp -d /wallet/proof-spend.XXXXXX)
# Rust embeds the existing product observer, not a second balance implementation.
cat > "$scratch/observe_balance.py" <<'PY'
__PROOFSTORM_BALANCE_OBSERVER__
PY

cli() {
    work=$1
    shift
    timeout -k 2 30 cdk-cli --work-dir "$work" --unit sat --non-interactive "$@"
}

balance() {
    # CLI balance starts saga recovery and mixes its messages into stdout.
    # Observe the database passively, failing closed on missing/incompatible state.
    python3 -c '
import runpy, sys
try:
    observe = runpy.run_path(sys.argv[1])["observe"]
    result = observe(sys.argv[2], "fixture", "mint", "http://mint:3338")
except Exception:
    sys.exit("wallet balance observation failed")
print(result["balance_sat"])
' "$scratch/observe_balance.py" "$1/cdk-cli.sqlite"
}

send() {
    cli /wallet/cdk send --mint-url http://mint:3338 -a "$1" > "$scratch/send.log" 2>&1
    token=$(grep -oE 'cashu[AB][A-Za-z0-9+/_=-]+' "$scratch/send.log")
    # Refuse multiple matches instead of choosing a possibly unrelated token.
    test "$(printf '%s\n' "$token" | wc -l)" -eq 1
    test -n "$token"
}

spent() {
    if grep -Eiq 'already (been )?spent|proofs?.*spent|token.*spent' "$1"; then
        printf true
    else
        printf false
    fi
}

stage=balance-before
before=$(balance /wallet/cdk)
stage=send-for-replay
send 32
stage='first-receive'
cli "$scratch/recipient" receive --allow-untrusted "$token" > "$scratch/first.log" 2>&1
stage=balance-first
first=$(balance "$scratch/recipient")
if cli "$scratch/recipient" receive --allow-untrusted "$token" > "$scratch/replay.log" 2>&1; then
    replay_rc=0
else
    replay_rc=$?
fi
stage=balance-after-replay
after_replay=$(balance "$scratch/recipient")
# A fresh client also submits the spent proofs, avoiding a local-wallet-only oracle.
if cli "$scratch/fresh" receive --allow-untrusted "$token" > "$scratch/fresh.log" 2>&1; then
    fresh_rc=0
else
    fresh_rc=$?
fi
stage=balance-fresh-replay
fresh_balance=$(balance "$scratch/fresh")

stage=send-for-race
send 16
stage=seed-isolation
# Two independently initialized states; no seed or DB copying.
cli "$scratch/race-a" balance > "$scratch/init-a.log" 2>&1
cli "$scratch/race-b" balance > "$scratch/init-b.log" 2>&1
balance "$scratch/race-a" > "$scratch/empty-a"
balance "$scratch/race-b" > "$scratch/empty-b"
test "$(cat "$scratch/empty-a")" -eq 0
test "$(cat "$scratch/empty-b")" -eq 0
if cmp -s "$scratch/race-a/seed" "$scratch/race-b/seed"; then exit 1; fi
test -s "$scratch/race-a/seed"
test -s "$scratch/race-b/seed"

race() {
    slot=$1
    : > "$scratch/ready-$slot"
    while [ ! -f "$scratch/go" ]; do sleep 0.01; done
    if cli "$scratch/race-$slot" receive --allow-untrusted "$token" > "$scratch/race-$slot.log" 2>&1; then
        printf '0\n' > "$scratch/rc-$slot"
    else
        printf '%s\n' "$?" > "$scratch/rc-$slot"
    fi
}
stage=race
race a &
pid_a=$!
race b &
pid_b=$!
while [ ! -f "$scratch/ready-a" ] || [ ! -f "$scratch/ready-b" ]; do sleep 0.01; done
: > "$scratch/go"
wait "$pid_a"
wait "$pid_b"
stage=accounting
a=$(balance "$scratch/race-a")
b=$(balance "$scratch/race-b")
source_after=$(balance /wallet/cdk)
printf '{"before":%s,"first":%s,"after_replay":%s,"replay_rc":%s,"fresh_rc":%s,"fresh_spent":%s,"fresh_balance":%s,"race_rc":[%s,%s],"race_spent":[%s,%s],"race_balance":[%s,%s],"source_after":%s}\n' \
    "$before" "$first" "$after_replay" "$replay_rc" "$fresh_rc" "$(spent "$scratch/fresh.log")" "$fresh_balance" \
    "$(cat "$scratch/rc-a")" "$(cat "$scratch/rc-b")" "$(spent "$scratch/race-a.log")" "$(spent "$scratch/race-b.log")" \
    "$a" "$b" "$source_after"
