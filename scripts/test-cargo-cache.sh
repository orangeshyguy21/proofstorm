#!/usr/bin/env bash
# Exercise cache pruning on fake target directories with a fake cargo process probe.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/scripts" "$scratch/bin"
cp "$root/scripts/cargo-cache.sh" "$fixture/scripts/cargo-cache.sh"
printf '[toolchain]\nchannel = "1.0.0"\n' > "$fixture/rust-toolchain.toml"
# CARGO_RUNNING=1 stands in for another build using the directory.
cat > "$scratch/bin/pgrep" <<'STUB'
#!/bin/sh
[ "${CARGO_RUNNING:-0}" = 1 ]
STUB
chmod +x "$scratch/bin/pgrep"
export PATH="$scratch/bin:$PATH"
limit=$(sed -n 's/^max_units=//p' "$root/scripts/cargo-cache.sh")
target="$fixture/target/check"

fail() {
  printf 'Cargo cache check failed: %s\n' "$1" >&2
  exit 1
}
cache() {
  bash "$fixture/scripts/cargo-cache.sh" "$@"
}
make_target() {
  mkdir -p "$1/debug/.fingerprint/unit" "$1/debug/deps" "$1/debug/incremental" "$1/release/deps"
  printf 'binary\n' > "$1/debug/proofstorm"
}
cached() {
  [[ -d "$1/debug/.fingerprint" && -d "$1/debug/deps" && -d "$1/release/deps" ]]
}
dropped() {
  [[ ! -e "$1/debug/.fingerprint" && ! -e "$1/debug/deps" && ! -e "$1/debug/incremental" ]] &&
    [[ ! -e "$1/release/deps" && -f "$1/debug/proofstorm" ]]
}

if cache > /dev/null 2>&1; then fail 'accepted a missing command'; fi
if cache prune > /dev/null 2>&1; then fail 'accepted prune without directories'; fi

# Directories without cargo state are left alone and never stamped.
mkdir -p "$fixture/plain"
cache prune "$fixture/plain" "$fixture/missing"
[[ ! -e "$fixture/plain/.proofstorm-toolchain" ]] || fail 'stamped a directory without cargo state'

# The first prune adopts an existing cache; an unchanged one stays.
make_target "$target"
cache prune "$target"
cached "$target" || fail 'dropped an unstamped cache'
cmp -s "$fixture/rust-toolchain.toml" "$target/.proofstorm-toolchain" || fail 'did not stamp the toolchain'
cache prune "$target"
cached "$target" || fail 'dropped an unchanged cache'

# A toolchain change waits for a running cargo, then drops all but final binaries.
printf '[toolchain]\nchannel = "2.0.0"\n' > "$fixture/rust-toolchain.toml"
CARGO_RUNNING=1 cache prune "$target"
cached "$target" || fail 'dropped a cache while cargo was running'
if cmp -s "$fixture/rust-toolchain.toml" "$target/.proofstorm-toolchain"; then
  fail 'stamped a skipped prune'
fi
cache prune "$target" 2> "$scratch/stderr"
dropped "$target" || fail 'did not drop a cache from an older toolchain'
grep -q 'the pinned toolchain changed' "$scratch/stderr" || fail 'missing toolchain reason'
cmp -s "$fixture/rust-toolchain.toml" "$target/.proofstorm-toolchain" || fail 'did not restamp the toolchain'
for _ in $(seq 50); do
  compgen -G "$target/.dropped-*" > /dev/null || break
  sleep 0.1
done
if compgen -G "$target/.dropped-*" > /dev/null; then fail 'dropped files were not deleted'; fi

# More units than the limit means most of the cache is stale.
make_target "$target"
(cd "$target/debug/.fingerprint" && seq "$limit" | xargs touch)
cache prune "$target" 2> "$scratch/stderr"
dropped "$target" || fail 'did not drop an oversized cache'
grep -q "$((limit + 1)) build units" "$scratch/stderr" || fail 'missing unit count reason'

# Clean drops every checkout target directory unless cargo is running.
directories=("$fixture/target" "$target" "$fixture/target/one-off" "$fixture/.proofstorm-dev/target")
for dir in "${directories[@]}"; do make_target "$dir"; done
if CARGO_RUNNING=1 cache clean > /dev/null 2>&1; then fail 'cleaned while cargo was running'; fi
for dir in "${directories[@]}"; do cached "$dir" || fail "cleaned $dir while cargo was running"; done
cache clean > /dev/null
for dir in "${directories[@]}"; do dropped "$dir" || fail "clean kept cargo files in $dir"; done
printf 'Cargo cache checks passed\n'
