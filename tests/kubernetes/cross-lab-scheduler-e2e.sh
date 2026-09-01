#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${ROOT_DIR}/.tools/bin:${PATH}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT
export PROOFSTORM_TEST_RUN_ID="$(date +%s)-$$"

cargo build --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -p proofstorm-mcp
python3 "${ROOT_DIR}/tests/kubernetes/cross_lab_scheduler_mcp_client.py" \
  "${ROOT_DIR}/target/debug/proofstorm-mcp" "${TMP_DIR}/proofstorm.sqlite3"

if "${ROOT_DIR}/.tools/bin/kubectl" --context k3d-proofstorm get namespaces \
  -l proofstorm.dev/instance -o name | grep -q .; then
  echo "a Proofstorm instance namespace remains after cross-lab close" >&2
  exit 1
fi

echo "MCP six-lab global protocol-probe scheduling, fair rotation, restart convergence, and verified teardown acceptance passed"
