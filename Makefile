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

# Pinned tools win over anything already on PATH.
export PATH := $(BIN_DIR):$(PATH)
KUBECTL := $(BIN_DIR)/kubectl --context $(CONTEXT)
HELM := $(BIN_DIR)/helm
K3D := $(BIN_DIR)/k3d

PLATFORM_OS := $(shell uname -s | tr '[:upper:]' '[:lower:]')
PLATFORM_ARCH := $(shell uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')

# Every gate the acceptance runner knows, in the plan's port order.
GATES := slice2 slice4 slice5 native-exec cross-lab-scheduler \
	cross-implementation-wallet nutshell-mint nutshell-cln nutshell-postgres \
	cdk-cln cdk-ldk cdk-ldk-postgres cdk-postgres cdk-bdk-stress cdk-bdk-postgres \
	failed-melt quote-composition
# Excluded from `make e2e`: fails on a known upstream Nutshell defect.
EXPECTED_FAIL_GATES := nutshell-oidc
# Development checkpoints needing an image provisioned in the local registry.
LOCAL_IMAGE_GATES := cdk-wallet cdk-wallet-fees reliable-exec

.PHONY: help build test lint tools cluster-up docker-build docker-push install \
	deploy setup doctor cluster-schema e2e build-installer down clean-tools \
	$(addprefix e2e-,$(GATES) $(EXPECTED_FAIL_GATES) $(LOCAL_IMAGE_GATES))

help:
	@echo "Proofstorm targets:"
	@echo "  make setup            tools, cluster, image, CRDs, controller, binaries, doctor"
	@echo "  make doctor           verify pinned tools, cluster, controller, and MCP discovery"
	@echo "  make down             delete the local cluster and its registry"
	@echo ""
	@echo "  make build            build the MCP server, controller, and gate runner"
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

build:
	cargo build --locked -p proofstorm-mcp -p proofstorm-acceptance
	cargo build --locked --release -p proofstorm-mcp

test:
	cargo test --workspace --all-targets

lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings
	$(HELM) lint $(CHART)

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
		--rollback-on-failure --wait
	$(KUBECTL) rollout restart deployment/proofstormd -n $(CONTROL_NAMESPACE)
	$(KUBECTL) rollout status deployment/proofstormd -n $(CONTROL_NAMESPACE) --timeout=90s

setup: cluster-up docker-push deploy build doctor

doctor: tools build
	@docker info >/dev/null
	@$(K3D) version | grep -F "$(patsubst v%,%,$(K3D_VERSION))" >/dev/null
	@$(BIN_DIR)/kubectl version --client | grep -F "$(patsubst v%,%,$(KUBECTL_VERSION))" >/dev/null
	@$(HELM) version --short | grep -F "$(HELM_VERSION)" >/dev/null
	@$(KUBECTL) get namespace $(CONTROL_NAMESPACE) >/dev/null
	@$(KUBECTL) wait --for=condition=Available deployment/proofstormd \
		-n $(CONTROL_NAMESPACE) --timeout=90s
	@$(ACCEPTANCE) doctor
	@echo "proofstorm doctor passed: pinned tools, cluster, controller, and MCP discovery are healthy"

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
