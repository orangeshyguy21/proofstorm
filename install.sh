#!/bin/sh
# Installs prebuilt artifacts only. No Rust, Python, Docker, or source checkout needed.
set -eu
umask 077

fail() { printf 'proofstorm install: %s\n' "$*" >&2; exit 1; }
usage() {
  printf '%s\n' 'Usage: sh install.sh [--prefix ABSOLUTE_PATH] [--version VERSION]' \
    'Local testing: --artifact-dir DIRECTORY [--archive NAME.tar.gz] --allow-development'
}

install_prefix="${HOME:?HOME is required}/.local"
install_version="0.1.0-alpha.3"
artifact_dir=""
archive_name=""
allow_development=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix|--version|--artifact-dir|--archive)
      [ "$#" -ge 2 ] || fail "$1 needs a value"
      case "$1" in
        --prefix) install_prefix=$2 ;;
        --version) install_version=$2 ;;
        --artifact-dir) artifact_dir=$2 ;;
        --archive) archive_name=$2 ;;
      esac
      shift 2 ;;
    --allow-development) allow_development=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) fail "unknown option: $1" ;;
  esac
done
case "$install_prefix" in /*) ;; *) fail '--prefix must be an absolute path' ;; esac
case "$install_version" in ''|*[!A-Za-z0-9.+-]*) fail 'invalid version' ;; esac
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) install_platform=macos-arm64; install_target=aarch64-apple-darwin ;;
  Linux-x86_64|Linux-amd64) install_platform=linux-amd64; install_target=x86_64-unknown-linux-gnu ;;
  *) fail 'this alpha supports macOS Apple Silicon and Linux x86-64' ;;
esac
legacy_archive=""
if [ -z "$archive_name" ]; then
  archive_name="proofstorm-$install_version-$install_platform.tar.gz"
  legacy_archive="proofstorm-$install_version-$install_target.tar.gz"
fi
case "$archive_name" in *[!A-Za-z0-9._+-]*|'') fail 'invalid archive name' ;; esac
case "$archive_name" in proofstorm-*.tar.gz) ;; *) fail 'expected a Proofstorm .tar.gz archive' ;; esac
if [ "$allow_development" = true ]; then
  [ -n "$artifact_dir" ] || fail '--allow-development is restricted to --artifact-dir local tests'
fi

# Only this freshly created directory is cleaned up; installed versions are retained.
install_scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-install.XXXXXXXX")
trap 'rm -rf -- "$install_scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
if [ -n "$artifact_dir" ]; then
  if [ -n "$legacy_archive" ] && [ ! -e "$artifact_dir/$archive_name" ] && [ ! -L "$artifact_dir/$archive_name" ]; then
    archive_name=$legacy_archive
  fi
  [ -f "$artifact_dir/$archive_name" ] || fail 'local archive is missing'
  cp "$artifact_dir/$archive_name" "$install_scratch/archive.tar.gz"
  cp "$artifact_dir/$archive_name.sha256" "$install_scratch/checksum"
else
  release_url="https://github.com/orangeshyguy21/proofstorm/releases/download/v$install_version"
  printf 'Downloading Proofstorm %s…\n' "$install_version"
  download() {
    download_status=0
    download_http=$(curl --fail --location --retry 3 --silent --show-error --proto '=https' --proto-redir '=https' \
      "$release_url/$1" --output "$install_scratch/$2" --write-out '%{http_code}' 2> "$install_scratch/download-error") || download_status=$?
    [ "$download_status" -eq 0 ] && [ "$download_http" = 200 ]
  }
  download_failed() {
    cat "$install_scratch/download-error" >&2
    fail 'release download failed; nothing was installed'
  }
  if ! download "$archive_name" archive.tar.gz; then
    # Old public releases retain their original assets. Only a missing automatic
    # archive selection may fall back; auth/network/checksum failures never do.
    if [ -n "$legacy_archive" ] && [ "$download_status" -eq 22 ] && [ "$download_http" = 404 ]; then
      archive_name=$legacy_archive
      download "$archive_name" archive.tar.gz || download_failed
    else
      download_failed
    fi
  fi
  download "$archive_name.sha256" checksum || download_failed
fi
expected=$(awk -v name="$archive_name" 'NF == 2 && $2 == name { if (++count == 1) digest=$1 } END { if (NR == 1 && count == 1) print digest; else exit 1 }' "$install_scratch/checksum") || fail 'invalid checksum receipt'
[ "${#expected}" = 64 ] || fail 'invalid checksum length'
case "$expected" in *[!0-9a-f]*) fail 'invalid checksum' ;; esac
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$install_scratch/archive.tar.gz" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$install_scratch/archive.tar.gz" | awk '{print $1}')
else
  fail 'install sha256sum or shasum to verify the download'
fi
[ "$actual" = "$expected" ] || fail 'archive checksum mismatch; nothing was installed'

# Published archives contain regular files only. Refuse links, traversal, and duplicates.
tar -tzf "$install_scratch/archive.tar.gz" > "$install_scratch/names" || fail 'invalid archive'
awk '
  !/^proofstorm\/[A-Za-z0-9_.\/-]+$/ { exit 1 }
  /(^|\/)\.\.?($|\/)/ || /\/\// { exit 1 }
  seen[$0]++ { exit 1 }
  END { if (NR == 0) exit 1 }
' "$install_scratch/names" || fail 'unsafe archive paths'
LC_ALL=C tar -tvzf "$install_scratch/archive.tar.gz" > "$install_scratch/types" || fail 'invalid archive metadata'
awk 'substr($0,1,1) != "-" { exit 1 }' "$install_scratch/types" || fail 'archive links and special files are refused'
mkdir "$install_scratch/unpacked"
# Preserve the verified payload modes despite our private scratch-directory umask.
tar -xpzf "$install_scratch/archive.tar.gz" -C "$install_scratch/unpacked"
bundle="$install_scratch/unpacked/proofstorm"
[ -x "$bundle/bin/proofstorm" ] || fail 'bundle executable missing'
set -- "$bundle/bin/proofstorm" install-bundle --bundle "$bundle" --prefix "$install_prefix"
if [ "$allow_development" = true ]; then set -- "$@" --allow-development; fi
"$@" > "$install_scratch/install-result.json"
printf '\nInstalled. Check it with: "%s/bin/proofstorm" --version\n' "$install_prefix"
case ":${PATH:-}:" in *":$install_prefix/bin:"*) ;; *) printf 'PATH was not changed. Add %s/bin to PATH when ready.\n' "$install_prefix" ;; esac
printf '%s\n' 'No cluster, shell profile, or agent configuration was changed.'
