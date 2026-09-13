#!/usr/bin/env bash
# Run the real wrapper and Rust filesystem helper with fake build/runtime commands.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
helper=${1:?pass the compiled proofstorm-xtask executable}
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout's directory"
mkdir -p "$fixture/scripts" "$fixture/.tools/bin" "$fixture/target/maintainer/debug" \
  "$fixture/cache/debug/examples" "$fixture/charts/proofstorm/crds"
cp "$root/scripts/develop.sh" "$fixture/scripts/develop.sh"
cp "$helper" "$fixture/target/maintainer/debug/proofstorm-xtask"
printf 'fixture\n' > "$fixture/Cargo.toml"
printf 'fixture chart\n' > "$fixture/charts/proofstorm/Chart.yaml"
export TRACE="$scratch/trace" FIXTURE="$fixture"

cat > "$scratch/build-stub" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_DB-unset} == unset && ${TRUNK_BUILD_DIST-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]]
[[ ${PROOFSTORM_HOME-unset} == unset && ${PROOFSTORM_KUBECONFIG-unset} == unset ]]
printf '<%s>' "${0##*/}" "$@" >> "$TRACE"
printf '\n' >> "$TRACE"
case "${0##*/}" in
  cargo)
    if [[ " $* " == *' --bins '* && ${FAIL_BUILD:-0} == 1 ]]; then exit 9; fi ;;
  trunk)
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == --dist ]]; then
        mkdir -p "$2"
        printf 'fixture html\n' > "$2/index.html"
        break
      fi
      shift
    done ;;
esac
STUB
chmod +x "$scratch/build-stub"
ln -s "$scratch/build-stub" "$fixture/.tools/bin/cargo"
ln -s "$scratch/build-stub" "$fixture/.tools/bin/trunk"
cat > "$fixture/.tools/bin/git" <<'STUB'
#!/bin/sh
printf 'Cargo.toml\0'
STUB
chmod +x "$fixture/.tools/bin/git"
cat > "$fixture/cache/debug/proofstorm" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == version && "$2" == --json ]]; then printf '{"version":"fixture"}\n'; exit 0; fi
[[ "$1" == --home && "$3" == internal && "$4" == checkout-register ]]
printf '<register>\n' >> "$TRACE"
mkdir -p "$2"
printf '{}\n' > "$2/checkout-artifacts.json"
STUB
chmod +x "$fixture/cache/debug/proofstorm"
cp "$fixture/cache/debug/proofstorm" "$fixture/cache/debug/proofstorm-mcp"
cat > "$fixture/cache/debug/examples/export_crds" <<'STUB'
#!/bin/sh
mkdir -p "$1"
printf 'fixture CRD\n' > "$1/fixture.yaml"
STUB
chmod +x "$fixture/cache/debug/examples/export_crds"

run() {
  : > "$TRACE"
  local status
  if PATH="$fixture/.tools/bin:$PATH" PROOFSTORM_DB=foreign PROOFSTORM_HOME=foreign \
    PROOFSTORM_KUBECONFIG=foreign TRUNK_BUILD_DIST=foreign \
    CARGO_BUILD_TARGET=foreign CARGO_TARGET_DIR=foreign \
    bash "$fixture/scripts/develop.sh" "$@" > "$scratch/output" 2>&1; then
    return 0
  else
    status=$?
    cat "$scratch/output" >&2
    return "$status"
  fi
}
run --help
[[ ! -s "$TRACE" && ! -d "$fixture/.proofstorm-dev" ]]
if run --web-only --watch-web; then exit 1; fi
[[ ! -s "$TRACE" && ! -d "$fixture/.proofstorm-dev" ]]
if run --target-dir=; then exit 1; fi
[[ ! -s "$TRACE" && ! -d "$fixture/.proofstorm-dev" ]]
run --target-dir "$fixture/cache"
grep -q '^<register>$' "$TRACE"
[[ -x "$fixture/.proofstorm-dev/bin/proofstorm" ]]
[[ -f "$fixture/.proofstorm-dev/state/checkout-artifacts.json" ]]

run --web-only
grep -q '^<trunk><build>' "$TRACE"
if grep -Eq '<register>|<--bins>|<export_crds>' "$TRACE"; then exit 1; fi
run --watch-web
grep -q '^<trunk><watch>' "$TRACE"
if grep -Eq '<register>|<--bins>|<export_crds>' "$TRACE"; then exit 1; fi

if FAIL_BUILD=1 run; then printf 'Build failure did not propagate\n' >&2; exit 1; fi
if grep -q '<register>' "$TRACE"; then printf 'Registered a failed build\n' >&2; exit 1; fi

# Recovery must finish before any compiler or asset writer touches the build.
printf '{}\n' > "$fixture/.proofstorm-dev/reset-pending.json"
if run; then printf 'Built during an unfinished reset\n' >&2; exit 1; fi
[[ ! -s "$TRACE" ]]
grep -q 'reset is unfinished' "$scratch/output"
rm "$fixture/.proofstorm-dev/reset-pending.json"

# Existing foreign markers must never be silently adopted.
printf '{"source":"/foreign"}\n' > "$fixture/.proofstorm-dev/owner.json"
if run; then exit 1; fi
if grep -q '<trunk>' "$TRACE"; then exit 1; fi
printf 'Development wrapper checks passed\n'
