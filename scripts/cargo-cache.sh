#!/usr/bin/env bash
# Bound checkout cargo target directories. Cargo never deletes files left by older
# toolchains, dependency versions or feature sets, so its caches grow without limit.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)

# A fresh `just check` leaves about 900 build units (8 GB) in target/check.
max_units=2000

usage() {
  printf '%s\n' 'Usage: bash scripts/cargo-cache.sh prune TARGET_DIRECTORY... | clean' >&2
  exit 2
}

cargo_running() {
  pgrep -x cargo > /dev/null
}

# Move cargo's intermediate files aside, then delete them in the background:
# millions of files take minutes. Final binaries stay where they are, so
# registered launchers and running programs work until the next build.
drop() {
  local dir=$1 trash profile part
  trash="$dir/.dropped-$(date +%s)-$$"
  mkdir "$trash"
  for profile in debug release; do
    for part in .fingerprint build deps examples incremental; do
      if [[ -e "$dir/$profile/$part" ]]; then
        mv -- "$dir/$profile/$part" "$trash/$profile-$part"
      fi
    done
  done
  # Also finish deletions that an earlier run did not complete.
  nohup rm -rf -- "$dir"/.dropped-* < /dev/null > /dev/null 2>&1 &
}

# Drop when the pinned toolchain changed since the last prune, or the directory
# holds several fresh builds' worth of units. A running cargo may be using it.
prune() {
  local dir=$1 stamp="$1/.proofstorm-toolchain" units reason=
  [[ -d "$dir/debug/.fingerprint" ]] || return 0
  units=$(find "$dir/debug/.fingerprint" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')
  if [[ -f "$stamp" ]] && ! cmp -s "$root/rust-toolchain.toml" "$stamp"; then
    reason='the pinned toolchain changed'
  elif [[ "$units" -gt "$max_units" ]]; then
    reason="$units build units"
  fi
  if [[ -n "$reason" ]]; then
    if cargo_running; then return 0; fi
    printf 'Dropping stale cargo build files in %s (%s)\n' "$dir" "$reason" >&2
    drop "$dir"
  fi
  cp -- "$root/rust-toolchain.toml" "$stamp"
}

clean() {
  local dir
  if cargo_running; then
    printf 'Cargo is running; run just clean-cache again after it finishes\n' >&2
    exit 1
  fi
  for dir in "$root/target" "$root"/target/*/ "$root/.proofstorm-dev/target"; do
    dir=${dir%/}
    [[ -d "$dir/debug" || -d "$dir/release" ]] || continue
    drop "$dir"
    printf 'Dropped cargo build files in %s\n' "$dir"
  done
  printf 'Deletion continues in the background. The next build of each directory starts cold.\n'
}

case ${1:-} in
  prune)
    shift
    [[ $# -gt 0 ]] || usage
    for dir in "$@"; do prune "$dir"; done ;;
  clean)
    [[ $# -eq 1 ]] || usage
    clean ;;
  *) usage ;;
esac
