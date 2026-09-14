#!/bin/sh
# Runs inside source-free Debian, never on the maintainer's host.
set -eu
for tool in cargo rustc trunk python3 docker; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "Unexpected prerequisite in clean installer test: $tool" >&2
        exit 1
    fi
done
test ! -e /input/source
mkdir -p /tmp/first-user
export HOME=/tmp/first-user PROOFSTORM_HOME=/tmp/runtime-must-not-exist
if [ "$2" = true ]; then set -- "$1" --allow-development; else set -- "$1"; fi
for attempt in first reinstall; do
    printf 'Checking %s install\n' "$attempt"
    if [ "$attempt" = first ]; then
        sh /input/install.sh --artifact-dir /input --archive "$1" \
            --prefix /tmp/first-user/.local ${2:+"$2"}
    else
        observed=$(readlink /tmp/first-user/.local/lib/proofstorm/current)
        digest=$(awk '{print $1}' "/input/$1.sha256")
        bytes=$(wc -c < "/input/$1" | tr -d ' ')
        if sh /input/install.sh --artifact-dir /input --archive "$1" \
            --prefix /tmp/first-user/.local ${2:+"$2"} --expected-current ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff \
            --expected-sha256 "$digest" --expected-bytes "$bytes" --report-json > /tmp/stale-receipt.json 2>/tmp/stale-error; then
            echo 'Stale activation precondition was accepted' >&2; exit 1
        fi
        test "$observed" = "$(readlink /tmp/first-user/.local/lib/proofstorm/current)"
        sh /input/install.sh --artifact-dir /input --archive "$1" \
            --prefix /tmp/first-user/.local ${2:+"$2"} --expected-current "${observed#versions/}" \
            --expected-sha256 "$digest" --expected-bytes "$bytes" --report-json > /tmp/update-receipt.json
        grep -Fq "\"bundle_id\": \"${observed#versions/}\"" /tmp/update-receipt.json
        /tmp/first-user/.local/bin/proofstorm update --help | grep -q -- --check
        /tmp/first-user/.local/bin/proofstorm upgrade --help >/dev/null
    fi
    /tmp/first-user/.local/bin/proofstorm --version
    /tmp/first-user/.local/bin/proofstorm --help >/dev/null
    /tmp/first-user/.local/bin/proofstorm version --json > /tmp/cli-info.json
    /tmp/first-user/.local/bin/proofstorm-mcp --version
    /tmp/first-user/.local/bin/proofstorm-mcp --help >/dev/null
    /tmp/first-user/.local/bin/proofstorm-mcp --release-info > /tmp/mcp-info.json
    cmp /tmp/cli-info.json /tmp/mcp-info.json
    test ! -e /tmp/runtime-must-not-exist
    test ! -e /tmp/first-user/.codex
    test ! -e /tmp/first-user/.config/opencode
done
echo 'Install and reinstall checks passed.'
