#!/bin/bash
# Runs only within the verified Mac sandbox, with a disposable HOME and no network.
set -euo pipefail
work=$1 archive=$2
cd "$work"
for tool in cargo rustc trunk; do
  if command -v "$tool" >/dev/null 2>&1; then printf 'Unexpected build tool on installer PATH: %s\n' "$tool" >&2; exit 1; fi
done
for attempt in first reinstall; do
  printf 'Checking %s Mac install\n' "$attempt"
  if [[ "$attempt" == first ]]; then
    /bin/sh "$work/input/install.sh" --artifact-dir "$work/input" --archive "$archive" --prefix "$HOME/.local"
  else
    observed=$(readlink "$HOME/.local/lib/proofstorm/current")
    digest=$(awk '{print $1}' "$work/input/$archive.sha256")
    bytes=$(wc -c < "$work/input/$archive" | tr -d ' ')
    if /bin/sh "$work/input/install.sh" --artifact-dir "$work/input" --archive "$archive" --prefix "$HOME/.local" \
      --expected-current ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff \
      --expected-sha256 "$digest" --expected-bytes "$bytes" --report-json > "$work/stale-receipt.json" 2> "$work/stale-error"; then
      printf 'Stale activation precondition was accepted\n' >&2; exit 1
    fi
    [[ "$observed" == "$(readlink "$HOME/.local/lib/proofstorm/current")" ]]
    /bin/sh "$work/input/install.sh" --artifact-dir "$work/input" --archive "$archive" --prefix "$HOME/.local" \
      --expected-current "${observed#versions/}" --expected-sha256 "$digest" --expected-bytes "$bytes" --report-json > "$work/update-receipt.json"
    grep -Fq "\"bundle_id\": \"${observed#versions/}\"" "$work/update-receipt.json"
    "$HOME/.local/bin/proofstorm" update --help | grep -q -- --check
    "$HOME/.local/bin/proofstorm" upgrade --help >/dev/null
  fi
  "$HOME/.local/bin/proofstorm" --version
  "$HOME/.local/bin/proofstorm" --help >/dev/null
  "$HOME/.local/bin/proofstorm" version --json > "$work/cli-info.json"
  "$HOME/.local/bin/proofstorm-mcp" --version
  "$HOME/.local/bin/proofstorm-mcp" --help >/dev/null
  "$HOME/.local/bin/proofstorm-mcp" --release-info > "$work/mcp-info.json"
  cmp "$work/cli-info.json" "$work/mcp-info.json"
  if [[ "$attempt" == first ]]; then cp "$work/cli-info.json" "$work/first-info.json"; else cmp "$work/first-info.json" "$work/cli-info.json"; fi
  [[ ! -e "$PROOFSTORM_HOME" && ! -e "$HOME/.codex" && ! -e "$HOME/.claude.json" && ! -e "$HOME/.config/opencode" ]]
done
printf 'Mac install and reinstall checks passed.\n'
