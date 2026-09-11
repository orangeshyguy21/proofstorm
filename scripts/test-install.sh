#!/usr/bin/env bash
# Real installer and archives with fake HTTP transport; no network or product runtime.
set -Eeuo pipefail
trap 'printf "Installer fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/server" "$scratch/payload/proofstorm/bin"
export INSTALL_TEST_SERVER="$scratch/server" INSTALL_TEST_LOG="$scratch/downloads" INSTALL_TEST_MARKER="$scratch/installed"
cat > "$scratch/payload/proofstorm/bin/proofstorm" <<'STUB'
#!/bin/sh
[ "$1" = install-bundle ] || exit 97
printf '%s\n' "$@" > "$INSTALL_TEST_MARKER"
STUB
chmod 755 "$scratch/payload/proofstorm/bin/proofstorm"
for platform in linux-amd64 macos-arm64 x86_64-unknown-linux-gnu aarch64-apple-darwin; do
  archive="$scratch/server/proofstorm-0.1.0-alpha.1-$platform.tar.gz"
  tar -czf "$archive" -C "$scratch/payload" proofstorm/bin/proofstorm
  if command -v sha256sum >/dev/null; then digest=$(sha256sum "$archive"); else digest=$(shasum -a 256 "$archive"); fi
  printf '%s  %s\n' "${digest%% *}" "${archive##*/}" > "$archive.sha256"
done
cat > "$scratch/bin/uname" <<'STUB'
#!/bin/sh
case "$1" in -s) echo "$INSTALL_TEST_OS" ;; -m) echo "$INSTALL_TEST_ARCH" ;; *) exit 97 ;; esac
STUB
cat > "$scratch/bin/curl" <<'STUB'
#!/bin/bash
set -euo pipefail
url='' output='' protocols=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    https://*) url=$1; shift ;;
    --output) output=$2; shift 2 ;;
    --proto|--proto-redir) [[ "$2" == '=https' ]] || exit 97; protocols=$((protocols+1)); shift 2 ;;
    --write-out) [[ "$2" == '%{http_code}' ]] || exit 97; shift 2 ;;
    --retry) shift 2 ;;
    --fail|--location|--silent|--show-error) shift ;;
    *) exit 97 ;;
  esac
done
[[ "$protocols" == 2 && -n "$output" && "$url" == https://github.com/orangeshyguy21/proofstorm/releases/download/v0.1.0-alpha.1/* ]] || exit 97
name=${url##*/}
printf '%s\n' "$name" >> "$INSTALL_TEST_LOG"
case "${INSTALL_TEST_MODE:-friendly}" in
  network) printf 000; exit 6 ;;
  auth) printf 403; exit 22 ;;
  legacy) case "$name" in *-linux-amd64.tar.gz|*-macos-arm64.tar.gz) printf 404; exit 22 ;; esac ;;
  missing-checksum) case "$name" in *.sha256) printf 404; exit 22 ;; esac ;;
  corrupt) case "$name" in *.tar.gz) echo corrupt > "$output"; printf 200; exit 0 ;; esac ;;
esac
[[ -f "$INSTALL_TEST_SERVER/$name" ]] || { printf 404; exit 22; }
cp "$INSTALL_TEST_SERVER/$name" "$output"
printf 200
STUB
chmod +x "$scratch/bin/"*
export PATH="$scratch/bin:$PATH"
run() {
  : > "$INSTALL_TEST_LOG"
  : > "$INSTALL_TEST_MARKER"
  sh "$root/install.sh" --version 0.1.0-alpha.1 --prefix "$scratch/new prefix" "$@" > "$scratch/output" 2>&1
}
fail() { cat "$scratch/output" >&2; printf '%s\n' "$1" >&2; exit 1; }
for system in Linux Darwin; do
  export INSTALL_TEST_OS=$system
  case "$system" in
    Linux) export INSTALL_TEST_ARCH=x86_64; platform=linux-amd64; legacy=x86_64-unknown-linux-gnu ;;
    Darwin) export INSTALL_TEST_ARCH=arm64; platform=macos-arm64; legacy=aarch64-apple-darwin ;;
  esac
  run || fail 'Friendly download failed'
  [[ $(wc -l < "$INSTALL_TEST_LOG" | tr -d ' ') == 2 && -s "$INSTALL_TEST_MARKER" ]]
  grep -Fxq "proofstorm-0.1.0-alpha.1-$platform.tar.gz" "$INSTALL_TEST_LOG"
  if grep -q -- --allow-development "$INSTALL_TEST_MARKER"; then fail 'Public install used a development override'; fi
  INSTALL_TEST_MODE=legacy run || fail 'Legacy download fallback failed'
  [[ $(wc -l < "$INSTALL_TEST_LOG" | tr -d ' ') == 3 && -s "$INSTALL_TEST_MARKER" ]]
  grep -Fxq "proofstorm-0.1.0-alpha.1-$legacy.tar.gz.sha256" "$INSTALL_TEST_LOG"
  run --artifact-dir "$scratch/server" || fail 'Local friendly install failed'
  [[ ! -s "$INSTALL_TEST_LOG" && -s "$INSTALL_TEST_MARKER" ]]
  mkdir "$scratch/legacy-$system"
  cp "$scratch/server/proofstorm-0.1.0-alpha.1-$legacy.tar.gz"* "$scratch/legacy-$system/"
  run --artifact-dir "$scratch/legacy-$system" || fail 'Local legacy install failed'
  for mode in network auth missing-checksum corrupt; do
    if INSTALL_TEST_MODE=$mode run; then fail "Accepted $mode"; fi
    [[ ! -s "$INSTALL_TEST_MARKER" ]]
    if grep -q -- "$legacy" "$INSTALL_TEST_LOG"; then fail "Unexpected fallback for $mode"; fi
  done
  if run --archive proofstorm-explicit-missing.tar.gz; then fail 'Accepted missing explicit archive'; fi
  [[ $(wc -l < "$INSTALL_TEST_LOG" | tr -d ' ') == 1 && ! -s "$INSTALL_TEST_MARKER" ]]
done
printf 'Installer naming and legacy download checks passed\n'
