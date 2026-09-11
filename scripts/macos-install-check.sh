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
  /bin/sh "$work/input/install.sh" --artifact-dir "$work/input" --archive "$archive" --prefix "$HOME/.local"
  "$HOME/.local/bin/proofstorm" --version
  "$HOME/.local/bin/proofstorm" --help >/dev/null
  "$HOME/.local/bin/proofstorm" release-info > "$work/cli-info.json"
  "$HOME/.local/bin/proofstorm-mcp" --version
  "$HOME/.local/bin/proofstorm-mcp" --help >/dev/null
  "$HOME/.local/bin/proofstorm-mcp" --release-info > "$work/mcp-info.json"
  cmp "$work/cli-info.json" "$work/mcp-info.json"
  if [[ "$attempt" == first ]]; then cp "$work/cli-info.json" "$work/first-info.json"; else cmp "$work/first-info.json" "$work/cli-info.json"; fi
  [[ ! -e "$PROOFSTORM_HOME" && ! -e "$HOME/.codex" && ! -e "$HOME/.claude.json" && ! -e "$HOME/.config/opencode" ]]
done
printf 'Mac install and reinstall checks passed.\n'
