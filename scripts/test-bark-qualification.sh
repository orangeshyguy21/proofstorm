#!/usr/bin/env bash
# Preflight must refuse a developer checkout or a non-native host before writes.
# Covers the shared stack-qualification.sh through Bark's entry point, for Spark,
# and the --local mode that never stages pins in the developer checkout.
set -Eeuo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir -p "$scratch/checkout/scripts" "$scratch/bin"
cp "$root/scripts/bark-qualification.sh" "$root/scripts/stack-qualification.sh" "$scratch/checkout/scripts/"
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
original_path=$PATH
export PATH="$scratch/bin:$PATH"
revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
run() {
  bash "$scratch/checkout/scripts/bark-qualification.sh" "$1" "$revision" 1 "$scratch/artifacts" "$scratch/work" > "$scratch/log" 2>&1
}
stack_run() {
  bash "$scratch/checkout/scripts/stack-qualification.sh" "$@" "$revision" 1 "$scratch/artifacts" "$scratch/work" > "$scratch/log" 2>&1
}
unset GITHUB_ACTIONS RUNNER_ENVIRONMENT
if run linux/amd64; then exit 1; fi
if stack_run spark linux/amd64; then exit 1; fi
grep -Fq 'Use a fresh GitHub-hosted checkout' "$scratch/log"
export GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted
if run linux/arm64; then exit 1; fi
if run linux/foreign; then exit 1; fi
if stack_run lnd linux/amd64; then exit 1; fi
if stack_run spark linux/arm64; then exit 1; fi
export BARK_TEST_OS=Darwin BARK_TEST_MACHINE=arm64
if run linux/arm64; then exit 1; fi
if stack_run spark linux/arm64; then exit 1; fi
grep -Fq 'use --local on this Mac' "$scratch/log"
export BARK_TEST_OS=Linux BARK_TEST_MACHINE=x86_64
mkdir "$scratch/checkout/.proofstorm-dev"
if run linux/amd64; then exit 1; fi
if stack_run spark linux/amd64; then exit 1; fi
rmdir "$scratch/checkout/.proofstorm-dev"
if run linux/amd64; then exit 1; fi
grep -Fq 'Qualification source must start clean' "$scratch/log"
if stack_run spark linux/amd64; then exit 1; fi
grep -Fq 'Qualification source must start clean' "$scratch/log"
# --local needs no hosted runner and tolerates a developer installation, which
# it only reads, but still requires a native host and a clean committed source.
unset GITHUB_ACTIONS RUNNER_ENVIRONMENT
mkdir "$scratch/checkout/.proofstorm-dev"
export BARK_TEST_OS=Darwin BARK_TEST_MACHINE=arm64
if stack_run spark --local linux/amd64; then exit 1; fi
grep -Fq 'requires the matching native Linux runner' "$scratch/log"
export BARK_TEST_MACHINE=x86_64
if stack_run spark --local linux/arm64; then exit 1; fi
grep -Fq 'requires the matching native Linux runner' "$scratch/log"
export BARK_TEST_MACHINE=arm64
for stack in spark bark; do
  if stack_run "$stack" --local linux/arm64; then exit 1; fi
  grep -Fq 'Qualification source must start clean' "$scratch/log"
done
export BARK_TEST_OS=Linux BARK_TEST_MACHINE=aarch64
if stack_run spark --local linux/arm64; then exit 1; fi
grep -Fq 'Qualification source must start clean' "$scratch/log"
[[ ! -e "$scratch/work" ]]
# A clean local run clones the committed checkout and builds only in the clone.
real="$scratch/real checkout"
mkdir -p "$real/scripts" "$scratch/local-bin"
cp "$root/scripts/stack-qualification.sh" "$real/scripts/"
cp "$scratch/bin/uname" "$scratch/local-bin/"
cat > "$scratch/local-bin/cargo" <<'SH'
#!/usr/bin/env bash
printf '%s\n%s\n' "$PWD" "$CARGO_TARGET_DIR" > "$BARK_TEST_CARGO"
exit 42
SH
chmod +x "$scratch/local-bin/cargo"
PATH="$scratch/local-bin:$original_path"
git -C "$real" init -q
git -C "$real" add .
git -C "$real" -c user.name=Fixture -c user.email=fixture@example.invalid \
  -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture
head=$(git -C "$real" rev-parse HEAD)
export BARK_TEST_CARGO="$scratch/cargo-call"
status=0
bash "$real/scripts/stack-qualification.sh" spark --local linux/arm64 "$revision" 1 \
  "$scratch/artifacts" "$scratch/local-work" > "$scratch/log" 2>&1 || status=$?
[[ "$status" == 42 ]]
clone="$scratch/local-work/checkout"
[[ "$(git -C "$clone" rev-parse HEAD)" == "$head" && -z $(git -C "$real" status --porcelain) ]]
[[ "$(sed -n 1p "$BARK_TEST_CARGO")" == "$(cd "$clone" && pwd -P)" ]]
[[ "$(sed -n 2p "$BARK_TEST_CARGO")" == "$(cd "$clone" && pwd -P)/target/check" ]]
jq -e '.stage=="restore" and .exit_status==42 and .published==false' "$scratch/local-work/public/status.json" >/dev/null
echo 'Managed Bark and Spark native host, developer-checkout and local-clone refusal checks passed'
