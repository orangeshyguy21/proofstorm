#!/usr/bin/env bash
# proofstorm attack runner (SPEC.md Phase 6).
# Usage: run-attack.sh [scenario] [MINT=cdk|nutshell]
#   scenario: a name under scenarios/ (without .sh), or "all" (default).
# The regtest stack (compose.regtest.yml) must be up and funded first:
#   make regtest-up && make regtest-fund
set -euo pipefail

PROOFSTORM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCEN_DIR="${PROOFSTORM_ROOT}/scenarios"

log() { printf '[run-attack] %s\n' "$*"; }
die() { printf '[run-attack] error: %s\n' "$*" >&2; exit 1; }

# "Built" scenarios with automated oracles (see SPEC.md §3/§4).
BUILT=(double-spend-replay double-spend-race mint-quote-flood)

target="${1:-all}"
export MINT="${MINT:-cdk}"

run_one() {
  local name="$1" script="${SCEN_DIR}/$1.sh"
  [[ -f "$script" ]] || die "no scenario '${name}' (${script} not found)"
  printf '\n============================================================\n'
  printf '  ATTACK: %s   (MINT=%s)\n' "${name}" "${MINT}"
  printf '============================================================\n'
  bash "$script"
}

if [[ "$target" == "all" ]]; then
  log "running all built scenarios against MINT=${MINT}"
  failed=0
  for s in "${BUILT[@]}"; do
    if ! run_one "$s"; then
      log "scenario ${s} FAILED"
      failed=$((failed + 1))
    fi
  done
  (( failed == 0 )) || die "${failed} scenario(s) failed"
  log "all attack scenarios passed — mint upheld every oracle"
else
  run_one "$target"
fi
