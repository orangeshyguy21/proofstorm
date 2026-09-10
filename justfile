# Developer commands; product operations remain in the Rust CLI.

set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments := true

root := justfile_directory()
export PATH := root / ".tools/bin" + ":" + env_var("PATH")

unexport PROOFSTORM_HOME
unexport PROOFSTORM_KUBECONFIG

# List available commands without building or starting anything.
default:
    @just --list

alias help := default
alias build := dev-build
alias deploy := setup
alias serve := gui

# Build and enter a shell selecting the checkout installation.
dev *args: web-tools
    bash scripts/develop.sh --shell "$@"

# Rebuild/register checkout artifacts; preserve labs and permissions.
dev-build *args: web-tools
    bash scripts/develop.sh "$@"

# Build, then set up the checkout runtime.
setup *args: dev-build
    .proofstorm-dev/bin/proofstorm setup "$@"

# Check the checkout runtime.
doctor *args:
    .proofstorm-dev/bin/proofstorm doctor "$@"

# Open the managed GUI without rebuilding (run setup first).
gui *args:
    .proofstorm-dev/bin/proofstorm gui "$@"

# Stop the managed GUI, leaving labs running.
stop:
    .proofstorm-dev/bin/proofstorm stop

# Run the same quick checks, lints, and tests as CI; no runtime needed.
check:
    bash scripts/check.sh

# Check formatting and shell tooling before compiling Rust.
check-quick:
    bash scripts/check.sh quick

# Strict Clippy followed by hermetic workspace tests.
check-rust:
    bash scripts/check.sh rust

# Hermetic workspace tests only.
test:
    bash scripts/check.sh test

# Formatting, shell checks, and strict Clippy.
lint:
    bash scripts/check.sh quick
    bash scripts/check.sh clippy

# Chart validation (requires the pinned Helm tool).
lint-helm:
    .tools/bin/helm lint charts/proofstorm

# Install the pinned web builder and Rust browser target.
web-tools:
    sh tools/install-trunk.sh
    rustup target add wasm32-unknown-unknown

# Rebuild managed GUI assets once.
web *args: web-tools
    bash scripts/develop.sh --web-only "$@"

# Watch GUI assets; refresh the managed browser tab after builds.
web-dev *args: web-tools
    bash scripts/develop.sh --watch-web "$@"

# Install pinned maintainer tools (not required by code checks).
tools:
    bash tools/install-host-tools.sh

# Remove downloaded checkout tools, not installation state or labs.
clean-tools:
    rm -rf -- .tools

# LEGACY: build the acceptance runner and its embedded GUI.
[group('legacy')]
legacy-gate-build: web
    PROOFSTORM_WEB_DIST="$PWD/.proofstorm-dev/web" cargo build --locked -p proofstorm-app -p proofstorm-mcp -p proofstorm-acceptance

# LEGACY: create the fixed k3d-proofstorm cluster, not the checkout runtime.
[group('legacy')]
cluster-up: tools
    .tools/bin/k3d cluster get proofstorm >/dev/null 2>&1 || .tools/bin/k3d cluster create --config infra/k3d/proofstorm.yaml

# LEGACY: build the image-restoration command.
[group('legacy')]
images-build:
    cargo build --locked -p proofstorm-acceptance

# LEGACY: restore exact catalog images without rebuilding them.
[group('legacy')]
images: cluster-up images-build
    target/debug/proofstorm-acceptance images

# LEGACY: build/publish Bitcoin into the fixed local registry.
[group('legacy')]
bitcoin-image-build: cluster-up
    mkdir -p .tools/downloads
    docker buildx build --platform linux/amd64,linux/arm64 --provenance=false --file docker/bitcoin/Dockerfile --tag localhost:5111/bitcoin-core:31.1 --metadata-file .tools/downloads/bitcoin-31.1-build.json --push docker/bitcoin

# LEGACY: delete the fixed cluster and registry, not the checkout runtime.
[group('legacy')]
down: tools
    .tools/bin/k3d cluster get proofstorm >/dev/null 2>&1 && .tools/bin/k3d cluster delete proofstorm || true
    .tools/bin/k3d registry list 2>/dev/null | grep -F 'proofstorm-registry.localhost' >/dev/null && .tools/bin/k3d registry delete proofstorm-registry.localhost || true

# LEGACY: run named gates, or the default suite (requires an idle legacy cluster).
[group('legacy')]
e2e *gates: legacy-gate-build
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ $# == 0 ]]; then
      # Known upstream failure nutshell-oidc and local-image gates stay opt-in.
      set -- mint-management private-transfer slice2 slice4 slice5 controller-recovery \
        network-faults channel-lifecycle native-exec cross-lab-scheduler \
        cross-implementation-wallet nutshell-mint nutshell-cln nutshell-postgres \
        cdk-cln cdk-ldk cdk-ldk-postgres cdk-postgres cdk-bdk-stress cdk-bdk-postgres \
        failed-melt quote-composition dynamic-lab
    fi
    for gate in "$@"; do
      printf '[proofstorm] gate %s\n' "$gate"
      target/debug/proofstorm-acceptance "$gate"
    done
    printf '[proofstorm] all %s gates passed\n' "$#"

# Render the standalone Kubernetes installer manifest.
build-installer: tools
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    cat charts/proofstorm/crds/*.yaml > dist/install.yaml
    printf '\n---\n' >> dist/install.yaml
    .tools/bin/helm template proofstorm charts/proofstorm --namespace proofstorm-system >> dist/install.yaml
    printf '[proofstorm] wrote dist/install.yaml\n'

# LEGACY: invoke the unchanged Compose harness; only this recipe still needs Make.
[group('legacy')]
compose +args:
    make -f Makefile.compose "$@"
