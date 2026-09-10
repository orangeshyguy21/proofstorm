#!/usr/bin/env bash
# Exercise dispatch with fake tools in a disposable checkout, never the real runtime.
set -Eeuo pipefail
last_recipe=initialization
report_failure() {
  local status=$1 line=$2
  printf 'Just dispatch check failed at scripts/test-just.sh:%s (recipe: %s, status: %s)\n' "$line" "$last_recipe" "$status" >&2
}
trap 'report_failure "$?" "$LINENO"' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/.tools/bin" "$fixture/.proofstorm-dev/bin" "$fixture/scripts" "$fixture/target/debug"
cp "$root/justfile" "$fixture/justfile"
export TRACE="$scratch/trace"
export PROOFSTORM_HOME=must-not-leak PROOFSTORM_KUBECONFIG=must-not-leak

cat > "$scratch/stub" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '<%s>\n' "${0##*/}" "$PWD" "${PROOFSTORM_HOME-unset}" "${PROOFSTORM_KUBECONFIG-unset}" "$@" >> "$TRACE"
exit "${STUB_EXIT:-0}"
STUB
chmod +x "$scratch/stub"
for tool in rustup sh make cargo; do
  ln -s "$scratch/stub" "$fixture/.tools/bin/$tool"
done
ln -s "$scratch/stub" "$fixture/.proofstorm-dev/bin/proofstorm"
ln -s "$scratch/stub" "$fixture/scripts/check.sh"
ln -s "$scratch/stub" "$fixture/scripts/develop.sh"
ln -s "$scratch/stub" "$fixture/scripts/release-build.sh"
ln -s "$scratch/stub" "$fixture/target/debug/proofstorm-acceptance"

run() {
  last_recipe=${1:-default}
  : > "$TRACE"
  local result
  # Capture output ourselves: --quiet also discards child-command errors.
  if just --justfile "$fixture/justfile" "$@" > "$scratch/just.stdout" 2> "$scratch/just.stderr"; then
    return 0
  else
    result=$?
    # Expected failure cases should not look like a broken check to contributors.
    if [[ ${STUB_EXIT:-0} == 0 ]]; then
      cat "$scratch/just.stdout" "$scratch/just.stderr" >&2
    fi
    return "$result"
  fi
}
expect() {
  printf '<%s>\n' "$@" > "$scratch/expected"
  diff -u "$scratch/expected" "$TRACE"
}

# Default/help is discovery, with no build or runtime command.
run
[[ ! -s "$TRACE" ]]
run help
[[ ! -s "$TRACE" ]]

# Literal arguments survive whitespace, quotes, and shell metacharacters.
tricky="folder with 'quotes'; \$(touch $scratch/INJECTED)"
for recipe in gui serve; do
  run "$recipe" "$tricky" --no-open
  expect proofstorm "$fixture" unset unset gui "$tricky" --no-open
done
[[ ! -e "$scratch/INJECTED" ]]
run doctor --json
expect proofstorm "$fixture" unset unset doctor --json
run stop
expect proofstorm "$fixture" unset unset stop
run check-quick
expect check.sh "$fixture" unset unset quick
run check-rust
expect check.sh "$fixture" unset unset rust
run release-check "$tricky" --alpha --json
expect cargo "$fixture" unset unset run --locked -p proofstorm-xtask -- release-check "$tricky" --alpha --json
run release-verify "$tricky" --json
expect cargo "$fixture" unset unset run --locked -p proofstorm-xtask -- release-verify "$tricky" --json
run release-build --work-dir "$tricky" --output output --debug
expect release-build.sh "$fixture" unset unset --work-dir "$tricky" --output output --debug
for recipe in release-package release-pack release-extract; do
  run "$recipe" "$tricky" destination --json
  expect cargo "$fixture" unset unset run --locked -p proofstorm-xtask -- "$recipe" "$tricky" destination --json
done

# Dependencies run in order; aliases preserve build flags as individual arguments.
for recipe in dev-build build; do
  run "$recipe" --target-dir "$tricky"
  expect sh "$fixture" unset unset tools/install-trunk.sh \
    rustup "$fixture" unset unset target add wasm32-unknown-unknown \
    develop.sh "$fixture" unset unset --target-dir "$tricky"
done
run dev --target-dir "$tricky"
expect sh "$fixture" unset unset tools/install-trunk.sh \
  rustup "$fixture" unset unset target add wasm32-unknown-unknown \
  develop.sh "$fixture" unset unset --shell --target-dir "$tricky"
for recipe in setup deploy; do
  run "$recipe" --json
  expect sh "$fixture" unset unset tools/install-trunk.sh \
    rustup "$fixture" unset unset target add wasm32-unknown-unknown \
    develop.sh "$fixture" unset unset \
    proofstorm "$fixture" unset unset setup --json
done
run web --target-dir "$tricky"
expect sh "$fixture" unset unset tools/install-trunk.sh \
  rustup "$fixture" unset unset target add wasm32-unknown-unknown \
  develop.sh "$fixture" unset unset --web-only --target-dir "$tricky"
run web-dev
expect sh "$fixture" unset unset tools/install-trunk.sh \
  rustup "$fixture" unset unset target add wasm32-unknown-unknown \
  develop.sh "$fixture" unset unset --watch-web
run compose ps
expect make "$fixture" unset unset -f Makefile.compose ps
run e2e slice4 controller-recovery
expect sh "$fixture" unset unset tools/install-trunk.sh \
  rustup "$fixture" unset unset target add wasm32-unknown-unknown \
  develop.sh "$fixture" unset unset --web-only \
  cargo "$fixture" unset unset build --locked -p proofstorm-app -p proofstorm-mcp -p proofstorm-acceptance \
  proofstorm-acceptance "$fixture" unset unset slice4 \
  proofstorm-acceptance "$fixture" unset unset controller-recovery
run e2e
[[ $(grep -c '^<proofstorm-acceptance>$' "$TRACE") == 23 ]]
if grep -Eq '^<(nutshell-oidc|private-handoff)>$' "$TRACE"; then
  printf 'An opt-in gate unexpectedly ran in the default suite\n' >&2
  exit 1
fi

# Exit failures reach the caller and failed dependencies stop the build.
if STUB_EXIT=7 run gui; then
  printf 'Expected command failure to propagate\n' >&2
  exit 1
fi
if STUB_EXIT=7 run dev-build; then
  printf 'Expected prerequisite failure to stop the build\n' >&2
  exit 1
fi
expect sh "$fixture" unset unset tools/install-trunk.sh
printf 'Just dispatch checks passed\n'
