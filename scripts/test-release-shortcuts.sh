#!/usr/bin/env bash
# Real Rust helper, disposable Git checkout, fake authenticated GitHub: no network or publication.
set -Eeuo pipefail
trap 'printf "Release shortcut fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
helper=$1
[[ "$helper" == /* && -x "$helper" ]] || exit 2
# Keep URL-format coverage independent of personal Git insteadOf rewrites.
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout with 'quotes'"
mkdir -p "$fixture/scripts" "$fixture/tools" "$fixture/charts/proofstorm" "$fixture/release" "$scratch/bin"
cp "$root/scripts/release.sh" "$fixture/scripts/"
cp "$root/Cargo.toml" "$root/Cargo.lock" "$root/install.sh" "$fixture/"
cp "$root/tools/versions.env" "$fixture/tools/"
cp "$root/charts/proofstorm/Chart.yaml" "$root/charts/proofstorm/values.yaml" "$fixture/charts/proofstorm/"
cp "$root/release/controller.json" "$root/release/controller-linux-amd64.json" "$fixture/release/"
for manifest in "$root"/crates/*/Cargo.toml; do
  name=${manifest%/Cargo.toml}; name=${name##*/}
  mkdir -p "$fixture/crates/$name/src"
  cp "$manifest" "$fixture/crates/$name/"
  touch "$fixture/crates/$name/src/lib.rs" "$fixture/crates/$name/src/main.rs"
done
printf 'target/\n' > "$fixture/.gitignore"
git -C "$fixture" init -q -b main
git -C "$fixture" remote add origin git@github.com:owner/proofstorm.git
git -C "$fixture" add .
commit() { git -C "$fixture" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit -qam fixture; }
commit
export SHORTCUT_TEST_SHA
SHORTCUT_TEST_SHA=$(git -C "$fixture" rev-parse HEAD)
export SHORTCUT_TEST_HELPER="$helper" SHORTCUT_TEST_TRACE="$scratch/trace" SHORTCUT_TEST_STATE="$scratch/state"
real_cargo=$(command -v cargo)
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]] || exit 97
[[ "$*" == 'build --quiet --locked --manifest-path '* && "$*" == *' -p proofstorm-xtask' ]] || exit 97
printf 'helper-build\n' >> "$SHORTCUT_TEST_TRACE"
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$SHORTCUT_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$scratch/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$GH_HOST" == github.com && ${GH_REPO-unset} == unset ]] || exit 97
printf '%s\n' "$*" >> "$SHORTCUT_TEST_TRACE"
case "$1" in
  auth) [[ "$*" == 'auth status --hostname github.com' && ${SHORTCUT_TEST_FAIL:-none} != auth ]] || exit 23 ;;
  repo)
    # Reproduce gh selecting a fork's upstream when no repository is supplied.
    if [[ "$*" == 'repo view --json nameWithOwner --jq .nameWithOwner' ]]; then
      echo upstream/proofstorm
    else
      [[ "$2" == view && "$4 $5 $6 $7" == '--json nameWithOwner --jq .nameWithOwner' ]] || exit 97
      case "$3" in
        git@github.com:owner/proofstorm.git|https://github.com/owner/proofstorm.git|ssh://git@github.com/owner/proofstorm.git) echo owner/proofstorm ;;
        *) exit 97 ;;
      esac
    fi ;;
  api)
    shift
    method=GET endpoint='' input='' paginated=false
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -H) shift 2 ;;
        --method) method=$2; shift 2 ;;
        --input) input=$2; shift 2 ;;
        --paginate) paginated=true; shift ;;
        --slurp) shift ;;
        repos/*) endpoint=$1; shift ;;
        *) exit 97 ;;
      esac
    done
    sha=$SHORTCUT_TEST_SHA
    if [[ "$method" == POST ]]; then
      [[ "$endpoint" == repos/owner/proofstorm/actions/workflows/alpha-release.yml/dispatches ]] || exit 97
      [[ -f "$input" ]] || exit 97
      cp "$input" "$SHORTCUT_TEST_STATE/dispatch.json"
      [[ ${SHORTCUT_TEST_FAIL:-none} != dispatch ]] || exit 24
      exit 0
    fi
    [[ "$method" == GET ]] || exit 97
    case "$endpoint" in
      repos/owner/proofstorm/branches/main)
        if [[ ${SHORTCUT_TEST_FAIL:-none} == stale || ( ${SHORTCUT_TEST_FAIL:-none} == moved && -f "$SHORTCUT_TEST_STATE/main-seen" ) ]]; then sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb; fi
        touch "$SHORTCUT_TEST_STATE/main-seen"
        printf '{"name":"main","commit":{"sha":"%s"}}\n' "$sha" ;;
      "repos/owner/proofstorm/actions/workflows/check.yml/runs?branch=main&head_sha=$sha&per_page=100")
        [[ "$paginated" == true ]] || exit 97
        if [[ ${SHORTCUT_TEST_FAIL:-none} == no-build ]]; then echo '[{"workflow_runs":[]}]'; exit 0; fi
        status=success
        [[ ${SHORTCUT_TEST_FAIL:-none} != failed-build ]] || status=failure
        printf '[{"workflow_runs":[{"id":42,"repository":{"full_name":"owner/proofstorm"},"head_sha":"%s","head_branch":"main","event":"push","status":"completed","conclusion":"%s"}]}]\n' "$sha" "$status" ;;
      repos/owner/proofstorm/actions/runs/42)
        printf '{"id":42,"repository":{"full_name":"owner/proofstorm"},"head_repository":{"full_name":"owner/proofstorm"},"head_sha":"%s","head_branch":"main","path":".github/workflows/check.yml","workflow_id":7,"event":"push","status":"completed","conclusion":"success","run_attempt":2}\n' "$sha" ;;
      repos/owner/proofstorm/actions/workflows/check.yml) echo '{"id":7,"path":".github/workflows/check.yml"}' ;;
      "repos/owner/proofstorm/compare/$sha...main") printf '{"base_commit":{"sha":"%s"},"merge_base_commit":{"sha":"%s"},"status":"identical"}\n' "$sha" "$sha" ;;
      'repos/owner/proofstorm/actions/runs/42/attempts/2/jobs?per_page=100')
        [[ "$paginated" == true ]] || exit 97
        status=success
        [[ ${SHORTCUT_TEST_FAIL:-none} != skipped-job ]] || status=skipped
        printf '[{"jobs":['
        separator=''
        for job in 'Formatting and shell' 'Rust lints and tests' 'Linux bundle and installer' 'ARM64 controller' 'Mac bundle and installer' 'Mac installer isolation'; do
          printf '%s{"name":"%s","run_id":42,"head_sha":"%s","status":"completed","conclusion":"%s"}' "$separator" "$job" "$sha" "$status"
          separator=,
        done
        printf ']}]\n' ;;
      'repos/owner/proofstorm/actions/runs/42/artifacts?per_page=100')
        [[ "$paginated" == true ]] || exit 97
        expired=false
        [[ ${SHORTCUT_TEST_FAIL:-none} != expired ]] || expired=true
        printf '[{"artifacts":[{"id":123,"name":"proofstorm-linux-amd64-%s-2","expired":%s,"workflow_run":{"id":42,"head_sha":"%s"}},{"id":124,"name":"proofstorm-macos-arm64-%s-2","expired":%s,"workflow_run":{"id":42,"head_sha":"%s"}}]}]\n' "$sha" "$expired" "$sha" "$sha" "$expired" "$sha" ;;
      repos/owner/proofstorm/git/matching-refs/tags/v*)
        [[ ${SHORTCUT_TEST_FAIL:-none} != api ]] || exit 25
        if [[ ${SHORTCUT_TEST_FAIL:-none} == tag ]]; then printf '[{"ref":"refs/tags/%s"}]\n' "${endpoint##*/}"; else echo '[]'; fi ;;
      'repos/owner/proofstorm/releases?per_page=100') [[ "$paginated" == true ]] || exit 97; echo '[[]]' ;;
      *) exit 97 ;;
    esac ;;
  *) exit 97 ;;
