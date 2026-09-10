#!/usr/bin/env bash
# Real snapshot/packaging helpers; fake compilers. No Docker, network, or runtime.
set -Eeuo pipefail
trap 'printf "Release fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
export RELEASE_TEST_HELPER=${1:?pass the compiled proofstorm-xtask executable}
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout's directory"
export RELEASE_TEST_ROOT="$fixture" RELEASE_TEST_TRACE="$scratch/trace"
export RELEASE_TEST_METADATA="$scratch/metadata.json"
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) host_target=aarch64-apple-darwin; platform=linux/arm64 ;;
  Linux/x86_64) host_target=x86_64-unknown-linux-gnu; platform=linux/amd64 ;;
  *) printf 'Unsupported release fixture host\n' >&2; exit 1 ;;
esac
mkdir -p "$fixture/scripts" "$fixture/.tools/bin" "$fixture/tools" \
  "$fixture/crates/proofstorm-web" "$fixture/charts/proofstorm/templates" "$scratch/bin"
cp "$root/scripts/release-build.sh" "$fixture/scripts/"
printf "[workspace.package]\nversion = '0.1.0-alpha.1'\n" > "$fixture/Cargo.toml"
printf '.tools/\n.env\n' > "$fixture/.gitignore"
printf 'private\n' > "$fixture/.env"
printf 'fixture\n' > "$fixture/LICENSE"
printf 'fixture\n' > "$fixture/crates/proofstorm-web/index.html"
printf 'TRUNK_VERSION=fixture\n' > "$fixture/tools/versions.env"
printf 'version: 0.1.0-alpha.1\nappVersion: 0.1.0-alpha.1\n' > "$fixture/charts/proofstorm/Chart.yaml"
printf 'fixture\n' > "$fixture/charts/proofstorm/values.yaml"
for file in deployment.yaml _helpers.tpl serviceaccount.yaml rbac.yaml private-pvc.yaml; do
  printf 'fixture\n' > "$fixture/charts/proofstorm/templates/$file"
