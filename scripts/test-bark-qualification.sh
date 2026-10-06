#!/usr/bin/env bash
# Preflight must refuse a developer checkout or a non-native host before writes.
set -Eeuo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir -p "$scratch/checkout/scripts" "$scratch/bin"
cp "$root/scripts/bark-qualification.sh" "$scratch/checkout/scripts/"
cat > "$scratch/bin/uname" <<'SH'
#!/usr/bin/env bash
case "$1" in -s) echo "${BARK_TEST_OS:-Linux}" ;; -m) echo "${BARK_TEST_MACHINE:-x86_64}" ;; *) exit 97 ;; esac
SH
cat > "$scratch/bin/git" <<'SH'
#!/usr/bin/env bash
[[ "$3 $4" == 'status --porcelain' ]] || exit 97
echo ' M existing-work'
SH
chmod +x "$scratch/bin/"*
export PATH="$scratch/bin:$PATH"
run() {
  bash "$scratch/checkout/scripts/bark-qualification.sh" "$1" aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 "$scratch/artifacts" "$scratch/work" > "$scratch/log" 2>&1
}
unset GITHUB_ACTIONS RUNNER_ENVIRONMENT
if run linux/amd64; then exit 1; fi
export GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted
if run linux/arm64; then exit 1; fi
if run linux/foreign; then exit 1; fi
export BARK_TEST_OS=Darwin BARK_TEST_MACHINE=arm64
if run linux/arm64; then exit 1; fi
export BARK_TEST_OS=Linux BARK_TEST_MACHINE=x86_64
mkdir "$scratch/checkout/.proofstorm-dev"
if run linux/amd64; then exit 1; fi
rmdir "$scratch/checkout/.proofstorm-dev"
if run linux/amd64; then exit 1; fi
grep -Fq 'Qualification source must start clean' "$scratch/log"
[[ ! -e "$scratch/work" ]]
echo 'Managed Bark native host and developer-checkout refusal checks passed'
