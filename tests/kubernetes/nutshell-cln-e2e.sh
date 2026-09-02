#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${ROOT_DIR}/.tools/bin:${PATH}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

cargo build --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -p proofstorm-mcp
python3 "${ROOT_DIR}/tests/kubernetes/nutshell_cln_mcp_client.py" \
  "${ROOT_DIR}/target/debug/proofstorm-mcp" "${TMP_DIR}/proofstorm.sqlite3"

echo "Nutshell 0.20.3 + Core Lightning 26.06.7 REST, restricted rune, wallet round-trip, conservation, and teardown passed"
