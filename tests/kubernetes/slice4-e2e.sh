#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KUBECTL=(kubectl --context k3d-proofstorm)
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

cargo build --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -p proofstorm-mcp
python3 "${ROOT_DIR}/tests/kubernetes/slice4_mcp_client.py" \
  "${ROOT_DIR}/target/debug/proofstorm-mcp" "${TMP_DIR}/proofstorm.sqlite3"

"${KUBECTL[@]}" get configmap -n proofstorm-system \
  -l proofstorm.dev/receipt=teardown -o jsonpath='{.items[0].data.verifiedAbsent}' | grep -Fx true
if "${KUBECTL[@]}" get namespaces -l proofstorm.dev/instance -o name | grep -q .; then
  echo "a Proofstorm instance namespace remains after verified close" >&2
  exit 1
fi

echo "Slice 4 MCP materialization, readiness, and verified close acceptance passed"
