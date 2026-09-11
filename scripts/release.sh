#!/usr/bin/env bash
# Human-facing release shortcuts. GitHub login + confirmation is the dispatch gate.
set -Eeuo pipefail
stage=arguments
trap 'printf "Release stopped during %s (line %s, status %s).\n" "$stage" "$LINENO" "$?" >&2' ERR
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
usage() { printf 'Usage: just release-prepare VERSION\n       just release [--preview | --yes]\nRelease prepares a draft through GitHub Actions; it never publishes.\n'; }
mode=${1:-draft}
[[ $# == 0 ]] || shift
preview=false confirmed=false version=''
case "$mode" in
  prepare) [[ $# == 1 ]] || { usage >&2; exit 2; }; version=$1 ;;
  draft)
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --preview) preview=true ;;
        --yes) confirmed=true ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
      esac
      shift
    done
    [[ "$preview" == false || "$confirmed" == false ]] || { usage >&2; exit 2; } ;;
  *) usage >&2; exit 2 ;;
esac
cd "$root"
for tool in cargo git; do command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 1; }; done
stage='clean checkout check'
checkout_status=$(git status --porcelain --untracked-files=normal)
[[ -z "$checkout_status" ]] || { printf 'Commit or stash changes first; release shortcuts require a clean checkout.\n' >&2; exit 1; }
if [[ "$mode" == draft ]]; then
  command -v gh >/dev/null || { printf 'Install GitHub CLI, then run gh auth login.\n' >&2; exit 1; }
  [[ "$(git symbolic-ref --short HEAD)" == main ]] || { printf 'Run just release from an up-to-date main checkout.\n' >&2; exit 1; }
  export GH_HOST=github.com GH_PROMPT_DISABLED=1
  # Resolve the repository from this checkout, never from a lingering override.
  unset GH_REPO
  stage='GitHub authentication'
  gh auth status --hostname github.com >/dev/null
fi
for variable in "${!PROOFSTORM_@}" "${!TRUNK_@}" "${!K3D_@}"; do [[ -z "$variable" ]] || unset "$variable"; done
unset CARGO_BUILD_TARGET CARGO_TARGET_DIR
stage='release tools'
printf 'Preparing release tools...\n'
export CARGO_TARGET_DIR="$root/target/check"
cargo build --quiet --locked --manifest-path "$root/Cargo.toml" -p proofstorm-xtask
helper="$CARGO_TARGET_DIR/debug/proofstorm-xtask"
if [[ "$mode" == prepare ]]; then
  stage='source version preparation'
  "$helper" release-shortcut prepare "$root" "$version"
  exit 0
fi
scratch=$(mktemp -d "${TMPDIR:-/tmp}/proofstorm-release.XXXXXXXX")
scratch=$(cd "$scratch" && pwd -P)
trap 'rm -rf -- "$scratch"' EXIT
api() { gh api -H 'X-GitHub-Api-Version: 2022-11-28' "$@"; }
stage='current main selection'
# gh's implicit repository may be a fork's upstream or a saved CLI default.
# Pin every read and dispatch to this checkout's origin, without changing defaults.
if ! origin=$(git remote get-url origin 2>/dev/null); then
  printf 'This checkout needs an origin remote to select its release repository.\n' >&2
  exit 1
fi
repo=$(gh repo view "$origin" --json nameWithOwner --jq .nameWithOwner)
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || { printf 'Invalid GitHub repository.\n' >&2; exit 1; }
api "repos/$repo/branches/main" > "$scratch/main.json"
sha=$("$helper" release-shortcut main "$scratch/main.json")
local_sha=$(git rev-parse HEAD)
if [[ "$local_sha" != "$sha" ]]; then
  printf 'Local main differs from %s main.\nLocal:  %s\nGitHub: %s\nUpdate your checkout, then rerun just release.\n' "$repo" "$local_sha" "$sha" >&2
  exit 1
fi
tag=$("$helper" release-shortcut version "$root")
stage='green build selection'
printf 'Finding matching tested Linux and Mac builds for %s...\n' "$tag"
api --paginate --slurp "repos/$repo/actions/workflows/check.yml/runs?branch=main&head_sha=$sha&per_page=100" > "$scratch/runs.json"
run_id=$("$helper" release-shortcut select "$scratch/runs.json" "$repo" "$sha")
api "repos/$repo/actions/runs/$run_id" > "$scratch/run.json"
api "repos/$repo/actions/workflows/check.yml" > "$scratch/workflow.json"
"$helper" release-promotion run "$scratch" "$repo" "$run_id" "$tag" > "$scratch/plan"
plan=()
while IFS= read -r -d '' field; do plan+=("$field"); done < "$scratch/plan"
[[ ${#plan[@]} == 4 && ${plan[0]} == "$sha" ]] || exit 1
attempt=${plan[1]}
api "repos/$repo/compare/$sha...main" > "$scratch/ancestry.json"
api --paginate --slurp "repos/$repo/actions/runs/$run_id/attempts/$attempt/jobs?per_page=100" > "$scratch/jobs.json"
api --paginate --slurp "repos/$repo/actions/runs/$run_id/artifacts?per_page=100" > "$scratch/artifacts.json"
"$helper" release-promotion evidence "$scratch" "$repo" "$run_id" "$tag" >/dev/null
api "repos/$repo/git/matching-refs/tags/$tag" > "$scratch/refs.json"
api --paginate --slurp "repos/$repo/releases?per_page=100" > "$scratch/releases.json"
"$helper" release-promotion unused "$scratch" "$tag"
printf '\nDraft: %s — Linux AMD64 + macOS Apple Silicon\nRepository: %s\nCommit: %s\nBuild: https://github.com/%s/actions/runs/%s (attempt %s)\nGitHub will verify both bundles, create one draft, and check uploaded bytes. Nothing will publish automatically.\n' "$tag" "$repo" "$sha" "$repo" "$run_id" "$attempt"
if [[ "$preview" == true ]]; then printf 'Preview only. No workflow dispatched or GitHub changes made.\n'; exit 0; fi
if [[ "$confirmed" == false ]]; then
  [[ -t 0 ]] || { printf 'Confirmation needs a terminal. Use --preview to inspect, or --yes to authorize draft preparation.\n' >&2; exit 1; }
  printf 'Prepare this draft using your GitHub login? [y/N] '
  IFS= read -r answer || answer=''
  case "$answer" in y|Y|yes|YES) ;; *) printf 'Cancelled. No GitHub changes made.\n'; exit 0 ;; esac
fi
stage='final main recheck'
api "repos/$repo/branches/main" > "$scratch/main.json"
[[ "$("$helper" release-shortcut main "$scratch/main.json")" == "$sha" ]] || { printf 'Main changed while confirming. Rerun to review the new build.\n' >&2; exit 1; }
"$helper" release-shortcut dispatch "$run_id" "$tag" > "$scratch/dispatch.json"
stage='authenticated workflow dispatch'
if ! api --method POST "repos/$repo/actions/workflows/alpha-release.yml/dispatches" --input "$scratch/dispatch.json"; then
  printf 'Dispatch was not confirmed. Check Actions before retrying; GitHub may have accepted it.\n' >&2
  exit 1
fi
printf 'Draft preparation requested, not yet completed. Follow the run:\nhttps://github.com/%s/actions/workflows/alpha-release.yml\nAfter it passes, review the draft in Releases and publish when approved.\n' "$repo"