esac
STUB
chmod +x "$scratch/bin/cargo" "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" GH_REPO=must-not-leak PROOFSTORM_HOME=must-not-leak CARGO_BUILD_TARGET=must-not-leak
run() {
  : > "$SHORTCUT_TEST_TRACE"
  # Each invocation has separate API state; no real credential or service is used.
  SHORTCUT_TEST_STATE=$(mktemp -d "$scratch/state.XXXXXXXX")
  bash "$fixture/scripts/release.sh" "$@" > "$scratch/output" 2>&1
}
fail() { cat "$scratch/output" >&2; printf '%s\n' "$1" >&2; exit 1; }
run draft --preview || fail 'Preview failed'
if grep -q -- '--method POST' "$SHORTCUT_TEST_TRACE"; then fail 'Preview mutated GitHub'; fi
for origin in https://github.com/owner/proofstorm.git ssh://git@github.com/owner/proofstorm.git; do
  git -C "$fixture" remote set-url origin "$origin"
  run draft --preview || fail 'Explicit origin selection failed'
  grep -Fq "repo view $origin --json nameWithOwner" "$SHORTCUT_TEST_TRACE" || fail 'Origin was not passed explicitly'
  if grep -q 'repos/upstream/' "$SHORTCUT_TEST_TRACE"; then fail 'Release looked up the upstream repository'; fi
