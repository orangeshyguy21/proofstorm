#!/usr/bin/env bash
# Real Rust checks, fake Docker/curl, disposable source: never builds or publishes an image.
set -Eeuo pipefail
trap 'printf "Controller fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
helper=$1
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with 'quotes'"
mkdir -p "$fixture/scripts" "$fixture/docker/release" "$scratch/bin" "$scratch/registry"
cp "$root/scripts/controller-build.sh" "$fixture/scripts/"
printf "[workspace.package]\nversion = '0.1.0-alpha.2'\n" > "$fixture/Cargo.toml"
printf 'target/\n' > "$fixture/.gitignore"
printf 'FROM fixture\n' > "$fixture/Dockerfile.proofstormd"
printf 'FROM fixture\n' > "$fixture/docker/release/Dockerfile.linux-builder"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture
export CONTROLLER_TEST_HELPER="$helper" CONTROLLER_TEST_TRACE="$scratch/trace" CONTROLLER_TEST_STATE="$scratch/state" CONTROLLER_TEST_REGISTRY="$scratch/registry"
mkdir "$CONTROLLER_TEST_STATE"
digest() {
  local value
  if command -v sha256sum >/dev/null; then value=$(sha256sum "$1"); else value=$(shasum -a 256 "$1"); fi
  printf 'sha256:%s\n' "${value%% *}"
}
printf '{"os":"linux","architecture":"amd64"}\n' > "$scratch/registry/config"
export CONTROLLER_TEST_CONFIG CONTROLLER_TEST_MANIFEST CONTROLLER_TEST_INDEX
CONTROLLER_TEST_CONFIG=$(digest "$scratch/registry/config")
printf '{"schemaVersion":2,"config":{"digest":"%s"},"layers":[{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}\n' "$CONTROLLER_TEST_CONFIG" > "$scratch/registry/manifest"
CONTROLLER_TEST_MANIFEST=$(digest "$scratch/registry/manifest")
printf '{"schemaVersion":2,"manifests":[{"digest":"%s","platform":{"os":"linux","architecture":"amd64"}}]}\n' "$CONTROLLER_TEST_MANIFEST" > "$scratch/registry/index"
CONTROLLER_TEST_INDEX=$(digest "$scratch/registry/index")
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_CONTROLLER_RECEIPT-unset} == unset ]] || exit 97
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$CONTROLLER_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$scratch/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker %s\n' "$*" >> "$CONTROLLER_TEST_TRACE"
identity=$CONTROLLER_TEST_CONFIG
case ${CONTROLLER_TEST_ID:-config} in
  manifest) identity=$CONTROLLER_TEST_MANIFEST ;;
  index) identity=$CONTROLLER_TEST_INDEX ;;
  unrelated) identity=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ;;
esac
case "$1" in
  buildx)
    if [[ "$2" == build ]]; then
      [[ ${CONTROLLER_TEST_FAIL:-none} != build ]] || exit 23
      [[ " $* " == *' --platform linux/amd64 '* && " $* " == *' --provenance=false '* && " $* " == *' --load '* ]] || exit 97
      for arg in "$@"; do case "$arg" in PROOFSTORM_CONTROLLER_SOURCE_SHA256=*) printf '%s\n' "${arg#*=}" > "$CONTROLLER_TEST_STATE/source-sha" ;; esac; done
    else
      [[ "$2" == imagetools && "$3" == inspect ]] || exit 97
      printf '{"digest":"%s"}\n' "$CONTROLLER_TEST_INDEX"
    fi ;;
  image)
    [[ "$2" == inspect ]] || exit 97
    if [[ "$3" == --format ]]; then echo "$identity"; exit 0; fi
    user=65532:65532
    [[ ${CONTROLLER_TEST_FAIL:-none} != root ]] || user=root
    printf '[{"Id":"%s","Os":"linux","Architecture":"amd64","Config":{"User":"%s","Labels":{"dev.proofstorm.source-sha256":"%s"}}}]\n' "$identity" "$user" "$(< "$CONTROLLER_TEST_STATE/source-sha")" ;;
  run)
    [[ " $* " == *' --network none '* && " $* " == *' --read-only '* && " $* " == *' --cap-drop ALL '* ]] || exit 97
    if [[ " $* " == *' --entrypoint /usr/local/lib/proofstorm-exec '* ]]; then
      if [[ ${CONTROLLER_TEST_FAIL:-none} == helper ]]; then echo 'loader failure' >&2; exit 127; fi
      printf '{"runner_error":"native_runner_failed"}\n' >&2; exit 1
    fi
    version=0.1.0-alpha.2
    [[ ${CONTROLLER_TEST_FAIL:-none} != metadata ]] || version=0.1.0-alpha.1
    printf '{"format_version":1,"version":"%s","source_sha256":"%s","runtime_contract_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}\n' "$version" "$(< "$CONTROLLER_TEST_STATE/source-sha")" ;;
  push) [[ ${CONTROLLER_TEST_FAIL:-none} != push ]] || exit 24 ;;
  *) exit 97 ;;
