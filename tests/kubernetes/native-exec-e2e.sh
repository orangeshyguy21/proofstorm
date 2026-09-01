#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${ROOT_DIR}/.tools/bin:${PATH}"
KUBECTL=(kubectl --context k3d-proofstorm)
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT
export PROOFSTORM_TEST_RUN_ID="$(date +%s)-$$"

cargo build --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -p proofstorm-mcp
python3 "${ROOT_DIR}/tests/kubernetes/native_exec_mcp_client.py" \
  "${ROOT_DIR}/target/debug/proofstorm-mcp" "${TMP_DIR}/proofstorm.sqlite3"

"${KUBECTL[@]}" get configmap -n proofstorm-system \
  -l proofstorm.dev/receipt=teardown -o jsonpath='{.items[0].data.verifiedAbsent}' | grep -Fx true
if "${KUBECTL[@]}" get namespaces -l proofstorm.dev/instance -o name | grep -q .; then
  echo "a Proofstorm instance namespace remains after native-exec close" >&2
  exit 1
fi
if "${KUBECTL[@]}" get proofstormlabactions.proofstorm.dev -n proofstorm-system -o name | grep -q .; then
  echo "a native-exec ProofstormLabAction remains after verified close" >&2
  exit 1
fi

echo "MCP native component execution, bounded artifacts, workload isolation, evidence, and verified close acceptance passed"
