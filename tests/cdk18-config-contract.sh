#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Use the tested rendering's exact image digest and the configured public namespace.
# No fixed local registry, live cluster, compiler, or Compose installation is used.
public_image() {
  local logical namespace
  logical=$(jq -er '[.resources.deployments[].spec.template.spec.initContainers[]?
    | select(.name == "initialize-config") | .image] | unique | if length == 1 then .[0] else error("ambiguous initializer image") end' \
    "${ROOT_DIR}/crates/proofstorm-kube/tests/golden/$1.json") || return 1
  namespace=$(jq -er '.namespace' "${ROOT_DIR}/release/ghcr.json") || return 1
  [[ "$logical" =~ ^proofstorm-registry\.localhost:5000/(cdk-mint-management|cdk-ldk-mint-management)@sha256:[0-9a-f]{64}$ ]] || return 1
  [[ "$namespace" =~ ^ghcr\.io/[a-z0-9_-]+/proofstorm$ ]] || return 1
  printf '%s/%s\n' "$namespace" "${logical#proofstorm-registry.localhost:5000/}"
}
STANDARD_IMAGE=$(public_image cdk)
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
  docker run --rm --network none \
    -v "${config}:/proofstorm-config.toml:ro" \
    -v "${SECRET_FIXTURES}:/mint-secrets:ro" \
    "$@" \
    "${image}" \
    cdk-mintd config validate --file /proofstorm-config.toml
  echo "validated CDK 0.18 configuration: ${label}"
}

for golden in cdk cdk-cln-cell cdk-bdk cdk-postgres-cell; do
  config="${TMP_DIR}/${golden}.toml"
  extract_golden_config "${golden}" "${config}"
  image=$(public_image "$golden")
  if [[ "${golden}" == "cdk-postgres-cell" ]]; then
    validate_config \
      "${golden}" \
      "$image" \
      "${config}" \
      -e 'CDK_MINTD_POSTGRES_URL=postgresql://proofstorm:proofstorm@database:5432/cdk_mint'
  else
    validate_config "${golden}" "$image" "${config}"
  fi
done

config="${TMP_DIR}/cdk-ldk.toml"
extract_golden_config "cdk-ldk" "${config}"
image=$(public_image cdk-ldk)
validate_config "cdk-ldk" "$image" "${config}"

# Exercise the exact rendered initializer against the pinned CDK database,
# including accepted edits, restart retries, and failure without data loss.
jq -er '.resources.deployments[0].spec.template.spec.initContainers[]
  | select(.name == "initialize-config") | .command[2]' \
  "${ROOT_DIR}/crates/proofstorm-kube/tests/golden/cdk.json" > "${TMP_DIR}/initialize.sh"
docker run --rm --network none -i \
  --entrypoint sh \
  --tmpfs /config:rw,mode=1777 \
  --tmpfs /app/data:rw,mode=1777 \
  -v "${TMP_DIR}/cdk.toml:/fixture.toml:ro" \
  -v "${SECRET_FIXTURES}:/mint-secrets:ro" \
  -v "${TMP_DIR}/initialize.sh:/initialize.sh:ro" \
  -e CDK_MINTD_WORK_DIR=/app/data \
  "${STANDARD_IMAGE}" -es <<'SH'
cp /fixture.toml /config/config.toml
sh -e /initialize.sh
printf 'persistent-state\n' > /app/data/sentinel
sed 's/name = "Proofstorm CDK mint"/name = "edited mint"/' /fixture.toml > /config/config.toml
sh -e /initialize.sh
cdk-mintd config show > /tmp/accepted.toml
grep -q 'name = "edited mint"' /tmp/accepted.toml
sh -e /initialize.sh
cdk-mintd config show > /tmp/retried.toml
cmp /tmp/accepted.toml /tmp/retried.toml
printf 'not valid TOML = [\n' > /config/config.toml
if sh -e /initialize.sh; then
    echo 'invalid configuration was accepted' >&2
    exit 1
fi
cdk-mintd config show > /tmp/after-failure.toml
cmp /tmp/accepted.toml /tmp/after-failure.toml
cp /fixture.toml /config/config.toml
sh -e /initialize.sh
cdk-mintd config show | grep -q 'name = "Proofstorm CDK mint"'
grep -q '^persistent-state$' /app/data/sentinel
echo 'CDK initialization, edit, retry, invalid edit and recovery passed'
SH

echo "All generated CDK 0.18 configurations satisfy the pinned upstream binaries"
