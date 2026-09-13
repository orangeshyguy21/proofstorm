#!/usr/bin/env bash
set -euo pipefail
umask 077
export HOME=/wallet
driver=/opt/proofstorm/driver
daemon_pid=''
phase=startup
cleanup() {
    local result=$?
    if [[ -n "$daemon_pid" ]]; then
        kill "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
    if ((result != 0)); then printf 'Coco native contract failed: %s\n' "$phase" >&2; fi
}
trap cleanup EXIT
if command -v python || command -v python3; then exit 1; fi
[[ $(id -u) == 1000 ]]
cocod daemon >/wallet/daemon.log 2>&1 &
daemon_pid=$!
for ((attempt=0; attempt<80; attempt++)); do
    if "$driver" ready cocod >/dev/null; then break; fi
    kill -0 "$daemon_pid"
    sleep 0.25
done
"$driver" ready cocod
phase=initialization
"$driver" coco uninitialized
"$driver" coco initialize http://127.0.0.1:3338
"$driver" coco locked
identity_before=$("$driver" coco identity)
[[ "$identity_before" =~ ^\"[a-f0-9]{64}\"$ ]]
phase=restart
kill "$daemon_pid"
wait "$daemon_pid" || true
daemon_pid=''
cocod daemon >>/wallet/daemon.log 2>&1 &
daemon_pid=$!
for ((attempt=0; attempt<80; attempt++)); do
    if "$driver" ready cocod >/dev/null; then break; fi
    kill -0 "$daemon_pid"
    sleep 0.25
done
"$driver" ready cocod
"$driver" coco locked
[[ $("$driver" coco identity) == "$identity_before" ]]
printf '{"coco_native_lifecycle":true,"identity_survived_restart":true,"python_absent":true}\n'
