#!/usr/bin/env bash
# Fast sequencing fixture; Rust unit tests verify the actual snapshot and relocation contracts.
set -Eeuo pipefail
trap 'printf "Linux worker fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout's directory"
input="$scratch/input with spaces"
mkdir -p "$fixture/scripts" "$scratch/bin" "$input/source/tools"
cp "$root/scripts/linux-build-worker.sh" "$fixture/scripts/"
export WORKER_TEST_TRACE="$scratch/trace" WORKER_TEST_INPUT="$input" WORKER_TEST_ROOT="$fixture"
cat > "$scratch/bin/uname" <<'STUB'
#!/bin/sh
case "$1" in -s) printf '%s\n' "${WORKER_TEST_OS:-Linux}" ;; -m) echo x86_64 ;; *) exit 97 ;; esac
STUB
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]] || exit 97
[[ "$PWD" == "$WORKER_TEST_ROOT" && "$CARGO_TARGET_DIR" != "$WORKER_TEST_ROOT"/* ]] || exit 97
printf 'helper\n' >> "$WORKER_TEST_TRACE"
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$WORKER_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$scratch/helper" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  release-worker-prepare)
    printf 'verify\n' >> "$WORKER_TEST_TRACE"
    [[ ${WORKER_TEST_FAIL:-none} != verify ]] || exit 23
    [[ "$2" == "$WORKER_TEST_INPUT" && ! -e "$3" && ! -e "$4" ]] || exit 97
    mkdir "$3"
    cp -R "$2/source" "$3/source"
    [[ ${WORKER_TEST_CONTROLLER:-false} == false ]] || printf '{}\n' > "$3/controller.json"
    printf '%s\0' "$3" "$4" "${WORKER_TEST_DEVELOPMENT:-false}" "${WORKER_TEST_DEBUG:-false}" ;;
  release-smoke)
    printf 'relocate\n' >> "$WORKER_TEST_TRACE"
    [[ -f "$2" && ! -e "$3" ]] || exit 97
    [[ ${WORKER_TEST_FAIL:-none} != relocate ]] || exit 24
    mkdir "$3"
    printf '{}\n' > "$3/smoke-report.json" ;;
  *) exit 97 ;;
esac
STUB
export WORKER_TEST_HELPER="$scratch/helper"
cat > "$input/source/tools/install-trunk.sh" <<'STUB'
#!/bin/sh
set -eu
printf 'tools\n' >> "$WORKER_TEST_TRACE"
[ "${WORKER_TEST_FAIL:-none}" != tools ] || exit 25
source_dir=$(cd "$(dirname "$0")/.." && pwd -P)
[ "$source_dir" != "$WORKER_TEST_INPUT/source" ] || exit 97
mkdir -p "$source_dir/.tools/bin"
touch "$source_dir/.tools/bin/trunk"
STUB
printf 'fixture installer\n' > "$input/source/install.sh"
printf '{}\n' > "$input/source.json"
cat > "$fixture/scripts/release-build.sh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf 'build\n' >> "$WORKER_TEST_TRACE"
[[ ${WORKER_TEST_FAIL:-none} != build ]] || exit 26
[[ ! -e "$WORKER_TEST_INPUT/source/.tools" ]] || exit 97
output='' work='' trunk='' controller='' development=false debug=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) [[ "$2" == "$WORKER_TEST_INPUT/source" ]] || exit 97; shift 2 ;;
    --provenance) [[ "$2" == "$WORKER_TEST_INPUT/source.json" ]] || exit 97; shift 2 ;;
    --work-dir) work=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    --target-dir) [[ "$2" == "${work%/release-build}/target" ]] || exit 97; shift 2 ;;
    --trunk) trunk=$2; shift 2 ;;
    --controller-receipt) controller=$2; shift 2 ;;
    --development) development=true; shift ;;
    --debug) debug=true; shift ;;
    --json) shift ;;
    *) exit 97 ;;
  esac
done
[[ -f "$trunk" && "$development" == "${WORKER_TEST_DEVELOPMENT:-false}" && "$debug" == "${WORKER_TEST_DEBUG:-false}" ]] || exit 97
if [[ ${WORKER_TEST_CONTROLLER:-false} == true ]]; then
  [[ "$controller" == "${work%/release-build}/controller.json" && -f "$controller" ]] || exit 97
else
  [[ -z "$controller" ]] || exit 97
fi
mkdir -p "$output" "$work"
[[ ${WORKER_TEST_FAIL:-none} == missing ]] || touch "$output/proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz"
if [[ ${WORKER_TEST_FAIL:-none} == duplicate ]]; then touch "$output/proofstorm-extra-linux-amd64.tar.gz"; fi
printf '{"fixture":true}\n'
STUB
chmod +x "$scratch/helper" "$scratch/bin/"*
run() {
  : > "$WORKER_TEST_TRACE"
  PATH="$scratch/bin:$PATH" PROOFSTORM_HOME=foreign CARGO_BUILD_TARGET=foreign \
    bash "$fixture/scripts/linux-build-worker.sh" --input "$input" --work-dir "$scratch/$1-work" --output "$scratch/$1-output" > "$scratch/stdout" 2> "$scratch/stderr"
}
if WORKER_TEST_OS=Darwin run forbidden; then exit 1; fi
[[ ! -s "$WORKER_TEST_TRACE" ]] || exit 1
run normal
printf 'helper\nverify\ntools\nbuild\nrelocate\n' > "$scratch/expected"
diff -u "$scratch/expected" "$WORKER_TEST_TRACE"
[[ ! -e "$input/source/.tools" ]] || exit 1
for name in build-report.json smoke-report.json install.sh; do [[ -s "$scratch/normal-output/$name" ]] || exit 1; done
WORKER_TEST_DEBUG=true run debug
WORKER_TEST_CONTROLLER=true run controller
WORKER_TEST_DEVELOPMENT=true WORKER_TEST_DEBUG=true run development
for failure in verify tools build relocate missing duplicate; do
  if WORKER_TEST_FAIL=$failure run "$failure"; then printf 'Unexpected success: %s\n' "$failure" >&2; exit 1; fi
  [[ ! -e "$scratch/$failure-output/build-report.json" && ! -e "$scratch/$failure-output/smoke-report.json" ]] || exit 1
  case "$failure" in verify) [[ $(tail -n 1 "$WORKER_TEST_TRACE") == verify ]] || exit 1 ;;
    tools) [[ $(tail -n 1 "$WORKER_TEST_TRACE") == tools ]] || exit 1 ;; esac
done
printf 'Linux build worker sequencing checks passed\n'
