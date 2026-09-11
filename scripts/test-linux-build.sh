#!/usr/bin/env bash
# Complete host build/install orchestration: real Rust safeguards, fake Docker.
set -Eeuo pipefail
# Exercise hosts with private default permissions, not only CI's usual umask.
umask 077
trap 'printf "Linux build fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
export LINUX_TEST_HELPER=${1:?pass the compiled proofstorm-xtask executable}
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with 'quotes'"
mkdir -p "$fixture/scripts" "$fixture/docker/release" "$scratch/bin" "$scratch/artifacts"
cp "$root/scripts/linux-build.sh" "$root/scripts/ci-linux-bundle.sh" \
  "$root/scripts/linux-install-smoke.sh" "$root/scripts/linux-install-check.sh" "$fixture/scripts/"
printf "[workspace.package]\nversion = '0.1.0-alpha.1'\n" > "$fixture/Cargo.toml"
printf 'FROM fixture\n' > "$fixture/docker/release/Dockerfile.linux-builder"
printf '.env\n' > "$fixture/.gitignore"
printf 'private\n' > "$fixture/.env"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.name=Fixture -c user.email=fixture@example.invalid \
  -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture
export LINUX_TEST_ROOT="$fixture" LINUX_TEST_TRACE="$scratch/trace" LINUX_TEST_STATE="$scratch"
export LINUX_TEST_ARTIFACTS="$scratch/artifacts"
archive="$scratch/artifacts/proofstorm-0.1.0-alpha.1-linux-amd64.tar.gz"
printf 'fake worker archive' > "$archive"
if command -v sha256sum >/dev/null; then digest=$(sha256sum "$archive"); else digest=$(shasum -a 256 "$archive"); fi
printf '%s  %s\n' "${digest%% *}" "${archive##*/}" > "$archive.sha256"
printf '#!/bin/sh\nexit 0\n' > "$scratch/artifacts/install.sh"
printf '{}\n' > "$scratch/artifacts/build-report.json"
printf '{}\n' > "$scratch/artifacts/smoke-report.json"
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]] || exit 97
[[ "$PWD" == "$LINUX_TEST_ROOT" && "$CARGO_TARGET_DIR" != "$LINUX_TEST_ROOT"/* ]] || exit 97
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$LINUX_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$scratch/bin/python3" <<'STUB'
#!/bin/sh
echo 'Python must not be used by Linux build/install orchestration' >&2
exit 97
STUB
cat > "$scratch/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
mode=$1
shift
purpose=build
case " $* " in *proofstorm-linux-install-*) purpose=install ;; esac
event="$purpose:$mode"
if [[ "$mode" == cp ]]; then
  if [[ "$1" == *:/artifacts ]]; then event=build:export; else event=build:transport; fi
fi
printf '%s\n' "$event" >> "$LINUX_TEST_TRACE"
[[ ${LINUX_TEST_FAIL:-none} != "$event" ]] || exit 23
case "$mode" in
  buildx)
    [[ "$1" == build && "$2" == --platform && "$3" == linux/amd64 && "$4" == --load && "$5" == --tag && $# == 7 ]] || exit 97
    [[ -f "$7/Dockerfile" ]] || exit 97
    if [[ "$purpose" == build ]]; then
      [[ "$6" == proofstorm-linux-builder:* && ! -e "$7/input" && ! -e "$7/source" ]] || exit 97
      set -- "$7/"*; [[ $# == 1 ]] || exit 97
    else
      [[ -f "$7/.dockerignore" ]] || exit 97
      set -- "$7/input/"*; [[ $# == 3 ]] || exit 97
    fi ;;
  create)
    [[ "$1" == --name ]] || exit 97
    name=$2
    printf '%s\n' "$name" > "$LINUX_TEST_STATE/$purpose-name"
    shift 2
    if [[ "$purpose" == build ]]; then
      expected=(--platform linux/amd64 --cpus 2 --memory 3g --pids-limit 512 --cap-drop ALL --security-opt no-new-privileges)
    else
      expected=(--platform linux/amd64 --user 1000:1000 --network none --read-only --tmpfs /tmp:rw,exec,nosuid,nodev,size=768m --cpus 2 --memory 1g --pids-limit 128 --cap-drop ALL --security-opt no-new-privileges)
    fi
    for arg in "${expected[@]}"; do [[ "$1" == "$arg" ]] || exit 97; shift; done
    if [[ "$purpose" == build ]]; then
      [[ "$1" == proofstorm-linux-builder:* && "$2" == bash && "$3" == /input/source/scripts/linux-build-worker.sh && $# == 3 ]] || exit 97
    else
      [[ "$1" == "$name:inputs" && "$2" == sh && "$3" == -c && "$5" == install-check && "$7" == false && $# == 7 ]] || exit 97
    fi ;;
  cp)
    name=$(< "$LINUX_TEST_STATE/build-name")
    if [[ "$event" == build:transport ]]; then
      [[ "$2" == "$name:/input" && -f "$1/source.json" && ! -e "$1/source/.env" && ! -e "$1/source/.git" ]] || exit 97
      # Model a different container UID with no DAC override capabilities.
      [[ -z $(find "$1" -type d ! -perm -005 -print -quit) ]] || exit 97
      [[ -z $(find "$1" -type f ! -perm -004 -print -quit) ]] || exit 97
      grep -q "\"debug\": ${LINUX_TEST_DEBUG:-false}" "$1/options.json"
      grep -q "\"development\": ${LINUX_TEST_DEVELOPMENT:-false}" "$1/options.json"
    else
      [[ "$1" == "$name:/artifacts" && ! -e "$2" ]] || exit 97
      cp -R "$LINUX_TEST_ARTIFACTS" "$2"
    fi ;;
  start) [[ "$1" == --attach && "$2" == "$(< "$LINUX_TEST_STATE/$purpose-name")" && $# == 2 ]] || exit 97 ;;
  inspect)
    [[ "$1" == --format && "$2" == '{{.State.ExitCode}}' && "$3" == "$(< "$LINUX_TEST_STATE/$purpose-name")" && $# == 3 ]] || exit 97
    if [[ ${LINUX_TEST_FAIL:-none} == "$purpose:worker" ]]; then printf '9\n'; else printf '0\n'; fi ;;
  logs) [[ "$1" == "$(< "$LINUX_TEST_STATE/$purpose-name")" && $# == 1 ]] || exit 97; printf 'fixture logs\n' ;;
  stop) [[ "$1" == --timeout && "$2" == 10 && "$3" == "$(< "$LINUX_TEST_STATE/$purpose-name")" && $# == 3 ]] || exit 97 ;;
  rm) [[ "$1" == "$(< "$LINUX_TEST_STATE/$purpose-name")" && $# == 1 ]] || exit 97 ;;
  *) exit 97 ;;
esac
STUB
chmod +x "$scratch/bin/"*
run() {
  : > "$LINUX_TEST_TRACE"
  PATH="$scratch/bin:$PATH" PROOFSTORM_HOME=foreign CARGO_BUILD_TARGET=foreign \
    bash "$fixture/scripts/$1" "${@:2}" > "$scratch/stdout" 2> "$scratch/stderr"
}
run linux-build.sh --help
[[ ! -s "$LINUX_TEST_TRACE" ]] || exit 1
if run linux-build.sh --work-dir "$fixture/forbidden"; then exit 1; fi
[[ ! -s "$LINUX_TEST_TRACE" && ! -e "$fixture/forbidden" ]] || exit 1
LINUX_TEST_DEBUG=true LINUX_TEST_DEVELOPMENT=true run linux-build.sh --work-dir "$scratch/development" --debug --development
grep -q 'build:export' "$LINUX_TEST_TRACE"
run ci-linux-bundle.sh --work-dir "$scratch/complete flow"
set -- "$scratch/complete flow/bundle/"*
[[ $# == 6 ]] || exit 1
printf 'build:buildx\nbuild:create\nbuild:transport\nbuild:start\nbuild:inspect\nbuild:export\nbuild:logs\nbuild:stop\nbuild:rm\ninstall:buildx\ninstall:create\ninstall:start\ninstall:inspect\ninstall:logs\ninstall:stop\ninstall:rm\n' > "$scratch/expected"
diff -u "$scratch/expected" "$LINUX_TEST_TRACE"
for failure in build:buildx build:create build:transport build:start build:inspect build:worker build:export install:worker; do
  if LINUX_TEST_FAIL=$failure run ci-linux-bundle.sh --work-dir "$scratch/fail-${failure/:/-}"; then
    printf 'Unexpected success: %s\n' "$failure" >&2; exit 1
  fi
  [[ ! -e "$scratch/fail-${failure/:/-}/bundle" ]] || exit 1
  if [[ "$failure" != install:worker ]]; then
    if grep -q '^install:' "$LINUX_TEST_TRACE"; then exit 1; fi
    case "$failure" in build:buildx|build:create) ;; *) [[ $(tail -n 1 "$LINUX_TEST_TRACE") == build:rm ]] || exit 1 ;; esac
  fi
done
for failure in build:logs build:stop build:rm; do
  LINUX_TEST_FAIL=$failure run linux-build.sh --work-dir "$scratch/cleanup-${failure/:/-}"
  [[ -d "$scratch/cleanup-${failure/:/-}/artifacts" ]] || exit 1
  grep -q '^build:stop$' "$LINUX_TEST_TRACE"
done
printf 'Linux build/install Bash/Rust integration checks passed\n'
