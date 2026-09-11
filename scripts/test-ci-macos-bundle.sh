#!/usr/bin/env bash
# Native build orchestration contracts; no compiler, sandbox, Docker or service is invoked.
set -Eeuo pipefail
trap 'printf "Mac CI fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/scripts" "$scratch/bin"
cp "$root/scripts/ci-macos-bundle.sh" "$fixture/scripts/"
export MAC_CI_TRACE="$scratch/trace" MAC_CI_HELPER="$scratch/helper"
cat > "$scratch/bin/uname" <<'STUB'
#!/bin/sh
case "$1" in -s) echo Darwin ;; -m) echo arm64 ;; *) exit 97 ;; esac
STUB
cat > "$fixture/scripts/release-build.sh" <<'STUB'
#!/bin/bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${CARGO_TARGET_DIR-unset} == unset ]] || exit 97
[[ $# == 6 && "$1" == --work-dir && "$3" == --output && "$5" == --controller-receipt && -f "$6" ]] || exit 97
echo build >> "$MAC_CI_TRACE"
[[ ${MAC_CI_FAIL:-none} != build ]] || exit 23
mkdir -p "$2/source" "$4"
echo fixture > "$2/source/install.sh"
[[ ${MAC_CI_FAIL:-none} == report ]] || echo '{}' > "$2/result.json"
name=proofstorm-0.1.0-alpha.2-macos-arm64.tar.gz
[[ ${MAC_CI_FAIL:-none} == missing ]] || echo fixture > "$4/$name"
echo fixture > "$4/$name.sha256"
echo private > "$4/.env"
echo unexpected > "$4/foreign.txt"
if [[ ${MAC_CI_FAIL:-none} == duplicate ]]; then echo fixture > "$4/proofstorm-0.1.0-alpha.3-macos-arm64.tar.gz"; fi
if [[ ${MAC_CI_FAIL:-none} == symlink ]]; then
  rm "$2/source/install.sh"
  ln -s "$MAC_CI_TRACE" "$2/source/install.sh"
fi
STUB
cat > "$scratch/bin/cargo" <<'STUB'
#!/bin/bash
set -euo pipefail
[[ "$*" == 'build --quiet --locked -p proofstorm-xtask' ]] || exit 97
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$MAC_CI_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$MAC_CI_HELPER" <<'STUB'
#!/bin/bash
set -euo pipefail
[[ $# == 7 && "$1" == release-smoke && -f "$2" && "$4" == --deny-source && "$6" == --deny-source ]] || exit 97
echo relocation >> "$MAC_CI_TRACE"
[[ ${MAC_CI_FAIL:-none} != relocation ]] || exit 24
mkdir -p "$3"
echo '{}' > "$3/smoke-report.json"
STUB
cat > "$fixture/scripts/macos-install-smoke.sh" <<'STUB'
#!/bin/bash
set -euo pipefail
[[ $# == 8 && "$1" == --archive && -f "$2" && "$3" == --installer && -f "$4" && "$5" == --snapshot && -d "$6" && "$7" == --work-dir ]] || exit 97
echo install >> "$MAC_CI_TRACE"
[[ ${MAC_CI_FAIL:-none} != install ]] || exit 25
mkdir -p "$8"
[[ ${MAC_CI_FAIL:-none} == receipt ]] || echo '{}' > "$8/install-smoke-report.json"
STUB
cat > "$scratch/bin/docker" <<'STUB'
#!/bin/sh
exit 97
STUB
chmod +x "$scratch/bin/"* "$MAC_CI_HELPER"
export PATH="$scratch/bin:$PATH"
echo '{}' > "$scratch/controller receipt.json"
run() {
  : > "$MAC_CI_TRACE"
  PROOFSTORM_HOME=foreign CARGO_TARGET_DIR=foreign bash "$fixture/scripts/ci-macos-bundle.sh" "$@" > "$scratch/output" 2>&1
}
fail() { cat "$scratch/output" >&2; printf '%s\n' "$1" >&2; exit 1; }
run --help
[[ ! -s "$MAC_CI_TRACE" ]]
work="$scratch/build with 'quotes'"
run --work-dir "$work" --controller-receipt "$scratch/controller receipt.json" || fail 'Mac fixture build failed'
[[ $(grep -c '^build$' "$MAC_CI_TRACE") == 1 && $(grep -c '^relocation$' "$MAC_CI_TRACE") == 1 && $(grep -c '^install$' "$MAC_CI_TRACE") == 1 ]]
set -- "$work/bundle/"*
[[ $# == 6 && ! -e "$work/bundle/.env" && ! -e "$work/bundle/foreign.txt" ]]
if run --work-dir "$scratch/no-controller"; then fail 'Accepted missing controller receipt'; fi
if run --work-dir "$fixture/inside" --controller-receipt "$scratch/controller receipt.json"; then fail 'Accepted checkout work directory'; fi
if run --work-dir "$work" --controller-receipt "$scratch/controller receipt.json"; then fail 'Accepted existing work directory'; fi
[[ ! -s "$MAC_CI_TRACE" ]]
for failure in build missing report duplicate symlink relocation install receipt; do
  if MAC_CI_FAIL=$failure run --work-dir "$scratch/fail-$failure" --controller-receipt "$scratch/controller receipt.json"; then fail "Accepted $failure"; fi
  [[ ! -e "$scratch/fail-$failure/bundle" ]] || fail "Collected failed artifacts: $failure"
done
printf 'Mac CI orchestration checks passed\n'
