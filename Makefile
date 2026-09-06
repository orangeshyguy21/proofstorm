# Proofstorm — the single entrypoint for build, test, cluster, and gates.
#
# Everything here is sequencing and paths. Anything needing a conditional or a
# parser lives in Rust, so this file stays readable and cannot drift from the
# real types. The legacy Docker Compose harness lives in Makefile.compose and
# is reachable as `make compose-<target>`.

ROOT := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
TOOLS_DIR := $(ROOT).tools
BIN_DIR := $(TOOLS_DIR)/bin
DOWNLOAD_DIR := $(TOOLS_DIR)/downloads
ACCEPTANCE := $(ROOT)target/debug/proofstorm-acceptance

# The one source of pinned versions.
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
GATES := private-transfer slice2 slice4 slice5 native-exec cross-lab-scheduler \
	cross-implementation-wallet nutshell-mint nutshell-cln nutshell-postgres \
	cdk-cln cdk-ldk cdk-ldk-postgres cdk-postgres cdk-bdk-stress cdk-bdk-postgres \
	failed-melt quote-composition
# Excluded from `make e2e`: fails on a known upstream Nutshell defect.
EXPECTED_FAIL_GATES := nutshell-oidc
# Development checkpoints needing an image provisioned in the local registry.
LOCAL_IMAGE_GATES := private-handoff private-transfer cdk-wallet cdk-wallet-fees reliable-exec cocod-wallet cocod-projection

.PHONY: help build serve web web-tools web-dev test lint tools images images-build cluster-up docker-build docker-push install \
	deploy setup doctor cluster-schema e2e build-installer down clean-tools \
	$(addprefix e2e-,$(GATES) $(EXPECTED_FAIL_GATES) $(LOCAL_IMAGE_GATES))

help:
	@echo "Proofstorm targets:"
	@echo "  make setup            tools, cluster, catalog images, CRDs, controller, binaries, doctor"
	@echo "  make doctor           verify tools, cluster, controller, MCP discovery, and catalog image pulls"
	@echo "  make images           restore exact catalog images into the local registry"
	@echo "  make down             delete the local cluster and its registry"
	@echo ""
	@echo "  make build            build the web app, developer CLI, MCP server, and gate runner"
	@echo "  make serve            build, initialize, and start the website (PORT=8787; ARGS for global CLI options)"
	@echo "  make web              compile the Rust/Wasm web app"
	@echo "  make web-dev          hot-reload UI (run proofstorm serve separately)"
	@echo "  make test             hermetic workspace tests; needs no cluster"
	@echo "  make lint             formatting, strict Clippy, and Helm lint"
	@echo ""
	@echo "  make e2e              every live gate in order (needs an idle cluster)"
	@echo "  make e2e-<gate>       one live gate; gates are:"
	@echo "                        $(GATES)"
	@echo "                        $(EXPECTED_FAIL_GATES) (expected to fail, upstream defect)"
	@echo "                        $(LOCAL_IMAGE_GATES) (local arm64 wallet image required)"
	@echo ""
	@echo "  make docker-build     build the controller image"
	@echo "  make install          apply the CRDs"
	@echo "  make deploy           schema check, then Helm upgrade and rollout"
	@echo "  make build-installer  render dist/install.yaml for a release"
	@echo ""
	@echo "  make compose-<target> the legacy Compose harness in Makefile.compose"

# ---- build and check -------------------------------------------------------

build: web
	cargo build --locked -p proofstorm-app -p proofstorm-mcp -p proofstorm-acceptance
	cargo build --locked --release -p proofstorm-app -p proofstorm-mcp

serve: web
	cargo run --locked -p proofstorm-app -- init $(ARGS)
	cargo run --locked -p proofstorm-app -- serve --port $(PORT) $(ARGS)

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
	NO_COLOR=true $(BIN_DIR)/trunk build --release --locked --config $(ROOT)crates/proofstorm-web/Trunk.toml

# Run `proofstorm serve` on port 8787 first; Trunk proxies its API and event stream.
web-dev: web-tools
	NO_COLOR=true $(BIN_DIR)/trunk serve --config $(ROOT)crates/proofstorm-web/Trunk.toml

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

docker-build:
	docker build --file $(ROOT)Dockerfile.proofstormd --tag $(IMAGE) $(ROOT)

docker-push: docker-build
	docker push $(IMAGE)

# The Makefile is the sole CRD field owner. Helm skips chart CRD installation
# on both fresh installs and upgrades so server-side apply can reconcile the
# checked-in API before the controller that depends on it.
install: tools
	$(KUBECTL) apply --server-side --force-conflicts \
		--field-manager=proofstorm-make -f $(CHART)/crds

cluster-schema: build
	$(ACCEPTANCE) cluster-schema

deploy: install cluster-schema
	$(HELM) upgrade --install proofstorm $(CHART) \
		--kube-context $(CONTEXT) \
		--namespace $(CONTROL_NAMESPACE) --create-namespace --skip-crds \
		--set image.tag=$(PROOFSTORM_VERSION) \
		--rollback-on-failure --wait
	$(KUBECTL) rollout restart deployment/proofstormd -n $(CONTROL_NAMESPACE)
	$(KUBECTL) rollout status deployment/proofstormd -n $(CONTROL_NAMESPACE) --timeout=90s

images-build:
	cargo build --locked -p proofstorm-acceptance

images: cluster-up images-build
	$(ACCEPTANCE) images

setup: cluster-up images docker-push deploy build doctor

doctor: tools build
	@docker info >/dev/null
	@$(K3D) version | grep -F "$(patsubst v%,%,$(K3D_VERSION))" >/dev/null
	@$(BIN_DIR)/kubectl version --client | grep -F "$(patsubst v%,%,$(KUBECTL_VERSION))" >/dev/null
	@$(HELM) version --short | grep -F "$(HELM_VERSION)" >/dev/null
	@$(KUBECTL) get namespace $(CONTROL_NAMESPACE) >/dev/null
	@$(KUBECTL) wait --for=condition=Available deployment/proofstormd \
		-n $(CONTROL_NAMESPACE) --timeout=90s
	@$(ACCEPTANCE) doctor
	@$(ACCEPTANCE) images-check
	@echo "proofstorm doctor passed: tools, cluster, controller, MCP discovery, and catalog image pulls are healthy"

down: tools
	@$(K3D) cluster get proofstorm >/dev/null 2>&1 && $(K3D) cluster delete proofstorm || true
	@$(K3D) registry list 2>/dev/null | grep -F 'proofstorm-registry.localhost' >/dev/null && \
		$(K3D) registry delete proofstorm-registry.localhost || true

# ---- live acceptance gates -------------------------------------------------
#
# Gates assert that zero instance namespaces exist anywhere, so they need the
# cluster to themselves for their duration. Check before starting:
#   kubectl --context k3d-proofstorm get ns -l proofstorm.dev/instance

$(addprefix e2e-,$(GATES) $(EXPECTED_FAIL_GATES) $(LOCAL_IMAGE_GATES)): e2e-%: build
	$(ACCEPTANCE) $*

e2e: build
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
