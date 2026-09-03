#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STANDARD_IMAGE="docker.io/cashubtc/mintd@sha256:fd938da187fb9fce82627ced6d419e675dbd6db5f0d50dc6930b1f6e18c359f0"
LDK_IMAGE="docker.io/cashubtc/mintd@sha256:2b0e9ff0430710b5c3df93cfaccdea01ffa2efc6d66c50daca4730f0c542d9be"
SECRET_FIXTURES="${ROOT_DIR}/tests/fixtures/cdk-mint-secrets"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

extract_golden_config() {
  local golden="$1"
  local destination="$2"
  jq -er \
    '.resources.configMaps[] | select(.metadata.name == "mint-config") | .data["config.toml"]' \
    "${ROOT_DIR}/crates/proofstorm-kube/tests/golden/${golden}.json" > "${destination}"
}

validate_config() {
  local label="$1"
  local image="$2"
  local config="$3"
  shift 3
  docker run --rm \
    -v "${config}:/proofstorm-config.toml:ro" \
    -v "${SECRET_FIXTURES}:/mint-secrets:ro" \
    "$@" \
    "${image}" \
    cdk-mintd config validate --file /proofstorm-config.toml
  echo "validated CDK 0.18 configuration: ${label}"
}

for golden in cdk cdk-cln-lab cdk-bdk cdk-postgres-lab; do
  config="${TMP_DIR}/${golden}.toml"
  extract_golden_config "${golden}" "${config}"
  if [[ "${golden}" == "cdk-postgres-lab" ]]; then
    validate_config \
      "${golden}" \
      "${STANDARD_IMAGE}" \
      "${config}" \
      -e 'CDK_MINTD_POSTGRES_URL=postgresql://proofstorm:proofstorm@database:5432/cdk_mint'
  else
    validate_config "${golden}" "${STANDARD_IMAGE}" "${config}"
  fi
done

config="${TMP_DIR}/cdk-ldk.toml"
extract_golden_config "cdk-ldk" "${config}"
validate_config "cdk-ldk" "${LDK_IMAGE}" "${config}"

validate_config \
  "compose fakewallet" \
  "${STANDARD_IMAGE}" \
  "${ROOT_DIR}/docker/mint/mintd.toml" \
  -e 'CDK_MINTD_MNEMONIC=abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about'
validate_config \
  "compose LND regtest" \
  "${STANDARD_IMAGE}" \
  "${ROOT_DIR}/docker/mint/mintd.regtest.toml" \
  -e 'CDK_MINTD_MNEMONIC=abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about'

echo "All generated and Compose CDK 0.18 configurations satisfy the pinned upstream binaries"