done
git -C "$fixture" remote remove origin
if run draft --yes; then fail 'Released without an origin remote'; fi
grep -q 'needs an origin remote' "$scratch/output" || fail 'Missing origin guidance'
if grep -q -- '--method POST' "$SHORTCUT_TEST_TRACE"; then fail 'Missing origin dispatched a release'; fi
git -C "$fixture" remote add origin git@github.com:owner/proofstorm.git
run draft --yes || fail 'Draft dispatch failed'
grep -q -- '--method POST' "$SHORTCUT_TEST_TRACE" || fail 'Missing authenticated dispatch'
grep -q '"create_draft":"true"' "$SHORTCUT_TEST_STATE/dispatch.json" || fail 'Request was not for a draft'
grep -q '"ref":"main"' "$SHORTCUT_TEST_STATE/dispatch.json" || fail 'Workflow must run from main'
grep -q 'not yet completed' "$scratch/output" || fail 'Dispatch was misreported as completed'
if run draft </dev/null; then fail 'Noninteractive release bypassed confirmation'; fi
if grep -q -- '--method POST' "$SHORTCUT_TEST_TRACE"; then fail 'Unconfirmed release dispatched'; fi
for failure in auth stale no-build failed-build skipped-job expired api tag moved; do
  if SHORTCUT_TEST_FAIL=$failure run draft --yes; then fail "Accepted $failure"; fi
  if grep -q -- '--method POST' "$SHORTCUT_TEST_TRACE"; then fail "Dispatched after $failure"; fi
  if [[ "$failure" == stale ]]; then
    grep -q 'Local main differs from owner/proofstorm main' "$scratch/output" || fail 'Mismatch omitted the selected repository'
    grep -q "Local:  $SHORTCUT_TEST_SHA" "$scratch/output" || fail 'Mismatch omitted the local commit'
    grep -q 'GitHub: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' "$scratch/output" || fail 'Mismatch omitted the remote commit'
  fi
done
if SHORTCUT_TEST_FAIL=dispatch run draft --yes; then fail 'Failed dispatch passed'; fi
grep -q 'Check Actions before retrying' "$scratch/output" || fail 'Missing uncertain-dispatch guidance'
git -C "$fixture" switch -q -c feature
if run draft --yes; then fail 'Released from a feature branch'; fi
[[ ! -s "$SHORTCUT_TEST_TRACE" ]] || fail 'Feature branch contacted GitHub'
git -C "$fixture" switch -q main
echo user-work > "$fixture/notes.txt"
if run prepare 0.1.0-alpha.2; then fail 'Modified a dirty checkout'; fi
[[ ! -s "$SHORTCUT_TEST_TRACE" ]] || fail 'Dirty checkout built tools'
rm "$fixture/notes.txt"
old_tag=$("$helper" release-shortcut version "$fixture")
# A higher major version stays valid even after the product's next alpha bump.
run prepare 99.0.0-alpha.1 || fail 'Version preparation failed'
[[ $("$helper" release-shortcut version "$fixture") == v99.0.0-alpha.1 ]] || fail 'Version files disagree'
[[ $(git -C "$fixture" diff --name-only | wc -l | tr -d ' ') == 6 ]] || fail 'Unexpected version edits'
cmp "$root/release/controller.json" "$fixture/release/controller.json"
cmp "$root/release/controller-linux-amd64.json" "$fixture/release/controller-linux-amd64.json"
if grep -Eq '^(auth |api )' "$SHORTCUT_TEST_TRACE"; then fail 'Version preparation contacted GitHub'; fi
grep -q 'Main CI will build, verify, and publish' "$scratch/output" || fail 'Missing automatic controller build guidance'
# Check the generated workspace lockfile with Cargo itself, without dependency downloads/builds.
(cd "$fixture"; unset CARGO_BUILD_TARGET; "$real_cargo" metadata --no-deps --offline --locked --format-version 1 >/dev/null)
[[ "$old_tag" != v99.0.0-alpha.1 ]] || fail 'Invalid fixture version'
printf 'Release shortcuts passed with real Rust validation and no GitHub writes\n'
