#!/usr/bin/env bash
# Real checksum/report helpers with fake Docker. Never builds or installs a runtime.
set -Eeuo pipefail
trap 'printf "Linux installer fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
export INSTALL_TEST_HELPER=${1:?pass the compiled proofstorm-xtask executable}
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with 'quotes'"
mkdir -p "$fixture/scripts" "$scratch/bin" "$scratch/artifacts"
cp "$root/scripts/linux-install-smoke.sh" "$root/scripts/linux-install-check.sh" "$fixture/scripts/"
export INSTALL_TEST_ROOT="$fixture" INSTALL_TEST_TRACE="$scratch/trace" INSTALL_TEST_NAME="$scratch/name"
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${TRUNK_BUILD_DIST-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]] || exit 97
[[ "$PWD" == "$INSTALL_TEST_ROOT" && "$CARGO_TARGET_DIR" != "$INSTALL_TEST_ROOT"/* ]] || exit 97
[[ " $* " == *' -p proofstorm-xtask '* ]] || exit 97
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$INSTALL_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$scratch/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset ]] || exit 97
mode=$1
shift
printf '%s\n' "$mode" >> "$INSTALL_TEST_TRACE"
[[ ${INSTALL_TEST_FAIL:-none} != "$mode" ]] || exit 23
case "$mode" in
  buildx)
    [[ "$1" == build && "$2" == --platform && "$3" == linux/amd64 && "$4" == --load && "$5" == --tag ]] || exit 97
    [[ "$6" == proofstorm-linux-install-*:inputs ]] || exit 97
    [[ -f "$7/Dockerfile" && -f "$7/.dockerignore" ]] || exit 97
    set -- "$7/input/"*
    [[ $# == 3 ]] || exit 97 ;;
  create)
    [[ "$1" == --name && "$2" == proofstorm-linux-install-* ]] || exit 97
    name=$2
    printf '%s\n' "$name" > "$INSTALL_TEST_NAME"
    shift 2
    expected=(--platform linux/amd64 --user 1000:1000 --network none --read-only
      --tmpfs /tmp:rw,exec,nosuid,nodev,size=768m --cpus 2 --memory 1g --pids-limit 128
      --cap-drop ALL --security-opt no-new-privileges "$name:inputs" sh -c)
    for arg in "${expected[@]}"; do [[ "$1" == "$arg" ]] || exit 97; shift; done
    [[ "$1" == "$(< "$INSTALL_TEST_ROOT/scripts/linux-install-check.sh")" ]] || exit 97
    [[ "$2" == install-check && "$3" == proofstorm-*-linux-amd64.tar.gz && "$4" == "${INSTALL_TEST_DEVELOPMENT:-false}" && $# == 4 ]] || exit 97 ;;
  start) [[ "$1" == --attach && "$2" == "$(< "$INSTALL_TEST_NAME")" && $# == 2 ]] || exit 97 ;;
  inspect)
    [[ "$1" == --format && "$2" == '{{.State.ExitCode}}' && "$3" == "$(< "$INSTALL_TEST_NAME")" && $# == 3 ]] || exit 97
    if [[ ${INSTALL_TEST_FAIL:-none} == worker ]]; then printf '9\n'; else printf '0\n'; fi ;;
  logs) [[ "$1" == "$(< "$INSTALL_TEST_NAME")" && $# == 1 ]] || exit 97; printf 'fixture logs\n' ;;
  stop) [[ "$1" == --timeout && "$2" == 10 && "$3" == "$(< "$INSTALL_TEST_NAME")" && $# == 3 ]] || exit 97 ;;
  rm) [[ "$1" == "$(< "$INSTALL_TEST_NAME")" && $# == 1 ]] || exit 97 ;;
  *) exit 97 ;;
esac
STUB
cat > "$scratch/bin/python3" <<'STUB'
#!/bin/sh
echo 'Python must not be used by the installer smoke path' >&2
exit 97
STUB
chmod +x "$scratch/bin/"*
archive="$scratch/artifacts/proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz"
printf 'fixture archive' > "$archive"
if command -v sha256sum >/dev/null; then digest=$(sha256sum "$archive"); else digest=$(shasum -a 256 "$archive"); fi
printf '%s  %s\n' "${digest%% *}" "${archive##*/}" > "$archive.sha256"
printf '#!/bin/sh\nexit 0\n' > "$scratch/artifacts/install.sh"
run() {
  : > "$INSTALL_TEST_TRACE"
  PATH="$scratch/bin:$PATH" PROOFSTORM_HOME=foreign TRUNK_BUILD_DIST=foreign CARGO_BUILD_TARGET=foreign \
    bash "$fixture/scripts/linux-install-smoke.sh" "$@" > "$scratch/stdout" 2> "$scratch/stderr"
}
check() {
  run --archive "$archive" --installer "$scratch/artifacts/install.sh" --work-dir "$1" "${@:2}"
}
run --help
[[ ! -s "$INSTALL_TEST_TRACE" ]]
if run --work-dir; then exit 1; fi
if check "$fixture/forbidden"; then exit 1; fi
[[ ! -s "$INSTALL_TEST_TRACE" && ! -e "$fixture/forbidden" ]] || exit 1
work="$scratch/normal 'run'"
check "$work"
printf 'buildx\ncreate\nstart\ninspect\nlogs\nstop\nrm\n' > "$scratch/expected"
diff -u "$scratch/expected" "$INSTALL_TEST_TRACE"
grep -q '"local_install": true' "$work/install-smoke-report.json"
grep -q '"development_override": false' "$work/install-smoke-report.json"
grep -q '"runtime_tested": false' "$work/install-smoke-report.json"
grep -q 'fixture logs' "$work/install.log"
if check "$work"; then exit 1; fi
[[ ! -s "$INSTALL_TEST_TRACE" ]] || exit 1
INSTALL_TEST_DEVELOPMENT=true check "$scratch/development" --development
grep -q '"development_override": true' "$scratch/development/install-smoke-report.json"
for failure in buildx create start inspect worker; do
  if INSTALL_TEST_FAIL=$failure check "$scratch/fail-$failure"; then
    printf 'Unexpected success for %s\n' "$failure" >&2; exit 1
  fi
  [[ ! -e "$scratch/fail-$failure/install-smoke-report.json" ]] || exit 1
  case "$failure" in
    buildx|create) if grep -q '^stop$' "$INSTALL_TEST_TRACE"; then exit 1; fi ;;
    *) [[ $(tail -n 1 "$INSTALL_TEST_TRACE") == rm ]] || exit 1 ;;
  esac
done
for failure in logs stop rm; do
  INSTALL_TEST_FAIL=$failure check "$scratch/cleanup-$failure"
  [[ -f "$scratch/cleanup-$failure/install-smoke-report.json" ]] || exit 1
  grep -q '^stop$' "$INSTALL_TEST_TRACE"
  if [[ "$failure" == logs ]]; then [[ $(tail -n 1 "$INSTALL_TEST_TRACE") == rm ]] || exit 1; fi
done
printf 'tampered' > "$archive"
if check "$scratch/tampered"; then exit 1; fi
[[ ! -s "$INSTALL_TEST_TRACE" && ! -e "$scratch/tampered" ]] || exit 1
printf 'Linux installer Bash/Rust checks passed\n'
