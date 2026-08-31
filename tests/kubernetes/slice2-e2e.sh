#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KUBECTL=(kubectl --context k3d-proofstorm)
CONTROL_NAMESPACE=proofstorm-system
LAB_NAME=slice2-security-spine
INSTANCE_NAMESPACE=proofstorm-01slice2lab00

"${KUBECTL[@]}" apply -f "${ROOT_DIR}/examples/slice2-lab.yaml"
"${KUBECTL[@]}" wait --for=jsonpath='{.status.phase}'=Ready \
  "proofstormlab/${LAB_NAME}" -n "${CONTROL_NAMESPACE}" --timeout=60s

actual_namespace="$("${KUBECTL[@]}" get "proofstormlab/${LAB_NAME}" \
  -n "${CONTROL_NAMESPACE}" -o jsonpath='{.status.instanceNamespace}')"
test "${actual_namespace}" = "${INSTANCE_NAMESPACE}"

if "${KUBECTL[@]}" apply -f "${ROOT_DIR}/tests/kubernetes/privileged-pod.yaml"; then
  echo "restricted Pod Security unexpectedly admitted a privileged pod" >&2
  exit 1
fi

"${KUBECTL[@]}" delete pod/network-server pod/network-client \
  -n "${INSTANCE_NAMESPACE}" --ignore-not-found --wait=true
"${KUBECTL[@]}" apply -f "${ROOT_DIR}/tests/kubernetes/network-pods.yaml"
"${KUBECTL[@]}" wait --for=condition=Ready pod/network-server pod/network-client \
  -n "${INSTANCE_NAMESPACE}" --timeout=90s
"${KUBECTL[@]}" exec -n "${INSTANCE_NAMESPACE}" network-server -- \
  wget -T 3 -qO- http://127.0.0.1:8080 | grep -Fx reachable
server_ip="$("${KUBECTL[@]}" get pod/network-server -n "${INSTANCE_NAMESPACE}" -o jsonpath='{.status.podIP}')"
if "${KUBECTL[@]}" exec -n "${INSTANCE_NAMESPACE}" network-client -- \
  wget -T 3 -qO- "http://${server_ip}:8080"; then
  echo "default-deny NetworkPolicy unexpectedly allowed pod traffic" >&2
  exit 1
fi

"${KUBECTL[@]}" delete limitrange/proofstorm-container-limits -n "${INSTANCE_NAMESPACE}"
"${KUBECTL[@]}" rollout restart deployment/proofstormd -n "${CONTROL_NAMESPACE}"
"${KUBECTL[@]}" rollout status deployment/proofstormd -n "${CONTROL_NAMESPACE}" --timeout=90s
for _ in {1..30}; do
  if "${KUBECTL[@]}" get limitrange/proofstorm-container-limits -n "${INSTANCE_NAMESPACE}" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
"${KUBECTL[@]}" get limitrange/proofstorm-container-limits -n "${INSTANCE_NAMESPACE}" >/dev/null
restarted_namespace="$("${KUBECTL[@]}" get "proofstormlab/${LAB_NAME}" \
  -n "${CONTROL_NAMESPACE}" -o jsonpath='{.status.instanceNamespace}')"
test "${restarted_namespace}" = "${INSTANCE_NAMESPACE}"

"${KUBECTL[@]}" apply -f "${ROOT_DIR}/tests/kubernetes/cleanup-blocker.yaml"
"${KUBECTL[@]}" delete "proofstormlab/${LAB_NAME}" -n "${CONTROL_NAMESPACE}" --wait=false
sleep 5
"${KUBECTL[@]}" get "proofstormlab/${LAB_NAME}" -n "${CONTROL_NAMESPACE}" >/dev/null
test "$("${KUBECTL[@]}" get "proofstormlab/${LAB_NAME}" -n "${CONTROL_NAMESPACE}" -o jsonpath='{.status.phase}')" = Closing
"${KUBECTL[@]}" get namespace "${INSTANCE_NAMESPACE}" >/dev/null

"${KUBECTL[@]}" patch configmap/cleanup-blocker -n "${INSTANCE_NAMESPACE}" \
  --type=merge -p '{"metadata":{"finalizers":null}}'
"${KUBECTL[@]}" wait --for=delete namespace/"${INSTANCE_NAMESPACE}" --timeout=90s
"${KUBECTL[@]}" wait --for=delete "proofstormlab/${LAB_NAME}" \
  -n "${CONTROL_NAMESPACE}" --timeout=90s
"${KUBECTL[@]}" get "configmap/proofstorm-teardown-01slice2lab00" \
  -n "${CONTROL_NAMESPACE}" -o jsonpath='{.data.verifiedAbsent}' | grep -Fx true

echo "Slice 2 live security and lifecycle acceptance passed"
