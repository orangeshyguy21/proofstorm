#!/usr/bin/env bash
# Fast two-platform orchestration fixture. Rust checks real metadata and archives.
set -Eeuo pipefail
trap 'printf "Alpha promotion fixture failed at line %s\n" "$LINENO" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
fixture="$scratch/checkout's directory"
mkdir -p "$fixture/scripts" "$scratch/bin"
cp "$root/scripts/release-promote.sh" "$fixture/scripts/"
export PROMOTE_TEST_TRACE="$scratch/trace" PROMOTE_TEST_HELPER="$scratch/helper"
cat > "$scratch/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ ${PROOFSTORM_HOME-unset} == unset && ${CARGO_BUILD_TARGET-unset} == unset ]] || exit 97
[[ "$*" == 'build --locked --manifest-path '* && "$*" == *' -p proofstorm-xtask' ]] || exit 97
printf 'helper-build\n' >> "$PROMOTE_TEST_TRACE"
mkdir -p "$CARGO_TARGET_DIR/debug"
cp "$PROMOTE_TEST_HELPER" "$CARGO_TARGET_DIR/debug/proofstorm-xtask"
STUB
cat > "$scratch/helper" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == release-promotion ]] || exit 97
printf '%s\n' "$2" >> "$PROMOTE_TEST_TRACE"
[[ "$2" != "${PROMOTE_TEST_FAIL:-none}" ]] || exit 23
case "$2" in
  run)
    attempt=2
    if [[ ${PROMOTE_TEST_FAIL:-none} == changed-run && -f "$3/run-seen" ]]; then attempt=3; fi
    touch "$3/run-seen"
    printf '%s\0' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa "$attempt" "proofstorm-linux-amd64-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-$attempt" "proofstorm-macos-arm64-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-$attempt" ;;
  evidence)
    echo 123
    if [[ ${PROMOTE_TEST_FAIL:-none} == changed-artifact && -f "$3/artifact-seen" ]]; then echo 125; else echo 124; fi
    touch "$3/artifact-seen" ;;
  unused) : ;;
  verify)
    [[ -f "$4/linux-amd64/install.sh" && -f "$4/macos-arm64/install.sh" && -f "$3/source-install.sh" ]] || exit 97
    printf 'fixture notes\n' > "$3/notes.md"
    printf '{"draft":true}\n' > "$3/create-release.json" ;;
  created) echo 9 ;;
  assets)
    [[ -f "$4/build-report-linux-amd64.json" && -f "$4/build-report-macos-arm64.json" ]] || exit 97
    files=("$4"/*)
    [[ ${#files[@]} == 11 ]] || exit 97 ;;
  uploaded) [[ -f "$4/install.sh" ]] || exit 97 ;;
  *) exit 97 ;;
esac
STUB
cat > "$scratch/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$GH_HOST" == github.com && "$GH_PROMPT_DISABLED" == 1 ]] || exit 97
action=$1
shift
case "$action" in
  api)
    method=GET endpoint='' input='' paginate=false slurp=false
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -H) shift 2 ;;
        --method) method=$2; shift 2 ;;
        --input) input=$2; shift 2 ;;
        --paginate) paginate=true; shift ;;
        --slurp) slurp=true; shift ;;
        repos/*) endpoint=$1; shift ;;
        *) exit 97 ;;
      esac
    done
    printf '%s %s\n' "$method" "$endpoint" >> "$PROMOTE_TEST_TRACE"
    if [[ "$method" == POST ]]; then
      [[ "$endpoint" == repos/owner/proofstorm/releases && -f "$input" ]] || exit 97
      [[ ${PROMOTE_TEST_FAIL:-none} != create-api ]] || exit 24
      printf '{"id":9}\n'
    elif [[ "$method" == GET ]]; then
      case "$endpoint" in
        repos/owner/proofstorm/actions/runs/42|repos/owner/proofstorm/actions/workflows/check.yml|repos/owner/proofstorm/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...main|repos/owner/proofstorm/releases/9) echo '{}' ;;
        repos/owner/proofstorm/git/matching-refs/tags/v0.1.0-alpha.1)
          [[ ${PROMOTE_TEST_FAIL:-none} != refs-api ]] || exit 25
          echo '[]' ;;
        'repos/owner/proofstorm/actions/runs/42/attempts/2/jobs?per_page=100'|'repos/owner/proofstorm/actions/runs/42/artifacts?per_page=100'|'repos/owner/proofstorm/releases?per_page=100')
          [[ "$paginate" == true && "$slurp" == true ]] || exit 97
          echo '[]' ;;
        'repos/owner/proofstorm/contents/install.sh?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa') echo 'fixture installer' ;;
        *) exit 97 ;;
      esac
    else exit 97; fi ;;
  run)
    [[ "$1" == download && "$2" == 42 && "$3" == --repo && "$4" == owner/proofstorm && "$5" == --name && "$7" == --dir ]] || exit 97
    case "$6" in
      proofstorm-linux-amd64-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-2) target=x86_64-unknown-linux-gnu ;;
      proofstorm-macos-arm64-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-2) target=aarch64-apple-darwin; [[ ${PROMOTE_TEST_FAIL:-none} != mac-download ]] || exit 26 ;;
      *) exit 97 ;;
    esac
    printf 'download-candidate\n' >> "$PROMOTE_TEST_TRACE"
    [[ ${PROMOTE_TEST_FAIL:-none} != download ]] || exit 26
    mkdir "$8"
    for file in "proofstorm-0.1.0-alpha.1-$target.tar.gz" "proofstorm-0.1.0-alpha.1-$target.tar.gz.sha256" install.sh build-report.json smoke-report.json install-smoke-report.json; do echo fixture > "$8/$file"; done ;;
  release)
    [[ "$2" == v0.1.0-alpha.1 ]] || exit 97
    case "$1" in
      upload)
        printf 'upload\n' >> "$PROMOTE_TEST_TRACE"
        [[ $# == 15 && "${14}" == --repo && "${15}" == owner/proofstorm ]] || exit 97
        for file in "${@:3:11}"; do [[ -f "$file" ]] || exit 97; done
        [[ ${PROMOTE_TEST_FAIL:-none} != upload ]] || exit 27 ;;
      download)
        printf 'download-uploaded\n' >> "$PROMOTE_TEST_TRACE"
        [[ $# == 8 && "$3" == --repo && "$4" == owner/proofstorm && "$5" == --dir && "$7" == --pattern && "$8" == '*' ]] || exit 97
        [[ ${PROMOTE_TEST_FAIL:-none} != redownload ]] || exit 28
        mkdir "$6"
        echo fixture > "$6/install.sh" ;;
      *) exit 97 ;;
    esac ;;
  *) exit 97 ;;
esac
STUB
chmod +x "$scratch/bin/cargo" "$scratch/bin/gh" "$scratch/helper"
export PATH="$scratch/bin:$PATH" PROOFSTORM_HOME=must-not-leak CARGO_BUILD_TARGET=must-not-leak
count=0
run() {
  count=$((count + 1))
  : > "$PROMOTE_TEST_TRACE"
  bash "$fixture/scripts/release-promote.sh" --repo owner/proofstorm --run-id 42 --tag v0.1.0-alpha.1 --work-dir "$scratch/work $count" "$@" > "$scratch/output" 2>&1
}
fail() { cat "$scratch/output" >&2; printf '%s\n' "$1" >&2; exit 1; }
run || fail 'Preview failed'
if grep -Eq '^(POST |upload|created|download-uploaded)' "$PROMOTE_TEST_TRACE"; then fail 'Preview attempted a mutation'; fi
grep -q 'Preview only' "$scratch/output" || fail 'Missing preview summary'
run --draft || fail 'Draft failed'
for action in 'POST repos/owner/proofstorm/releases' upload download-uploaded uploaded; do
  grep -Fxq "$action" "$PROMOTE_TEST_TRACE" || fail "Missing $action"
done
[[ $(grep -c '^verify$' "$PROMOTE_TEST_TRACE") == 2 ]] || fail 'Draft must reverify before writing'
[[ $(grep -c '^GET repos/owner/proofstorm/actions/runs/42$' "$PROMOTE_TEST_TRACE") == 2 ]] || fail 'Draft must recheck source run'
for failure in run evidence unused verify assets refs-api download mac-download changed-run changed-artifact; do
  if PROMOTE_TEST_FAIL=$failure run --draft; then fail "Accepted $failure"; fi
  if grep -Eq '^(POST |upload)' "$PROMOTE_TEST_TRACE"; then fail "Mutated after $failure"; fi
done
for failure in create-api created upload redownload uploaded; do
  if PROMOTE_TEST_FAIL=$failure run --draft; then fail "Accepted $failure"; fi
  grep -q 'retained for inspection' "$scratch/output" || fail 'Missing draft recovery guidance'
done
if run --tag 'v0.1.0-alpha.1;touch unwanted'; then fail 'Accepted unsafe tag'; fi
[[ ! -s "$PROMOTE_TEST_TRACE" ]] || fail 'Invalid arguments reached external tools'
printf 'Alpha promotion orchestration checks passed\n'