esac
STUB
cat > "$scratch/bin/curl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf 'anonymous request\n' >> "$CONTROLLER_TEST_TRACE"
[[ "$1" == -q && " $* " == *' --proto =https '* ]] || exit 97
if [[ " $* " == *' --head '* ]]; then [[ "$*" != *--max-filesize* ]] || exit 97; else [[ " $* " == *' --max-filesize 4194304 '* ]] || exit 97; fi
[[ "$*" != *must-not-leak* ]] || exit 97
url=${!#}
case "$url" in
  'https://ghcr.io/token?service=ghcr.io&scope=repository:orangeshyguy21/proofstorm/proofstormd:pull')
    [[ "$*" != *Authorization* ]] || exit 97
    [[ ${CONTROLLER_TEST_FAIL:-none} != private ]] || exit 22
    printf '{"token":"anonymous-fixture-token"}\n' ;;
  https://ghcr.io/v2/orangeshyguy21/proofstorm/proofstormd/*)
    [[ "$*" == *'Authorization: Bearer anonymous-fixture-token'* ]] || exit 97
    case "${url##*/}" in
      "$CONTROLLER_TEST_INDEX") cat "$CONTROLLER_TEST_REGISTRY/index" ;;
      "$CONTROLLER_TEST_MANIFEST")
        if [[ ${CONTROLLER_TEST_FAIL:-none} == digest ]]; then echo tampered; else cat "$CONTROLLER_TEST_REGISTRY/manifest"; fi ;;
      "$CONTROLLER_TEST_CONFIG") cat "$CONTROLLER_TEST_REGISTRY/config" ;;
      sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
        [[ "$*" == *--head* && ${CONTROLLER_TEST_FAIL:-none} != layer ]] || exit 22
        echo 'HTTP/2 200' ;;
      *) exit 97 ;;
    esac ;;
  *) exit 97 ;;
esac
STUB
chmod +x "$scratch/bin/cargo" "$scratch/bin/docker" "$scratch/bin/curl"
export PATH="$scratch/bin:$PATH" GH_TOKEN=must-not-leak PROOFSTORM_CONTROLLER_RECEIPT=must-not-leak
run() { bash "$fixture/scripts/controller-build.sh" "$@" > "$scratch/output" 2>&1; }
fail() { cat "$scratch/output" >&2; printf '%s\n' "$1" >&2; exit 1; }
for identity in config manifest index; do
  export CONTROLLER_TEST_ID=$identity
  work="$scratch/$identity"
  : > "$CONTROLLER_TEST_TRACE"
  run build --work-dir "$work" || fail 'Build checks failed'
  if grep -q '^docker push' "$CONTROLLER_TEST_TRACE"; then fail 'Build published an image'; fi
  run publish --work-dir "$work" --confirm-namespace ghcr.io/orangeshyguy21/proofstorm || fail 'Publication checks failed'
  [[ -f "$work/controller.json" ]] || fail 'Missing published receipt'
done
# The generated receipt crosses both host/container boundaries without dirtying source.
"$helper" linux-build-prepare "$fixture" "$scratch/linux" false false "$work/controller.json" > "$scratch/plan"
"$helper" release-worker-prepare "$scratch/linux/input" "$scratch/worker" "$scratch/artifacts" > "$scratch/worker-plan"
cmp "$work/controller.json" "$scratch/worker/controller.json"
"$helper" release-controller stage "$scratch/worker/controller.json" "$scratch/worker/source" "$scratch/linux/input/source.json" "$scratch/staged.json"
cmp "$scratch/staged.json" "$work/controller.json"
printf 'tampered' > "$scratch/linux/input/controller.json"
if "$helper" release-worker-prepare "$scratch/linux/input" "$scratch/worker-bad" "$scratch/artifacts-bad" > "$scratch/output" 2>&1; then fail 'Accepted a tampered transported controller'; fi
for failure in build root helper metadata; do
  : > "$CONTROLLER_TEST_TRACE"
  if CONTROLLER_TEST_FAIL=$failure run build --work-dir "$scratch/fail-$failure"; then fail "Accepted $failure"; fi
  if grep -q '^docker push' "$CONTROLLER_TEST_TRACE"; then fail 'Failed build published'; fi
done
for failure in push private digest layer; do
  work="$scratch/fail-$failure"
  run build --work-dir "$work" || fail 'Fixture build failed'
  if CONTROLLER_TEST_FAIL=$failure run publish --work-dir "$work" --confirm-namespace ghcr.io/orangeshyguy21/proofstorm; then fail "Accepted $failure"; fi
  [[ ! -f "$work/controller.json" ]] || fail 'Failed publication produced a usable receipt'
done
if run publish --work-dir "$work" --confirm-namespace ghcr.io/wrong; then fail 'Accepted unconfirmed namespace'; fi
export CONTROLLER_TEST_ID=unrelated
run build --work-dir "$scratch/unrelated" || fail 'Fixture build failed'
if run publish --work-dir "$scratch/unrelated" --confirm-namespace ghcr.io/orangeshyguy21/proofstorm; then fail 'Accepted a different registry image'; fi
export CONTROLLER_TEST_ID=config
run build --work-dir "$scratch/changed-source" || fail 'Fixture build failed'
printf 'changed source\n' > "$scratch/changed-source/source/Dockerfile.proofstormd"
: > "$CONTROLLER_TEST_TRACE"
if run publish --work-dir "$scratch/changed-source" --confirm-namespace ghcr.io/orangeshyguy21/proofstorm; then fail 'Published changed source'; fi
if grep -q '^docker push' "$CONTROLLER_TEST_TRACE"; then fail 'Source mismatch reached publication'; fi
[[ -z "$(git -C "$fixture" status --porcelain)" ]] || fail 'Controller flow modified the source checkout'
printf 'Controller build/publication and bundle transport checks passed without network or Docker\n'
