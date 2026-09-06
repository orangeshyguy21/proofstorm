#!/bin/sh
# Install the pinned, checksum-verified Trunk release into this checkout.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/tools/versions.env"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  *) echo 'No pinned Trunk binary for this platform.' >&2; exit 1 ;;
esac
mkdir -p "$root/.tools/bin" "$root/.tools/downloads"
if [ -x "$root/.tools/bin/trunk" ] && [ "$(NO_COLOR=true "$root/.tools/bin/trunk" --version)" = "trunk $TRUNK_VERSION" ]; then exit 0; fi
archive="trunk-$target.tar.gz"
base="https://github.com/trunk-rs/trunk/releases/download/v$TRUNK_VERSION"
for file in "$archive" "$archive.sha256"; do
  curl --fail --location --retry 3 --silent --show-error "$base/$file" --output "$root/.tools/downloads/$file"
done
expected=$(awk '{print $1}' "$root/.tools/downloads/$archive.sha256")
actual=$(shasum -a 256 "$root/.tools/downloads/$archive" | awk '{print $1}')
[ "$expected" = "$actual" ] || { echo 'Trunk checksum mismatch.' >&2; exit 1; }
unpack=$(mktemp -d)
trap 'rm -rf -- "$unpack"' EXIT HUP INT TERM
tar -xzf "$root/.tools/downloads/$archive" -C "$unpack" trunk
install -m 0755 "$unpack/trunk" "$root/.tools/bin/trunk"
