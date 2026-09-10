#!/usr/bin/env bash
# Pinned maintainer-tool downloads, formerly Makefile file targets.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=tools/versions.env
source "$root/tools/versions.env"

platform_os=$(uname -s | tr '[:upper:]' '[:lower:]')
case "$(uname -m)" in
  x86_64) platform_arch=amd64 ;;
  arm64|aarch64) platform_arch=arm64 ;;
  *) printf 'Unsupported host architecture\n' >&2; exit 1 ;;
esac
case "$platform_os" in darwin|linux) ;; *) printf 'Unsupported host OS\n' >&2; exit 1 ;; esac

bin_dir="$root/.tools/bin"
download_dir="$root/.tools/downloads"
mkdir -p "$bin_dir" "$download_dir"
download() {
  curl --fail --location --retry 3 --silent --show-error --proto '=https' --proto-redir '=https' "$1" --output "$2"
}
verify() {
  local expected=$1 file=$2 actual
  [[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || { printf 'Invalid checksum for %s\n' "$file" >&2; exit 1; }
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$file" | awk '{print $1}')
  else
    actual=$(shasum -a 256 "$file" | awk '{print $1}')
  fi
  [[ "$expected" == "$actual" ]] || { printf 'Checksum mismatch for %s\n' "$file" >&2; exit 1; }
}

# Preserve the old file-target behavior: installed tools are reused, not replaced.
if [[ ! -x "$bin_dir/k3d" ]]; then
  printf '[proofstorm] downloading k3d %s\n' "$K3D_VERSION"
  base="https://github.com/k3d-io/k3d/releases/download/$K3D_VERSION"
  download "$base/k3d-$platform_os-$platform_arch" "$download_dir/k3d"
  download "$base/checksums.txt" "$download_dir/k3d-checksums.txt"
  expected=$(awk -v name="_dist/k3d-$platform_os-$platform_arch" '$2 == name {print $1}' "$download_dir/k3d-checksums.txt")
  verify "$expected" "$download_dir/k3d"
  install -m 0755 "$download_dir/k3d" "$bin_dir/k3d"
fi
if [[ ! -x "$bin_dir/kubectl" ]]; then
  printf '[proofstorm] downloading kubectl %s\n' "$KUBECTL_VERSION"
  base="https://dl.k8s.io/release/$KUBECTL_VERSION/bin/$platform_os/$platform_arch/kubectl"
  download "$base" "$download_dir/kubectl"
  download "$base.sha256" "$download_dir/kubectl.sha256"
  expected=$(tr -d '[:space:]' < "$download_dir/kubectl.sha256")
  verify "$expected" "$download_dir/kubectl"
  install -m 0755 "$download_dir/kubectl" "$bin_dir/kubectl"
fi
if [[ ! -x "$bin_dir/helm" ]]; then
  printf '[proofstorm] downloading helm %s\n' "$HELM_VERSION"
  base="https://get.helm.sh/helm-$HELM_VERSION-$platform_os-$platform_arch.tar.gz"
  download "$base" "$download_dir/helm.tar.gz"
  download "$base.sha256sum" "$download_dir/helm.tar.gz.sha256sum"
  expected=$(awk '{print $1}' "$download_dir/helm.tar.gz.sha256sum")
  verify "$expected" "$download_dir/helm.tar.gz"
  unpack=$(mktemp -d)
  trap 'rm -rf -- "$unpack"' EXIT
  tar -xzf "$download_dir/helm.tar.gz" -C "$unpack" "$platform_os-$platform_arch/helm"
  install -m 0755 "$unpack/$platform_os-$platform_arch/helm" "$bin_dir/helm"
fi
