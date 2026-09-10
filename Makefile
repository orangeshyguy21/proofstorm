# Proofstorm — the single entrypoint for build, test, cluster, and gates.
#
# Product operations live in the Rust CLI; the development helper only builds
# and registers checkout artifacts. Remaining legacy targets are marked below.
# The legacy Docker Compose harness lives in Makefile.compose and
# is reachable as `make compose-<target>`.

ROOT := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
TOOLS_DIR := $(ROOT).tools
BIN_DIR := $(TOOLS_DIR)/bin
DOWNLOAD_DIR := $(TOOLS_DIR)/downloads
ACCEPTANCE := $(ROOT)target/debug/proofstorm-acceptance
DEV_CLI := $(ROOT).proofstorm-dev/bin/proofstorm

# Normal commands explicitly select this checkout through DEV_CLI. Remaining
# low-level legacy gate targets must not inherit another installation selection.
unexport PROOFSTORM_HOME PROOFSTORM_KUBECONFIG

# Pinned host tools and Proofstorm release version; component images live in the catalog.
include $(ROOT)tools/versions.env

CONTEXT := k3d-proofstorm
CONTROL_NAMESPACE := proofstorm-system
REGISTRY := localhost:5111
IMAGE := $(REGISTRY)/proofstormd:$(PROOFSTORM_VERSION)
CHART := $(ROOT)charts/proofstorm
PORT ?= 8787

# Pinned tools win over anything already on PATH.
export PATH := $(BIN_DIR):$(PATH)
KUBECTL := $(BIN_DIR)/kubectl --context $(CONTEXT)
HELM := $(BIN_DIR)/helm
K3D := $(BIN_DIR)/k3d

