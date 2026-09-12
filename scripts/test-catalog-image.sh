#!/usr/bin/env bash
# Real Rust/Bash workflow, fake Docker/GHCR: no builds, credentials, or publication.
set -Eeuo pipefail
trap 'printf "Catalog image fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
helper=$1
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/scripts" "$fixture/release" "$fixture/docker/wallet" "$scratch/bin" "$scratch/registry"
cp "$root/scripts/catalog-image.sh" "$fixture/scripts/"
cp "$root/release/ghcr.json" "$fixture/release/"
printf 'FROM fixture\n' > "$fixture/docker/wallet/Dockerfile.kube-cdk"
printf 'target/\n' > "$fixture/.gitignore"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture
export IMAGE_TEST_HELPER="$helper" IMAGE_TEST_TRACE="$scratch/trace" IMAGE_TEST_REGISTRY="$scratch/registry" IMAGE_TEST_SOURCE="$scratch/source-sha"
digest() {
  local value
  if command -v sha256sum >/dev/null; then value=$(sha256sum "$1"); else value=$(shasum -a 256 "$1"); fi
  printf 'sha256:%s\n' "${value%% *}"
}
export IMAGE_TEST_CONFIG IMAGE_TEST_MANIFEST IMAGE_TEST_ARCH
registry() {
  IMAGE_TEST_ARCH=$1
  printf '{"os":"linux","architecture":"%s"}\n' "$1" > "$scratch/registry/config"
  IMAGE_TEST_CONFIG=$(digest "$scratch/registry/config")
  printf '{"schemaVersion":2,"config":{"digest":"%s"},"layers":[{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}\n' "$IMAGE_TEST_CONFIG" > "$scratch/registry/manifest"
  IMAGE_TEST_MANIFEST=$(digest "$scratch/registry/manifest")
}
cat > "$scratch/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$IMAGE_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
SH
cat > "$scratch/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker %s\n' "$*" >> "$IMAGE_TEST_TRACE"
case "$1 $2" in
  'buildx build')
    [[ " $* " == *" --platform linux/$IMAGE_TEST_ARCH "* && " $* " == *' --load '* ]] || exit 97
    for arg in "$@"; do case "$arg" in dev.proofstorm.source-sha256=*) printf '%s' "${arg#*=}" > "$IMAGE_TEST_SOURCE" ;; esac; done ;;
  'image inspect')
    id=$IMAGE_TEST_CONFIG user=1000:1000
    [[ ${IMAGE_TEST_FAIL:-} != changed ]] || id=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    [[ ${IMAGE_TEST_FAIL:-} != root ]] || user=0
    printf '[{"Id":"%s","Os":"linux","Architecture":"%s","Config":{"User":"%s","Labels":{"dev.proofstorm.source-sha256":"%s"}}}]\n' "$id" "$IMAGE_TEST_ARCH" "$user" "$(< "$IMAGE_TEST_SOURCE")" ;;
  'run --rm')
    [[ " $* " == *' --network none '* && " $* " == *' --read-only '* && " $* " == *' --cap-drop ALL '* && " $* " == *" $IMAGE_TEST_CONFIG "* ]] || exit 97
    [[ ${IMAGE_TEST_FAIL:-} != probe ]] || exit 42
    printf 'cdk-cli 0.18.0\n' ;;
  'buildx imagetools')
    if [[ "$3" == create ]]; then [[ " $* " == *' --prefer-index=false '* ]] || exit 97
    else
      digest=$IMAGE_TEST_MANIFEST
      [[ ${IMAGE_TEST_MOVED:-0} == 0 ]] || digest=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
      printf '{"digest":"%s"}\n' "$digest"
    fi ;;
  tag*) [[ "$2" == "$IMAGE_TEST_CONFIG" ]] ;;
  push*) [[ ${IMAGE_TEST_FAIL:-} != push ]] ;;
  *) exit 97 ;;
