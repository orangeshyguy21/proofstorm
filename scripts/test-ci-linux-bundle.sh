#!/usr/bin/env bash
# Check CI sequencing and artifact selection without Python or Docker.
set -Eeuo pipefail
trap 'printf "Linux CI fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/scripts" "$scratch/bin"
cp "$root/scripts/ci-linux-bundle.sh" "$fixture/scripts/"
export TEST_CI_TRACE="$scratch/trace"
export TEST_CI_ADAPTER="$scratch/adapter"
cat > "$TEST_CI_ADAPTER" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${CARGO_TARGET_DIR-unset} == unset ]] || exit 97
mode=$1
shift
printf '<%s>' "$mode" "$@" >> "$TEST_CI_TRACE"
printf '\n' >> "$TEST_CI_TRACE"
work='' archive='' installer=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-dir) work=$2; shift 2 ;;
    --archive) archive=$2; shift 2 ;;
    --installer) installer=$2; shift 2 ;;
    --controller-receipt) [[ "$mode" == build && "$2" == "$TEST_CI_CONTROLLER" ]] || exit 97; shift 2 ;;
    --debug) shift ;;
    *) exit 97 ;;
  esac
done
mkdir -p "$work"
if [[ "$mode" == build ]]; then
  printf 'fixture build log\n'
  [[ ${TEST_CI_FAILURE:-none} != build ]] || exit 23
  mkdir -p "$work/artifacts"
  for file in proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz \
    proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz.sha256 \
    install.sh build-report.json smoke-report.json .env unexpected.txt; do
    [[ ${TEST_CI_FAILURE:-none} != missing || "$file" != *.tar.gz ]] || continue
    [[ ${TEST_CI_FAILURE:-none} != report || "$file" != build-report.json ]] || continue
    if [[ ${TEST_CI_FAILURE:-none} == symlink && "$file" == install.sh ]]; then
      ln -s "$TEST_CI_TRACE" "$work/artifacts/$file"
    else
      printf 'fixture\n' > "$work/artifacts/$file"
    fi
  done
  if [[ ${TEST_CI_FAILURE:-none} == duplicate ]]; then
    printf 'second\n' > "$work/artifacts/proofstorm-0.1.0-alpha.2-linux-amd64.tar.gz"
  fi
else
  [[ "$mode" == smoke && -f "$archive" && -f "$installer" ]] || exit 97
  printf 'fixture install log\n'
  [[ ${TEST_CI_FAILURE:-none} != smoke ]] || exit 24
  if [[ ${TEST_CI_FAILURE:-none} != receipt ]]; then
    printf '{}\n' > "$work/install-smoke-report.json"
  fi
fi
STUB
cat > "$scratch/bin/docker" <<'STUB'
#!/bin/sh
# The adapter is stubbed too, so no Docker invocation should reach here.
exit 97
STUB
chmod +x "$TEST_CI_ADAPTER" "$scratch/bin/docker"
# Reuse the staged-output fixture behind the new Bash installer entrypoint.
cat > "$fixture/scripts/linux-install-smoke.sh" <<'STUB'
#!/usr/bin/env bash
exec "$TEST_CI_ADAPTER" smoke "$@"
STUB
cat > "$fixture/scripts/linux-build.sh" <<'STUB'
#!/usr/bin/env bash
exec "$TEST_CI_ADAPTER" build "$@"
STUB
run() {
  : > "$TEST_CI_TRACE"
  PATH="$scratch/bin:$PATH" PROOFSTORM_HOME=foreign CARGO_TARGET_DIR=foreign \
    bash "$fixture/scripts/ci-linux-bundle.sh" "$@" > "$scratch/stdout" 2> "$scratch/stderr"
}
run --help
[[ ! -s "$TEST_CI_TRACE" ]]
export TEST_CI_CONTROLLER="$scratch/controller with 'quotes'.json"
printf '{}\n' > "$TEST_CI_CONTROLLER"
run --work-dir "$scratch/controller-build" --controller-receipt "$TEST_CI_CONTROLLER"
grep -Fq "<--controller-receipt><$TEST_CI_CONTROLLER>" "$TEST_CI_TRACE"
: > "$TEST_CI_TRACE"
if run --work-dir; then exit 1; fi
if run --work-dir "$fixture/forbidden"; then exit 1; fi
mkdir "$scratch/existing"
printf 'keep\n' > "$scratch/existing/sentinel"
if run --work-dir "$scratch/existing"; then exit 1; fi
ln -s "$scratch/existing" "$scratch/linked"
if run --work-dir "$scratch/linked"; then exit 1; fi
[[ ! -s "$TEST_CI_TRACE" && -f "$scratch/existing/sentinel" ]] || exit 1

work="$scratch/build with 'quotes'"
run --work-dir "$work"
[[ $(grep -c '^<build>' "$TEST_CI_TRACE") == 1 && $(grep -c '^<smoke>' "$TEST_CI_TRACE") == 1 ]] || exit 1
if grep -q -- '<--debug>\|<--development>' "$TEST_CI_TRACE"; then exit 1; fi
[[ -s "$work/build.log" && -s "$work/install.log" ]] || exit 1
set -- "$work/bundle/"*
[[ $# == 6 && ! -e "$work/bundle/.env" && ! -e "$work/bundle/unexpected.txt" ]] || exit 1
grep -q 'Runtime setup remains untested' "$scratch/stdout"
run --work-dir "$scratch/debug" --debug
grep -q '<--debug>' "$TEST_CI_TRACE"

for failure in build missing duplicate report symlink smoke receipt; do
  if TEST_CI_FAILURE=$failure run --work-dir "$scratch/fail-$failure"; then
    printf 'Unexpected success: %s\n' "$failure" >&2; exit 1
  else
    status=$?
  fi
  case "$failure" in build) [[ "$status" == 23 ]] ;; smoke) [[ "$status" == 24 ]] ;; esac
  if [[ -e "$scratch/fail-$failure/bundle" ]]; then
    printf 'Failed check collected uploadable artifacts: %s\n' "$failure" >&2; exit 1
  fi
  case "$failure" in
    smoke|receipt) [[ -s "$scratch/fail-$failure/install.log" ]] ;;
    *) if grep -q '^<smoke>' "$TEST_CI_TRACE"; then exit 1; fi ;;
  esac
done
printf 'Linux CI orchestration checks passed\n'
