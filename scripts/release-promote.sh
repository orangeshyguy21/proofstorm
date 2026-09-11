#!/usr/bin/env bash
# Download and verify tested artifacts; --draft explicitly authorizes remote writes.
set -Eeuo pipefail
stage=arguments
trap 'printf "Alpha promotion failed during %s (line %s, status %s). Any created draft is retained for inspection; nothing is published automatically.\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
repo='' run_id='' tag='' work='' create_draft=false
usage() {
  printf 'Usage: just release-promote --repo OWNER/REPO --run-id ID --tag vVERSION --work-dir NEW_DIRECTORY [--draft]\nRequires matching Linux and Mac artifacts. Without --draft this is a read-only preview.\n'
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo|--run-id|--tag|--work-dir)
      [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      case "$1" in --repo) repo=$2 ;; --run-id) run_id=$2 ;; --tag) tag=$2 ;; --work-dir) work=$2 ;; esac
      shift 2 ;;
    --draft) create_draft=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ && "$run_id" =~ ^[1-9][0-9]*$ && "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+$ && -n "$work" ]] || { usage >&2; exit 2; }
work="$(cd -- "$(dirname -- "$work")" && pwd -P)/$(basename -- "$work")"
case "$work" in "$root"|"$root/"*) printf 'Choose a work directory outside the checkout\n' >&2; exit 2 ;; esac
[[ ! -e "$work" && ! -L "$work" ]] || { printf 'Work directory must be new\n' >&2; exit 2; }
for tool in cargo gh; do command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }; done
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do [[ -z "$variable" ]] || unset "$variable"; done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
export GH_HOST=github.com GH_PROMPT_DISABLED=1
umask 077
mkdir "$work"
metadata="$work/metadata"
mkdir "$metadata"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-promote.XXXXXXXX")
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
stage='maintainer verifier'
(cd "$root"; CARGO_TARGET_DIR="$scratch/target" cargo build --locked --manifest-path "$root/Cargo.toml" -p proofstorm-xtask)
helper="$scratch/target/debug/proofstorm-xtask"
api() { gh api -H 'X-GitHub-Api-Version: 2022-11-28' "$@"; }
fetch_run() {
  api "repos/$repo/actions/runs/$run_id" > "$metadata/run.json"
  api "repos/$repo/actions/workflows/check.yml" > "$metadata/workflow.json"
  "$helper" release-promotion run "$metadata" "$repo" "$run_id" "$tag" > "$scratch/plan"
}
fetch_evidence() {
  api "repos/$repo/compare/$sha...main" > "$metadata/ancestry.json"
  api --paginate --slurp "repos/$repo/actions/runs/$run_id/attempts/$attempt/jobs?per_page=100" > "$metadata/jobs.json"
  api --paginate --slurp "repos/$repo/actions/runs/$run_id/artifacts?per_page=100" > "$metadata/artifacts.json"
  "$helper" release-promotion evidence "$metadata" "$repo" "$run_id" "$tag" > "$scratch/artifact-id"
}
fetch_unused() {
  # Successful empty responses distinguish absence from network/authentication errors.
  api "repos/$repo/git/matching-refs/tags/$tag" > "$metadata/refs.json"
  api --paginate --slurp "repos/$repo/releases?per_page=100" > "$metadata/releases.json"
  "$helper" release-promotion unused "$metadata" "$tag"
}
stage='successful main build selection'
fetch_run
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 4 ]] || exit 1
sha=${plan[0]} attempt=${plan[1]} linux_artifact=${plan[2]} mac_artifact=${plan[3]}
cp "$scratch/plan" "$scratch/original-plan"
fetch_evidence
cp "$scratch/artifact-id" "$scratch/original-artifact-id"
fetch_unused
stage='candidate download and verification'
mkdir "$work/candidate"
gh run download "$run_id" --repo "$repo" --name "$linux_artifact" --dir "$work/candidate/linux-amd64"
gh run download "$run_id" --repo "$repo" --name "$mac_artifact" --dir "$work/candidate/macos-arm64"
api -H 'Accept: application/vnd.github.raw+json' "repos/$repo/contents/install.sh?ref=$sha" > "$metadata/source-install.sh"
"$helper" release-promotion verify "$metadata" "$work/candidate" "$repo" "$run_id" "$tag"
stage='verified multi-platform asset collection'
mkdir "$work/assets"
assets=()
for platform in linux-amd64 macos-arm64; do
  case "$platform" in linux-amd64) target=x86_64-unknown-linux-gnu ;; macos-arm64) target=aarch64-apple-darwin ;; esac
  archive="proofstorm-${tag#v}-$target.tar.gz"
  for name in "$archive" "$archive.sha256"; do
    cp "$work/candidate/$platform/$name" "$work/assets/$name"
    assets+=("$work/assets/$name")
  done
  for report in build-report smoke-report install-smoke-report; do
    name="$report-$platform.json"
    cp "$work/candidate/$platform/$report.json" "$work/assets/$name"
    assets+=("$work/assets/$name")
  done
done
cmp "$work/candidate/linux-amd64/install.sh" "$work/candidate/macos-arm64/install.sh"
cp "$work/candidate/linux-amd64/install.sh" "$work/assets/install.sh"
assets+=("$work/assets/install.sh")
"$helper" release-promotion assets "$metadata" "$work/assets"
printf 'Verified %s from commit %s (Checks run %s, attempt %s). No payload binaries were executed or rebuilt.\n' "$tag" "$sha" "$run_id" "$attempt"
if [[ "$create_draft" == false ]]; then
  printf 'Preview only. No GitHub changes made. Review %s/notes.md\n' "$metadata"
  exit 0
fi
stage='final source and version recheck'
fetch_run
cmp "$scratch/original-plan" "$scratch/plan"
fetch_evidence
cmp "$scratch/original-artifact-id" "$scratch/artifact-id"
fetch_unused
"$helper" release-promotion verify "$metadata" "$work/candidate" "$repo" "$run_id" "$tag"
"$helper" release-promotion assets "$metadata" "$work/assets"
stage='draft creation'
api --method POST "repos/$repo/releases" --input "$metadata/create-release.json" > "$metadata/created.json"
release_id=$("$helper" release-promotion created "$metadata/created.json" "$tag" "$sha")
stage='draft asset upload'
# No --clobber: partial/existing drafts require explicit human recovery.
gh release upload "$tag" "${assets[@]}" --repo "$repo"
stage='uploaded-byte verification'
api "repos/$repo/releases/$release_id" > "$metadata/uploaded.json"
gh release download "$tag" --repo "$repo" --dir "$work/uploaded" --pattern '*'
"$helper" release-promotion uploaded "$metadata" "$work/uploaded"
printf 'Draft prerelease created and uploaded bytes verified: https://github.com/%s/releases\nReview the draft and use the Publish button in GitHub when approved. The release remains unpublished.\n' "$repo"