esac
SH
cat > "$scratch/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == -q ]] || exit 97
url=${!#}
case "$url" in
  https://ghcr.io/token*) [[ " $* " != *'Authorization:'* ]] || exit 97; printf '{"token":"fixture-anonymous-pull"}\n' ;;
  https://ghcr.io/v2/*|http://127.0.0.1:*/v2/*)
    if [[ " $* " == *' --head '* ]]; then
      [[ ${IMAGE_TEST_FAIL:-} != layer ]] || exit 22
      if [[ ${IMAGE_TEST_FAIL:-} == redirect ]]; then printf 'HTTP/1.1 301 Moved Permanently\n'; else printf 'HTTP/1.1 200 OK\n'; fi
      exit 0
    fi
    case "${url##*/}" in
      "$IMAGE_TEST_CONFIG") cat "$IMAGE_TEST_REGISTRY/config" ;;
      "$IMAGE_TEST_MANIFEST") cat "$IMAGE_TEST_REGISTRY/manifest" ;;
      *) exit 22 ;;
    esac ;;
  *) exit 97 ;;
esac
SH
chmod +x "$scratch/bin/"*
export PATH="$scratch/bin:$PATH"
run() { bash "$fixture/scripts/catalog-image.sh" "$@" > "$scratch/output" 2>&1; }
namespace=ghcr.io/orangeshyguy21/proofstorm
for arch in amd64 arm64; do
  registry "$arch"
  : > "$IMAGE_TEST_TRACE"
  run build cdk-cli-wallet "linux/$arch" "$scratch/$arch"
  if grep -q 'docker push\|imagetools create' "$IMAGE_TEST_TRACE"; then exit 1; fi
  run publish "$scratch/$arch" --confirm-namespace "$namespace"
  grep -q '"publication": "verified"' "$scratch/$arch/image.json"
  grep -q '"release_ready": false' "$scratch/$arch/image.json"
  if run publish "$scratch/$arch" --confirm-namespace "$namespace"; then exit 1; fi
  run verify-work "$scratch/$arch"
  export IMAGE_TEST_MOVED=1
  if run verify-work "$scratch/$arch"; then exit 1; fi
  grep -q '"publication": "uploaded"' "$scratch/$arch/image.json"
  unset IMAGE_TEST_MOVED
  run verify-work "$scratch/$arch"
done
registry amd64
: > "$IMAGE_TEST_TRACE"
if run publish "$scratch/amd64" --confirm-namespace ghcr.io/foreign; then exit 1; fi
[[ ! -s "$IMAGE_TEST_TRACE" ]]
for failure in root probe changed layer redirect push; do
  run build cdk-cli-wallet linux/amd64 "$scratch/$failure"
  : > "$IMAGE_TEST_TRACE"
  export IMAGE_TEST_FAIL=$failure
  if run publish "$scratch/$failure" --confirm-namespace "$namespace"; then exit 1; fi
  if [[ "$failure" == root || "$failure" == probe || "$failure" == changed ]]; then
    if grep -q 'docker push' "$IMAGE_TEST_TRACE"; then exit 1; fi
  else
    if [[ "$failure" == push ]]; then state=upload_attempted; else state=uploaded; fi
    grep -q "\"publication\": \"$state\"" "$scratch/$failure/image.json"
  fi
  unset IMAGE_TEST_FAIL
done
run prepare-copy "127.0.0.1:54321/cdk-cli-wallet@$IMAGE_TEST_MANIFEST" linux/amd64 "$scratch/copy"
run publish "$scratch/copy" --confirm-namespace "$namespace"
grep -q '"publication": "verified"' "$scratch/copy/image.json"
run verify "$namespace/cdk-cli-wallet@$IMAGE_TEST_MANIFEST" linux/amd64 "$scratch/verify.json"
grep -q '"anonymous_verified": true' "$scratch/verify.json"
if run verify "$namespace/cdk-cli-wallet@$IMAGE_TEST_MANIFEST" linux/arm64 "$scratch/wrong-platform.json"; then exit 1; fi
grep -q '"anonymous_verified": false' "$scratch/wrong-platform.json"
echo 'Catalog image builds, copies, publication guards and partial receipts passed'