PLATFORM_OS := $(shell uname -s | tr '[:upper:]' '[:lower:]')
PLATFORM_ARCH := $(shell uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')

# Every gate the acceptance runner knows, in the plan's port order.
GATES := mint-management private-transfer slice2 slice4 slice5 controller-recovery network-faults channel-lifecycle native-exec cross-lab-scheduler \
	cross-implementation-wallet nutshell-mint nutshell-cln nutshell-postgres \
	cdk-cln cdk-ldk cdk-ldk-postgres cdk-postgres cdk-bdk-stress cdk-bdk-postgres \
	failed-melt quote-composition dynamic-lab
# Excluded from `make e2e`: fails on a known upstream Nutshell defect.
EXPECTED_FAIL_GATES := nutshell-oidc
# Development checkpoints needing an image provisioned in the local registry.
LOCAL_IMAGE_GATES := private-handoff private-transfer cdk-wallet cdk-wallet-fees reliable-exec cocod-wallet cocod-projection

.PHONY: help dev dev-build build legacy-gate-build serve gui stop web web-tools web-dev test lint tools images images-build cluster-up \
	deploy setup doctor e2e build-installer down clean-tools \
	$(addprefix e2e-,$(GATES) $(EXPECTED_FAIL_GATES) $(LOCAL_IMAGE_GATES))

help:
	@echo "Proofstorm targets:"
	@echo "  make dev              build and enter a shell selecting the checkout installation"
	@echo "  make dev-build        rebuild/register checkout artifacts; preserve labs and permissions"
	@echo "  make setup            build, then run the product CLI setup for this checkout"
	@echo "  make doctor           run the product CLI doctor for this checkout"
	@echo "  make deploy           alias for setup: build/verify/deploy the local controller"
	@echo ""
	@echo "  make build            alias for make dev-build"
	@echo "  make gui / serve      open the checkout's managed GUI (run make setup first)"
	@echo "  make stop             stop that GUI, leaving labs running"
	@echo "  make web              rebuild managed GUI assets; refresh its browser tab"
	@echo "  make web-dev          watch UI assets; refresh the managed GUI after each build"
	@echo "  make test             hermetic workspace tests; needs no cluster"
	@echo "  make lint             formatting, strict Clippy, and Helm lint"
	@echo ""
	@echo "  Remaining legacy targets (NOT the checkout installation; consolidation pending):"
	@echo "  make images / down    legacy registry restore / legacy runtime teardown"
	@echo "  make e2e              every legacy live gate in order (needs an idle legacy cluster)"
	@echo "  make e2e-<gate>       one live gate; gates are:"
	@echo "                        $(GATES)"
	@echo "                        $(EXPECTED_FAIL_GATES) (expected to fail, upstream defect)"
	@echo "                        $(LOCAL_IMAGE_GATES) (local arm64 wallet image required)"
	@echo ""
	@echo "  make build-installer  render dist/install.yaml for a release"
	@echo ""
	@echo "  make compose-<target> the legacy Compose harness in Makefile.compose"

# ---- build and check -------------------------------------------------------

dev: web-tools
	python3 $(ROOT)scripts/develop.py --shell $(DEV_ARGS)

dev-build: web-tools
	python3 $(ROOT)scripts/develop.py $(DEV_ARGS)

build: dev-build

# Acceptance's remaining legacy runtime assumptions are not part of make dev.
legacy-gate-build: web
	PROOFSTORM_WEB_DIST="$(ROOT).proofstorm-dev/web" cargo build --locked -p proofstorm-app -p proofstorm-mcp -p proofstorm-acceptance

serve: gui

gui:
	"$(DEV_CLI)" gui $(ARGS)

stop:
	"$(DEV_CLI)" stop

test:
	cargo test --workspace --all-targets

lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings
	$(HELM) lint $(CHART)

# The browser stays Rust. Build it before embedding assets in the CLI binary.
web-tools:
	sh $(ROOT)tools/install-trunk.sh
	rustup target add wasm32-unknown-unknown

web: web-tools
	python3 $(ROOT)scripts/develop.py --web-only $(DEV_ARGS)

# Watched assets are served by the same managed/authenticated backend as releases.
web-dev: web-tools
	python3 $(ROOT)scripts/develop.py --watch-web $(DEV_ARGS)

# ---- pinned tools ----------------------------------------------------------

tools: $(BIN_DIR)/k3d $(BIN_DIR)/kubectl $(BIN_DIR)/helm

$(BIN_DIR)/k3d:
	@mkdir -p $(BIN_DIR) $(DOWNLOAD_DIR)
	@echo "[proofstorm] downloading k3d $(K3D_VERSION)"
	@curl --fail --location --retry 3 --silent --show-error \
		"https://github.com/k3d-io/k3d/releases/download/$(K3D_VERSION)/k3d-$(PLATFORM_OS)-$(PLATFORM_ARCH)" \
		--output "$(DOWNLOAD_DIR)/k3d"
	@curl --fail --location --retry 3 --silent --show-error \
		"https://github.com/k3d-io/k3d/releases/download/$(K3D_VERSION)/checksums.txt" \
		--output "$(DOWNLOAD_DIR)/k3d-checksums.txt"
	@expected=$$(awk -v name="_dist/k3d-$(PLATFORM_OS)-$(PLATFORM_ARCH)" '$$2 == name {print $$1}' \
		"$(DOWNLOAD_DIR)/k3d-checksums.txt"); \
	  test -n "$$expected" || { echo "k3d checksum entry was not published" >&2; exit 1; }; \
	  actual=$$(shasum -a 256 "$(DOWNLOAD_DIR)/k3d" | awk '{print $$1}'); \
	  test "$$expected" = "$$actual" || { echo "k3d checksum mismatch" >&2; exit 1; }
	@install -m 0755 "$(DOWNLOAD_DIR)/k3d" "$@"

$(BIN_DIR)/kubectl:
	@mkdir -p $(BIN_DIR) $(DOWNLOAD_DIR)
	@echo "[proofstorm] downloading kubectl $(KUBECTL_VERSION)"
	@curl --fail --location --retry 3 --silent --show-error \
		"https://dl.k8s.io/release/$(KUBECTL_VERSION)/bin/$(PLATFORM_OS)/$(PLATFORM_ARCH)/kubectl" \
		--output "$(DOWNLOAD_DIR)/kubectl"
	@curl --fail --location --retry 3 --silent --show-error \
		"https://dl.k8s.io/release/$(KUBECTL_VERSION)/bin/$(PLATFORM_OS)/$(PLATFORM_ARCH)/kubectl.sha256" \
		--output "$(DOWNLOAD_DIR)/kubectl.sha256"
	@expected=$$(tr -d '[:space:]' < "$(DOWNLOAD_DIR)/kubectl.sha256"); \
	  actual=$$(shasum -a 256 "$(DOWNLOAD_DIR)/kubectl" | awk '{print $$1}'); \
	  test "$$expected" = "$$actual" || { echo "kubectl checksum mismatch" >&2; exit 1; }
	@install -m 0755 "$(DOWNLOAD_DIR)/kubectl" "$@"

$(BIN_DIR)/helm:
	@mkdir -p $(BIN_DIR) $(DOWNLOAD_DIR)
	@echo "[proofstorm] downloading helm $(HELM_VERSION)"
	@curl --fail --location --retry 3 --silent --show-error \
		"https://get.helm.sh/helm-$(HELM_VERSION)-$(PLATFORM_OS)-$(PLATFORM_ARCH).tar.gz" \
		--output "$(DOWNLOAD_DIR)/helm.tar.gz"
	@curl --fail --location --retry 3 --silent --show-error \
		"https://get.helm.sh/helm-$(HELM_VERSION)-$(PLATFORM_OS)-$(PLATFORM_ARCH).tar.gz.sha256sum" \
		--output "$(DOWNLOAD_DIR)/helm.tar.gz.sha256sum"
	@expected=$$(awk '{print $$1}' "$(DOWNLOAD_DIR)/helm.tar.gz.sha256sum"); \
	  actual=$$(shasum -a 256 "$(DOWNLOAD_DIR)/helm.tar.gz" | awk '{print $$1}'); \
	  test "$$expected" = "$$actual" || { echo "helm checksum mismatch" >&2; exit 1; }
	@unpack=$$(mktemp -d); \
	  tar -xzf "$(DOWNLOAD_DIR)/helm.tar.gz" -C "$$unpack"; \
	  install -m 0755 "$$unpack/$(PLATFORM_OS)-$(PLATFORM_ARCH)/helm" "$@"; \
	  rm -rf -- "$$unpack"

clean-tools:
	rm -rf $(TOOLS_DIR)

# ---- cluster lifecycle -----------------------------------------------------

cluster-up: tools
	@$(K3D) cluster get proofstorm >/dev/null 2>&1 || \
		$(K3D) cluster create --config $(ROOT)infra/k3d/proofstorm.yaml

deploy: setup

images-build:
	cargo build --locked -p proofstorm-acceptance

# Explicit packaging step: review the resulting digest before changing the catalog.
# make images restores exact artifacts; it never rebuilds a reviewed image silently.
.PHONY: bitcoin-image-build
bitcoin-image-build: cluster-up
	@mkdir -p $(DOWNLOAD_DIR)
	docker buildx build --platform linux/amd64,linux/arm64 --provenance=false \
		--file $(ROOT)docker/bitcoin/Dockerfile --tag $(REGISTRY)/bitcoin-core:31.1 \
		--metadata-file $(DOWNLOAD_DIR)/bitcoin-31.1-build.json --push $(ROOT)docker/bitcoin

images: cluster-up images-build
	$(ACCEPTANCE) images

setup: dev-build
	"$(DEV_CLI)" setup $(ARGS)

doctor:
	"$(DEV_CLI)" doctor $(ARGS)

down: tools
	@$(K3D) cluster get proofstorm >/dev/null 2>&1 && $(K3D) cluster delete proofstorm || true
	@$(K3D) registry list 2>/dev/null | grep -F 'proofstorm-registry.localhost' >/dev/null && \
		$(K3D) registry delete proofstorm-registry.localhost || true

# ---- live acceptance gates -------------------------------------------------
#
# Most gates assert that zero instance namespaces exist and need an idle cluster.
# dynamic-lab scopes cleanup to its own disposable lab and can coexist with other labs.
# Check before running the other gates:
#   kubectl --context k3d-proofstorm get ns -l proofstorm.dev/instance

$(addprefix e2e-,$(sort $(GATES) $(EXPECTED_FAIL_GATES) $(LOCAL_IMAGE_GATES))): e2e-%: legacy-gate-build
	$(ACCEPTANCE) $*

e2e: legacy-gate-build
	@for gate in $(GATES); do \
		echo "[proofstorm] gate $$gate"; \
		$(ACCEPTANCE) $$gate || exit 1; \
	done
	@echo "[proofstorm] all $(words $(GATES)) gates passed"

# ---- release ---------------------------------------------------------------

build-installer: tools
	@mkdir -p $(ROOT)dist
	@cat $(CHART)/crds/*.yaml > $(ROOT)dist/install.yaml
	@echo "---" >> $(ROOT)dist/install.yaml
	@$(HELM) template proofstorm $(CHART) --namespace $(CONTROL_NAMESPACE) \
		>> $(ROOT)dist/install.yaml
	@echo "[proofstorm] wrote dist/install.yaml"

# ---- legacy Compose harness ------------------------------------------------

compose-%:
	@$(MAKE) -f $(ROOT)Makefile.compose $*
