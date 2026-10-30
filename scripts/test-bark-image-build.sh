#!/usr/bin/env bash
# Exercise native/receipt/export refusals without Docker, network, or real builds.
# Covers the shared stack-image-build.sh through Bark's entry point and for Spark.
set -Eeuo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with spaces"
mkdir -p "$fixture/scripts" "$scratch/bin"
cp "$root/scripts/bark-image-build.sh" "$root/scripts/stack-image-build.sh" \
  "$root/scripts/qualification-docker.sh" "$fixture/scripts/"
export BARK_TEST_TRACE="$scratch/trace" BARK_TEST_BIN="$scratch/bin"
export BARK_TEST_MACHINE=x86_64 BARK_TEST_ENGINE=linux/amd64 BARK_TEST_OS=Linux
export BARK_TEST_ENDPOINT=unix:///fixture/docker.sock
export BARK_TEST_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
if command -v sha256sum >/dev/null; then
  export BARK_TEST_HASH_BIN BARK_TEST_HASH_MODE=sha256sum
  BARK_TEST_HASH_BIN=$(command -v sha256sum)
else
  export BARK_TEST_HASH_BIN BARK_TEST_HASH_MODE=shasum
  BARK_TEST_HASH_BIN=$(command -v shasum)
fi
cat > "$scratch/bin/uname" <<'SH'
#!/usr/bin/env bash
case "$1" in -s) echo "$BARK_TEST_OS" ;; -m) echo "$BARK_TEST_MACHINE" ;; *) exit 97 ;; esac
SH
cat > "$scratch/bin/git" <<'SH'
#!/usr/bin/env bash
[[ "$3 $4" == 'rev-parse HEAD' ]] || exit 97
printf '%s\n' "${BARK_TEST_SHA:0:40}"
SH
cat > "$scratch/bin/timeout" <<'SH'
#!/usr/bin/env bash
shift
exec "$@"
SH
cat > "$scratch/bin/sha256sum" <<'SH'
#!/usr/bin/env bash
if [[ "$BARK_TEST_HASH_MODE" == shasum ]]; then
  exec "$BARK_TEST_HASH_BIN" -a 256 "$@"
else
  exec "$BARK_TEST_HASH_BIN" "$@"
fi
SH
cat > "$scratch/bin/docker-buildx" <<'SH'
#!/usr/bin/env bash
exit 97
SH
cat > "$scratch/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker %s\n' "$*" >> "$BARK_TEST_TRACE"
case "$1 $2" in
  'context inspect') jq -n --arg endpoint "$BARK_TEST_ENDPOINT" '{Host:$endpoint}' ;;
  'info --format')
    if [[ "$3" == '{{json .ClientInfo.Plugins}}' ]]; then
      jq -n --arg plugin "$BARK_TEST_BIN/docker-buildx" '[{Name:"buildx",Path:$plugin}]'
    else echo "$BARK_TEST_ENGINE"; fi ;;
  'version --format') echo fixture ;;
  'image save')
    [[ "$3" == --output && "$5" == "sha256:$BARK_TEST_SHA" ]] || exit 97
    printf 'immutable image bytes\n' > "$4"
    [[ ${BARK_TEST_FAIL:-} != export ]] || exit 45 ;;
  *) exit 97 ;;
esac
SH
cat > "$fixture/scripts/catalog-image.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo build >> "$BARK_TEST_TRACE"
[[ "$1" == build && "$DOCKER_HOST" == "$BARK_TEST_ENDPOINT" ]] || exit 97
[[ -z ${DOCKER_CONTEXT:-} && -z ${DOCKER_AUTH_CONFIG:-} && -z ${BUILDX_BUILDER:-} ]] || exit 97
jq -e '. == {auths:{"127.0.0.1":{}}}' "$DOCKER_CONFIG/config.json" >/dev/null
[[ ${BARK_TEST_FAIL:-} != build ]] || exit 42
mkdir -p "$4/source"
printf 'frozen source\n' > "$4/source/fixture"
printf 'native probe\n' > "$4/probe.stdout"
printf '[]\n' > "$4/inspect.json"
jq -n --arg platform "$3" --arg selection "$2" --arg sha "$BARK_TEST_SHA" \
  '{platform:$platform,repository:($selection|split("@")[0]),input:{kind:"Build",version:($selection|split("@")[1]),source:{dirty:false,revision:$sha[:40]}},local_verified:true,local_image_id:("sha256:"+$sha),publication:"prepared",image:null,release_ready:false}' > "$4/image.json"
case ${BARK_TEST_FAIL:-} in
  dirty) expression='.input.source.dirty=true' ;;
  revision) expression='.input.source.revision="wrong"' ;;
  platform) expression='.platform="linux/foreign"' ;;
  selection) expression='.repository="foreign"' ;;
  unverified) expression='.local_verified=false' ;;
  *) exit 0 ;;
