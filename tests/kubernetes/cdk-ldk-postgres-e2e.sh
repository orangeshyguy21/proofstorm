#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${ROOT_DIR}/.tools/bin:${PATH}"
export PROOFSTORM_STORAGE=postgres
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

cargo build --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -p proofstorm-mcp
python3 "${ROOT_DIR}/tests/kubernetes/cdk_ldk_mcp_client.py" \
  "${ROOT_DIR}/target/debug/proofstorm-mcp" "${TMP_DIR}/proofstorm.sqlite3"

echo "CDK embedded LDK + PostgreSQL MCP BOLT12 persistence and teardown passed"
