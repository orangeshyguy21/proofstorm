#!/usr/bin/env bash
# Exercise the Dockerfile's cache invalidation with backdated source snapshots.
# Uses tiny, offline Rust crates; no Docker daemon or runtime is needed.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
export CARGO_TARGET_DIR="$scratch/target"
unset CARGO_BUILD_TARGET
mkdir -p "$scratch/crates" "$scratch/dependencies/fixture-dependency/src"
cat > "$scratch/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/*"]
exclude = ["dependencies/fixture-dependency"]
resolver = "2"
EOF
cat > "$scratch/dependencies/fixture-dependency/Cargo.toml" <<'EOF'
[package]
name = "fixture-dependency"
version = "0.1.0"
edition = "2021"
EOF
printf 'pub const MARKER: &str = "dependency";\n' > "$scratch/dependencies/fixture-dependency/src/lib.rs"
for package in proofstorm-core proofstorm-kube proofstorm-transfer proofstorm-exec proofstorm-driver proofstorm-prober proofstormd; do
  mkdir -p "$scratch/crates/$package/src"
  printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' "$package" > "$scratch/crates/$package/Cargo.toml"
  printf 'pub const MARKER: &str = "fixture";\n' > "$scratch/crates/$package/src/lib.rs"
done
cat >> "$scratch/crates/proofstorm-core/Cargo.toml" <<'EOF'
[dependencies]
fixture-dependency = { path = "../../dependencies/fixture-dependency" }
EOF
cat > "$scratch/crates/proofstorm-core/src/lib.rs" <<'EOF'
pub fn contract() -> String { format!("{}:{}", include_str!("../../../contract.txt").trim(), fixture_dependency::MARKER) }
EOF
cat >> "$scratch/crates/proofstorm-kube/Cargo.toml" <<'EOF'
[dependencies]
proofstorm-core = { path = "../proofstorm-core" }
EOF
printf 'pub use proofstorm_core::contract;\n' > "$scratch/crates/proofstorm-kube/src/lib.rs"
cat >> "$scratch/crates/proofstormd/Cargo.toml" <<'EOF'
[dependencies]
proofstorm-kube = { path = "../proofstorm-kube" }
EOF
cat > "$scratch/crates/proofstormd/src/main.rs" <<'EOF'
fn main() { println!("{}:{}", env!("CACHE_FIXTURE_REVISION"), proofstorm_kube::contract()); }
EOF
printf 'showcase\n' > "$scratch/contract.txt"
build() {
  CACHE_FIXTURE_REVISION=$1 cargo build --offline --release -vv \
    --manifest-path "$scratch/Cargo.toml" -p proofstormd > "$scratch/build.log" 2>&1 || {
    cat "$scratch/build.log" >&2; return 1;
  }
}
build showcase
# The root binary recompiles because its embedded revision changes, but Cargo
# can reuse transitive workspace artifacts whose source appears older.
printf 'main\n' > "$scratch/contract.txt"
touch -t 200001010000 "$scratch/contract.txt"
build main
# Read the actual controller-stage cleanup, including continued argument lines.
clean=$(awk '
  /^FROM / { controller = ($0 ~ / AS build$/) }
  controller && /cargo clean --release/ {
    line = $0
    while (line ~ /\\$/) { sub(/\\$/, "", line); getline; line = line " " $0 }
    sub(/ &&.*$/, "", line)
    print line
    exit
  }
' "$root/Dockerfile.proofstormd")
[[ -n "$clean" ]] || { printf 'Missing controller cache invalidation\n' >&2; exit 1; }
read -r -a clean_args <<< "$clean"
(cd "$scratch" && "${clean_args[@]}" --offline) > "$scratch/clean.log" 2>&1 || {
  cat "$scratch/clean.log" >&2; exit 1;
}
build main
[[ $("$CARGO_TARGET_DIR/release/proofstormd") == main:main:dependency ]] || {
  printf 'Controller reused a stale transitive workspace contract\n' >&2; exit 1;
}
grep -q 'Fresh fixture-dependency' "$scratch/build.log" || {
  printf 'Controller unnecessarily rebuilt its non-workspace dependency\n' >&2; exit 1;
}
printf 'Controller cache regression passed\n'
