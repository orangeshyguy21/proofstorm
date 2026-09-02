#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${ROOT_DIR}/.tools/bin:${PATH}"
TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf -- "${TMP_DIR}"
}
trap cleanup EXIT

cargo build --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -p proofstorm-mcp

python3 "${ROOT_DIR}/tests/kubernetes/nutshell_oidc_mcp_client.py" \
  "${ROOT_DIR}/target/debug/proofstorm-mcp" \
  "${TMP_DIR}/proofstorm.sqlite3"

echo "Nutshell 0.20.2 + Keycloak 25.0.6 passed NUT-21/NUT-22 positive and negative limits, replay persistence, restart recovery, and teardown"
