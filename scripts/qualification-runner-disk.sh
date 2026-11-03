#!/usr/bin/env bash
# Free hosted-runner disk before native qualification loads images. Some
# ubuntu-24.04 runners (AWS-backed) leave about 6 GB after the image bundle, so
# k3s reaches its 5% disk-pressure eviction threshold mid-setup. Remove
# preinstalled toolchains that no qualification step uses.
set -euo pipefail
df -h /
sudo rm -rf /usr/local/lib/android /usr/share/dotnet /opt/ghc /usr/local/.ghcup \
  /opt/hostedtoolcache/CodeQL /usr/share/swift /usr/local/share/powershell \
  /usr/local/share/chromium /usr/local/julia*
df -h /
available=$(df --output=avail -B1G / | tail -1 | tr -d ' ')
if (( available < 40 )); then
  printf '::warning::Runner has %s GB free after cleanup; native clusters may hit disk pressure\n' "$available"
fi
