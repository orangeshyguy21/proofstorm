#!/usr/bin/env bash
# Native candidate builds only. Retain exact images/source for later qualification.
set -Eeuo pipefail
umask 077
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
[[ $# == 3 ]] || { echo 'Usage: bark-image-build.sh RECIPE@VERSION linux/amd64|linux/arm64 NEW_EXTERNAL_WORK' >&2; exit 2; }
selection=$1 platform=$2
case "$selection" in
  cdk-bark-processor@0.1.0-fe468ca|bark-server@0.7.0-6188e2d|cln-hold@26.06.7-hold.0.3.3) ;;
  *) echo 'Select an exact reviewed Bark recipe' >&2; exit 2 ;;
esac
case "$platform" in
  linux/amd64) machine=x86_64 ;;
  linux/arm64) machine=aarch64 ;;
  *) echo 'Select linux/amd64 or linux/arm64' >&2; exit 2 ;;
esac
[[ $(uname -s)/$(uname -m) == "Linux/$machine" ]] || { echo 'Build on the matching native Linux host; emulation is not qualification' >&2; exit 1; }
work=$(cd -- "$(dirname -- "$3")" && pwd -P)/$(basename -- "$3")
[[ "$work" != "$root" && "$work" != "$root/"* && ! -e "$work" && ! -L "$work" ]] || { echo 'Work must be new and outside the checkout' >&2; exit 1; }
mkdir -- "$work"
out="$work/artifacts"
mkdir -- "$out"
trap 'printf "Bark candidate build failed. Retain %s; a complete handoff requires SHA256SUMS.\n" "$work" >&2' ERR
# shellcheck source=scripts/qualification-docker.sh
source "$root/scripts/qualification-docker.sh"
qualification_docker_config "$work/docker-config"
engine=$(timeout 30 docker info --format '{{.OSType}}/{{.Architecture}}')
case "$platform:$engine" in
  linux/amd64:linux/x86_64|linux/amd64:linux/amd64|linux/arm64:linux/aarch64|linux/arm64:linux/arm64) ;;
  *) echo 'Docker engine architecture differs from the native host' >&2; exit 1 ;;
esac
version=$(timeout 30 docker version --format '{{.Server.Version}}')
revision=$(git -C "$root" rev-parse HEAD)
jq -n --arg platform "$platform" --arg machine "$machine" --arg engine "$engine" \
  --arg version "$version" --arg revision "$revision" \
  '{format_version:1,platform:$platform,host:{os:"Linux",machine:$machine},docker:{engine:$engine,version:$version},revision:$revision,managed_qualification:false,published:false}' > "$out/native.json"
bash "$root/scripts/catalog-image.sh" build "$selection" "$platform" "$work/build" 2>&1 | tee "$out/build.log"
image=$(jq -er --arg platform "$platform" --arg revision "$revision" --arg selection "$selection" '
  select(.platform==$platform and .input.source.revision==$revision and
    .input.kind=="Build" and .input.source.dirty==false and
    (.repository+"@"+.input.version)==$selection and .local_verified==true and
    .publication=="prepared" and .image==null and .release_ready==false) |
  .local_image_id | select(test("^sha256:[a-f0-9]{64}$"))' "$work/build/image.json")
# Save by immutable image ID, not the temporary upload tag. Preserve the source
# snapshot required by the ordinary catalog-image verification/publication guard.
timeout 300 docker image save --output "$out/image.tar.partial" "$image"
mv -- "$out/image.tar.partial" "$out/image.tar"
tar -C "$work/build" -czf "$out/work.tar.gz" source image.json inspect.json probe.stdout
cp "$work/build/image.json" "$work/build/inspect.json" "$work/build/probe.stdout" "$out/"
(
  cd "$out"
  sha256sum image.tar work.tar.gz image.json inspect.json probe.stdout native.json build.log > SHA256SUMS.partial
  mv SHA256SUMS.partial SHA256SUMS
)
printf 'Native candidate retained at %s. Managed qualification and publication remain separate.\n' "$out"
