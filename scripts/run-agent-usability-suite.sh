#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCHMARK="$ROOT/scripts/run-agent-usability-benchmark.sh"
CATALOG="$ROOT/scripts/agent-usability-scenarios.json"
MODEL="kimi-for-coding/k3"
RUN_PREFIX="dynamic-$(date -u +%Y%m%d%H%M%S)"
SCENARIOS="all"
VARIANT="auto"
REPEAT=1

usage() {
  printf '%s\n' \
    "usage: $0 [--model PROVIDER/MODEL] [--run-prefix ID] [--scenarios all|ID,ID] [--variant N|auto] [--repeat N]" \
    "" \
    "Runs the selected scenario corpus strictly serially. The next fresh" \
    "headless OpenCode session is never started until the prior benchmark" \
    "has exited and verified that no Proofstorm lab, candidate job, or storage remains."
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)
      MODEL="${2:-}"
      shift 2
      ;;
    --run-prefix)
      RUN_PREFIX="${2:-}"
      shift 2
      ;;
    --scenarios)
      SCENARIOS="${2:-}"
      shift 2
      ;;
    --variant)
      VARIANT="${2:-}"
      shift 2
      ;;
    --repeat)
      REPEAT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! "$RUN_PREFIX" =~ ^[a-zA-Z0-9][a-zA-Z0-9._-]{0,39}$ ]]; then
  printf '%s\n' "--run-prefix must be 1-40 safe filename characters" >&2
  exit 2
fi
if [[ ! "$REPEAT" =~ ^[1-9][0-9]*$ || "$REPEAT" -gt 10 ]]; then
  printf '%s\n' "--repeat must be an integer from 1 through 10" >&2
  exit 2
fi

SELECTED_SCENARIOS=()
if [[ "$SCENARIOS" == "all" ]]; then
  while IFS= read -r scenario; do
    SELECTED_SCENARIOS+=("$scenario")
  done < <(jq -r '.scenarios[].id' "$CATALOG")
else
  IFS=',' read -r -a SELECTED_SCENARIOS <<<"$SCENARIOS"
fi

for scenario in "${SELECTED_SCENARIOS[@]}"; do
  jq -e --arg id "$scenario" '.scenarios | any(.id == $id)' "$CATALOG" >/dev/null
  for (( repetition = 1; repetition <= REPEAT; repetition++ )); do
    run_id="$RUN_PREFIX-$scenario-r$repetition"
    "$BENCHMARK" \
      --run-id "$run_id" \
      --scenario "$scenario" \
      --variant "$VARIANT" \
      --model "$MODEL"

    idle="$(jq -r '.verified_idle == true' \
      "$ROOT/dev/agent-usability-runs/$run_id/cluster-after.json")"
    if [[ "$idle" != "true" ]]; then
      printf 'stopping serial suite: cluster idleness is not verified after %s\n' \
        "$run_id" >&2
      exit 1
    fi
  done
done