esac
jq "$expression" "$4/image.json" > "$4/changed.json"
mv "$4/changed.json" "$4/image.json"
SH
chmod +x "$scratch/bin/"* "$fixture/scripts/catalog-image.sh"
export PATH="$scratch/bin:$PATH"
export DOCKER_CONTEXT=desktop DOCKER_AUTH_CONFIG=unused BUILDX_BUILDER=remote
run() { bash "$fixture/scripts/bark-image-build.sh" "$@" > "$scratch/output" 2>&1; }
stack_run() { bash "$fixture/scripts/stack-image-build.sh" "$@" > "$scratch/output" 2>&1; }
selection=bark-server@0.7.0-6188e2d
spark=open-ssp@0.1.0-9c25f69
: > "$BARK_TEST_TRACE"
if run invalid linux/amd64 "$scratch/invalid"; then exit 1; fi
if run "$selection" linux/foreign "$scratch/invalid"; then exit 1; fi
if run "$selection" linux/arm64 "$scratch/emulated"; then exit 1; fi
export BARK_TEST_OS=Darwin
if run "$selection" linux/amd64 "$scratch/non-linux"; then exit 1; fi
export BARK_TEST_OS=Linux
mkdir "$scratch/existing"
if run "$selection" linux/amd64 "$scratch/existing"; then exit 1; fi
if run "$selection" linux/amd64 "$fixture/work"; then exit 1; fi
[[ ! -s "$BARK_TEST_TRACE" && ! -e "$scratch/emulated" && ! -e "$fixture/work" ]]
# Each stack builds only its own reviewed recipes; unknown stacks are refused.
if stack_run lnd "$selection" linux/amd64 "$scratch/unknown-stack"; then exit 1; fi
if stack_run spark "$selection" linux/amd64 "$scratch/cross-stack"; then exit 1; fi
if stack_run bark "$spark" linux/amd64 "$scratch/cross-stack"; then exit 1; fi
if stack_run spark ldk-server@0.1.0-50fe752 linux/amd64 "$scratch/cross-stack"; then exit 1; fi
if stack_run spark "$spark" linux/arm64 "$scratch/cross-stack"; then exit 1; fi
# Apple silicon is a native ARM64 builder only for an explicit local build.
export BARK_TEST_OS=Darwin BARK_TEST_MACHINE=arm64 BARK_TEST_ENGINE=linux/aarch64
if stack_run spark "$spark" linux/arm64 "$scratch/apple-ci"; then exit 1; fi
if stack_run spark --local "$spark" linux/amd64 "$scratch/apple-amd64"; then exit 1; fi
if run --local "$selection" linux/amd64 "$scratch/apple-amd64"; then exit 1; fi
export BARK_TEST_MACHINE=x86_64
if stack_run spark --local "$spark" linux/arm64 "$scratch/intel-mac"; then exit 1; fi
export BARK_TEST_OS=Linux BARK_TEST_ENGINE=linux/amd64
[[ ! -s "$BARK_TEST_TRACE" && ! -e "$scratch/cross-stack" && ! -e "$scratch/apple-ci" ]]
export BARK_TEST_ENDPOINT=tcp://remote.invalid:2375
if run "$selection" linux/amd64 "$scratch/remote"; then exit 1; fi
export BARK_TEST_ENDPOINT=unix:///fixture/docker.sock BARK_TEST_ENGINE=linux/arm64
if run "$selection" linux/amd64 "$scratch/wrong-engine"; then exit 1; fi
if grep -q '^build$' "$BARK_TEST_TRACE"; then exit 1; fi
export BARK_TEST_ENGINE=linux/amd64
for failure in build export dirty revision platform selection unverified; do
  export BARK_TEST_FAIL=$failure
  if run "$selection" linux/amd64 "$scratch/fail-$failure"; then exit 1; fi
  [[ ! -e "$scratch/fail-$failure/artifacts/SHA256SUMS" ]]
  [[ ! -e "$scratch/fail-$failure/artifacts/image.tar" ]]
done
unset BARK_TEST_FAIL
for arch in amd64 arm64; do
  BARK_TEST_MACHINE=x86_64
  [[ "$arch" != arm64 ]] || BARK_TEST_MACHINE=aarch64
  export BARK_TEST_MACHINE BARK_TEST_ENGINE="linux/$arch"
  for selection in cdk-bark-processor@0.1.0-fe468ca bark-server@0.7.0-6188e2d cln-hold@26.06.7-hold.0.3.3; do
    work="$scratch/$arch-$selection"
    run "$selection" "linux/$arch" "$work"
    (cd "$work/artifacts"; sha256sum --check SHA256SUMS >/dev/null)
    jq -e --arg platform "linux/$arch" --arg machine "$BARK_TEST_MACHINE" '.platform==$platform and .host=={os:"Linux",machine:$machine} and .managed_qualification==false and .published==false' "$work/artifacts/native.json" >/dev/null
    mkdir "$work/restored"
    tar -C "$work/restored" -xzf "$work/artifacts/work.tar.gz"
    cmp "$work/build/image.json" "$work/restored/image.json"
    cmp "$work/build/source/fixture" "$work/restored/source/fixture"
    [[ ! -e "$work/restored/docker-config" ]]
  done
  for selection in spark-operator@0.0.0-5e0c879 open-ssp@0.1.0-9c25f69 cdk-spark-processor@0.1.0-d7244e2 ldk-server@0.1.0-762bc47; do
    work="$scratch/$arch-$selection"
    stack_run spark "$selection" "linux/$arch" "$work"
    (cd "$work/artifacts"; sha256sum --check SHA256SUMS >/dev/null)
    jq -e --arg platform "linux/$arch" --arg machine "$BARK_TEST_MACHINE" '.platform==$platform and .host=={os:"Linux",machine:$machine}' "$work/artifacts/native.json" >/dev/null
    jq -e --arg selection "$selection" '(.repository+"@"+.input.version)==$selection' "$work/artifacts/image.json" >/dev/null
  done
done
# A local build on Apple silicon records the real host, never a Linux runner.
export BARK_TEST_OS=Darwin BARK_TEST_MACHINE=arm64 BARK_TEST_ENGINE=linux/aarch64
work="$scratch/apple-$spark"
stack_run spark --local "$spark" linux/arm64 "$work"
(cd "$work/artifacts"; sha256sum --check SHA256SUMS >/dev/null)
jq -e '.platform=="linux/arm64" and .host=={os:"Darwin",machine:"arm64"} and .docker.engine=="linux/aarch64"' "$work/artifacts/native.json" >/dev/null
echo 'Native Bark and Spark builds, isolated Docker, receipt and partial export guards passed'