done
# Add only the build-derived fields to the shared metadata contract fixture.
{
  printf '{"source_revision":"REVISION", "source_sha256":"SOURCE_SHA", "catalog":{"entries":[]}, "tools":"TRUNK_VERSION=fixture\\n",\n'
  sed '1d' "$root/crates/proofstorm-xtask/tests/fixtures/release-info.json" |
    sed "s/x86_64-unknown-linux-gnu/$host_target/g; s@linux/amd64@$platform@g"
} > "$RELEASE_TEST_METADATA"
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_DB-unset} == unset && ${PROOFSTORM_HOME-unset} == unset ]] || exit 97
[[ ${TRUNK_BUILD_DIST-unset} == unset && ${K3D_CLUSTER-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]] || exit 97
printf '<cargo><%s>' "$PWD" >> "$RELEASE_TEST_TRACE"
printf '<%s>' "$@" >> "$RELEASE_TEST_TRACE"
printf '\n' >> "$RELEASE_TEST_TRACE"
if [[ " $* " == *' -p proofstorm-xtask '* ]]; then
  [[ "$PWD" == "$RELEASE_TEST_ROOT" && "$CARGO_TARGET_DIR" != "$RELEASE_TEST_ROOT"/* ]] || exit 97
  mkdir -p "$CARGO_TARGET_DIR/debug"
  cp "$RELEASE_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
elif [[ "$1" == build ]]; then
  [[ "$PWD" != "$RELEASE_TEST_ROOT" && ! -d .git && ! -e .env && ! -e .tools ]] || exit 97
  [[ "$PROOFSTORM_WEB_DIST" == "$PWD/crates/proofstorm-web/dist" && "$PROOFSTORM_REQUIRE_WEB_ASSETS" == 1 ]] || exit 97
  [[ ${RELEASE_TEST_FAIL:-none} != host ]] || exit 19
  profile=debug
  [[ " $* " != *' --release '* ]] || profile=release
  mkdir -p "$CARGO_TARGET_DIR/$profile"
  for name in proofstorm proofstorm-mcp; do
    {
      printf '#!/bin/sh\n'
      printf "printf '%%s\\n' '"
      sed "s/REVISION/$PROOFSTORM_BUILD_REVISION/g; s/SOURCE_SHA/$PROOFSTORM_BUILD_SOURCE_SHA256/g; s/\"debug\"/\"$profile\"/g" "$RELEASE_TEST_METADATA"
      printf "'\n"
    } > "$CARGO_TARGET_DIR/$profile/$name"
    chmod +x "$CARGO_TARGET_DIR/$profile/$name"
  done
  if [[ ${RELEASE_TEST_FAIL:-none} == metadata ]]; then
    printf '#!/bin/sh\nprintf '\''{"target":"wrong-host"}\\n'\''\n' > "$CARGO_TARGET_DIR/$profile/proofstorm"
  fi
  if [[ ${RELEASE_TEST_FAIL:-none} == package ]]; then
    printf '#!/bin/sh\nprintf '\''{}\\n'\''\n' > "$CARGO_TARGET_DIR/$profile/proofstorm-mcp"
  fi
else
  [[ "$1" == run && " $* " == *' --example export_crds '* ]] || exit 97
  [[ ${RELEASE_TEST_FAIL:-none} != crds ]] || exit 20
  destination=${!#}
  [[ "$destination" == "$PWD/charts/proofstorm/crds" ]]
  mkdir -p "$destination"
  for name in proofstormlabs proofstormlabactions proofstormcandidatebuilds; do
    printf 'fixture CRD\n' > "$destination/proofstorm.dev_$name.yaml"
  done
fi
STUB
cat > "$fixture/.tools/bin/trunk" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == --version ]]; then printf 'trunk fixture\n'; exit; fi
[[ "$*" == 'build --release --locked' && "$PWD" != "$RELEASE_TEST_ROOT"/* ]] || exit 97
[[ "$PROOFSTORM_WEB_DIST" == "$PWD/dist" && -n "$PROOFSTORM_BUILD_SOURCE_SHA256" ]] || exit 97
printf '<trunk><build>\n' >> "$RELEASE_TEST_TRACE"
[[ ${RELEASE_TEST_FAIL:-none} != web ]] || exit 18
mkdir -p dist
printf 'fixture web\n' > dist/index.html
STUB
chmod +x "$scratch/bin/cargo" "$fixture/.tools/bin/trunk"
git -C "$fixture" init -q
git -C "$fixture" add .
git -C "$fixture" -c user.email=fixture@example.invalid -c user.name=Fixture \
  -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture
run() {
  : > "$RELEASE_TEST_TRACE"
  local status
  # Run outside the source root; relative arguments and the pinned bootstrap cwd matter.
  if (cd "$scratch"; PATH="$scratch/bin:$PATH" PROOFSTORM_DB=foreign PROOFSTORM_HOME=foreign \
    TRUNK_BUILD_DIST=foreign K3D_CLUSTER=foreign CARGO_BUILD_TARGET=foreign CARGO_TARGET_DIR=foreign \
    bash "$fixture/scripts/release-build.sh" "$@") > "$scratch/stdout" 2> "$scratch/stderr"; then
    return 0
  else
    status=$?
    cat "$scratch/stderr" >&2
    return "$status"
  fi
}
run --help
[[ ! -s "$RELEASE_TEST_TRACE" ]]
if run --work-dir; then exit 1; fi
[[ ! -s "$RELEASE_TEST_TRACE" ]]
for profile in debug release; do
  args=(--source "$fixture" --work-dir "work '$profile'" --output "out '$profile'" --json)
  [[ "$profile" != debug ]] || args+=(--debug --development)
  run "${args[@]}"
  cmp "$scratch/stdout" "$scratch/work '$profile'/result.json"
  grep -q '"release_ready": false' "$scratch/stdout"
  grep -q '<trunk><build>' "$RELEASE_TEST_TRACE"
  grep -q '<export_crds>' "$RELEASE_TEST_TRACE"
  [[ -x "$scratch/work '$profile'/target/$profile/proofstorm" ]]
  [[ ! -e "$fixture/target" && ! -e "$fixture/crates/proofstorm-web/dist" && ! -e "$fixture/charts/proofstorm/crds" ]] || exit 1
done
for failure in web host crds metadata package; do
  if RELEASE_TEST_FAIL=$failure run --work-dir "$scratch/fail-$failure" --output "$scratch/out-$failure"; then exit 1; fi
  if [[ -f "$scratch/fail-$failure/result.json" ]]; then
    printf 'Failed build wrote a success report\n' >&2; exit 1
  fi
  for archive in "$scratch/out-$failure/"*.tar.gz; do
    if [[ -e "$archive" ]]; then printf 'Failed build produced an archive\n' >&2; exit 1; fi
  done
  grep -q 'Release build failed during' "$scratch/stderr"
  if [[ "$failure" == web || "$failure" == host ]]; then
    if grep -q '<export_crds>' "$RELEASE_TEST_TRACE"; then exit 1; fi
  fi
done
# A failed web build has a pristine, Git-free source snapshot, like the Linux transport.
run --source "$scratch/fail-web/source" --provenance "$scratch/fail-web/source.json" \
  --trunk "$fixture/.tools/bin/trunk" --work-dir "$scratch/transported" --output "$scratch/out-transported" \
  --target-dir "$scratch/shared cache"
grep -q '^Bundle ready in ' "$scratch/stdout"
[[ -x "$scratch/shared cache/release/proofstorm" ]]
cmp "$scratch/fail-web/source.json" "$scratch/transported/source.json"
printf 'tampered\n' > "$scratch/fail-web/source/LICENSE"
if run --source "$scratch/fail-web/source" --provenance "$scratch/fail-web/source.json" \
  --trunk "$fixture/.tools/bin/trunk" --work-dir "$scratch/tampered" --output "$scratch/out-tampered"; then exit 1; fi
[[ ! -e "$scratch/tampered" && ! -e "$scratch/out-tampered" ]] || exit 1
if grep -q '<trunk><build>' "$RELEASE_TEST_TRACE"; then exit 1; fi
printf 'Release build wrapper checks passed\n'
