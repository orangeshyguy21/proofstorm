#!/usr/bin/env bash
# Contributor tooling: quick, read-only guard for deliberately retired entry points.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"
retired=(
  compose.yml compose.regtest.yml Makefile.compose infra/k3d/proofstorm.yaml
  scripts/release.py scripts/test_release.py scripts/linux_container.py
  scripts/test_linux_container.py scripts/test_install_script.py
  scripts/controller_release.py scripts/publish_images.py scripts/publish_linux_images.py
  scripts/bootstrap_pins.py scripts/test_installed_setup.py scripts/test_checkout.py
  scripts/test_checkout_controller.py scripts/test_agent_attachments.py
  scripts/test_managed_gui.py scripts/test_cli_progress.py scripts/test_smoke_cleanup.py
  scripts/test-installation-isolation.py scripts/agent-usability-cluster.py
  scripts/run-agent-usability-suite.sh scripts/run-private-handoff-campaign.py
  crates/proofstorm-web/assets/proofstorm-logo-light.svg
)
for path in "${retired[@]}"; do
  if [[ -e "$path" || -L "$path" ]]; then
    printf 'Retired workflow returned: %s. See dev/legacy-consolidation-plan.md.\n' "$path" >&2
    exit 1
  fi
done

# Restrict retired workflow checks to maintained orchestration. Historical
# evidence may still name commands that were used to collect it.
listing=$(git ls-files --cached --others --exclude-standard -- scripts tools .github/workflows justfile)
while IFS= read -r path; do
  [[ -f "$path" ]] || continue
  case "$path" in
    scripts/test-*|scripts/test_*) continue ;;
    *.sh|*.py|*.yml|*.yaml|justfile) ;;
    *) continue ;;
  esac
  if grep -nE 'scripts/(release\.py|linux_container\.py|test_installed_setup\.py|test_checkout\.py)|import (release|linux_container|test_installed_setup|test_checkout)([[:space:]]|$)|docker[[:space:]]+compose|docker-compose|localhost:5111|k3d-proofstorm([^[:alnum:]_-]|$)' "$path"; then
    printf 'Retired workflow reference in %s\n' "$path" >&2
    exit 1
  fi
done <<< "$listing"

# Component implementation languages do not become Proofstorm integration
# runtimes. Keep owned adapters, acceptance fixtures and tooling in Rust/Bash.
# Upstream Nutshell may remain Python; invoking its declared CLI needs no SDK glue.
listing=$(git ls-files --cached --others --exclude-standard -- crates scripts tools docker tests .github/workflows justfile)
while IFS= read -r path; do
  [[ -f "$path" ]] || continue
  case "$path" in
    *.py)
      printf 'Owned Python source is not supported: %s\n' "$path" >&2
      exit 1 ;;
    # These fixtures deliberately prove that installation needs no Python.
    crates/proofstorm-xtask/tests/installer.rs|scripts/test-linux-build.sh|scripts/test-linux-install-smoke.sh|scripts/linux-install-check.sh|tests/component-driver/coco.sh|tests/component-driver/cdk.sh) continue ;;
    *.rs|*.sh|*.yml|*.yaml|*Dockerfile*|justfile) ;;
    *) continue ;;
  esac
  if grep -nE "(^|[[:space:]\"'])(python[0-9.]*|pypy[0-9.]*)($|[[:space:]\"'])|from cashu[.]|import (cashu|grpc|sqlite3)|python:[0-9]" "$path"; then
    printf 'Owned Python execution returned in %s\n' "$path" >&2
    exit 1
  fi
done <<< "$listing"
printf 'Maintained workflow surface checks passed\n'
