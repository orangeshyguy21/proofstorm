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

# Restrict this to maintained orchestration. Test fixtures may mention rejected
# old commands; product drivers and historical docs are not a language blacklist.
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
printf 'Maintained workflow surface checks passed\n'
