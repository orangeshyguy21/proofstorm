# proofstorm — wallet-population runner
# Usage: make up && make smoke && make down

ROOT := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

# Command-line / shell env overrides must survive `include .env` (plain `=` in
# included files otherwise wins over `WALLET_IMPL=nutshell make …`).
_CMD_WALLET_IMPL := $(WALLET_IMPL)

include $(ROOT).env.example
-include $(ROOT).env

ifneq ($(_CMD_WALLET_IMPL),)
WALLET_IMPL := $(_CMD_WALLET_IMPL)
endif

export N_WALLETS FUND_AMOUNT MINT_IMPL WALLET_IMPL SWAP_AMOUNT
export CDK_MINTD_VERSION CDK_CLI_VERSION NUTSHELL_VERSION MINT_HOST_PORT UNIT
export CONSERVATION_EXPECTED CONSERVATION_TOLERANCE CONSERVATION_SWAP_TOLERANCE

N_WALLETS ?= 3
MAX_WALLETS := 10

ifeq ($(shell test $(N_WALLETS) -ge 1 -a $(N_WALLETS) -le $(MAX_WALLETS) && echo ok),)
$(error N_WALLETS must be 1..$(MAX_WALLETS))
endif

# Wallet implementation selects image + data directory inside containers.
ifeq ($(WALLET_IMPL),nutshell)
WALLET_DOCKERFILE := docker/wallet/Dockerfile.nutshell
WALLET_DATA_DIR := /root/.cashu
else ifeq ($(WALLET_IMPL),cdk)
WALLET_DOCKERFILE := docker/wallet/Dockerfile.cdk
WALLET_DATA_DIR := /root/.cdk
else
$(error WALLET_IMPL must be cdk or nutshell (got $(WALLET_IMPL)))
endif

export WALLET_DOCKERFILE WALLET_DATA_DIR

WALLET_SERVICES := $(shell seq -f 'wallet-%g' 1 $(N_WALLETS))

# Prefer Compose V2 plugin; fall back to standalone docker-compose 1.29+.
ifeq ($(shell docker compose version >/dev/null 2>&1 && echo yes),yes)
COMPOSE := docker compose
else
COMPOSE := docker-compose
endif

# Run from project root so relative volume paths resolve.
# Pass wallet build vars explicitly — compose also auto-loads `.env`, which only
# has WALLET_IMPL=cdk and would not set WALLET_DOCKERFILE on its own.
define compose
	cd $(ROOT) && \
		WALLET_DOCKERFILE="$(WALLET_DOCKERFILE)" \
		WALLET_DATA_DIR="$(WALLET_DATA_DIR)" \
		$(COMPOSE) -p proofstorm -f compose.yml $(1)
endef

# Regtest adversarial stack (SPEC.md Phase 6). Separate compose project so it
# never collides with the FakeWallet stack. Sources regtest/*.env for image
# pins + overrides; compose.regtest.yml also carries defaults for every var, so
# it runs even without those files.
MINT ?= cdk
SCENARIO ?= all
define compose_rt
	cd $(ROOT) && set -a && \
		{ [ -f regtest/versions.env ] && . ./regtest/versions.env || true; } && \
		{ [ -f regtest/env ] && . ./regtest/env || true; } && set +a && \
		$(COMPOSE) -p proofstorm-rt -f compose.regtest.yml $(1)
endef

.PHONY: help up down build fund balances smoke check watch snapshot wait-mint ps logs smoke-cdk smoke-nutshell \
	regtest-build regtest-up regtest-fund regtest-down regtest-ps regtest-logs attack

help:
	@echo "proofstorm targets:"
	@echo "  make up              start mint + wallet-1..N (N_WALLETS=$(N_WALLETS), WALLET_IMPL=$(WALLET_IMPL))"
	@echo "  make down            stop stack and remove volumes"
	@echo "  make build           build wallet image(s)"
	@echo "  make wait-mint       wait until mint /v1/info responds"
	@echo "  make fund            mint FUND_AMOUNT into each wallet"
	@echo "  make balances        print each wallet balance"
	@echo "  make smoke           fund + self-swap + balances + conservation check"
	@echo "  make check           conservation check only (stack must be up)"
	@echo "  make watch           live refreshing population dashboard (ctrl-c to exit)"
	@echo "  make snapshot        render the dashboard once and exit"
	@echo "  make smoke-cdk       smoke with WALLET_IMPL=cdk"
	@echo "  make smoke-nutshell  smoke with WALLET_IMPL=nutshell"
	@echo "  make ps              compose ps"
	@echo "  make logs            follow mint logs"
	@echo "  --- regtest adversarial harness (SPEC.md, Phase 6) ---"
	@echo "  make regtest-build   build the adversary image (first run or after Dockerfile changes)"
	@echo "  make regtest-up      start bitcoind + 2 LND + cdk-mintd + nutshell + adversary"
	@echo "  make regtest-fund    mine chain, fund LND, open channel, start block-miner"
	@echo "  make attack          run built attack scenarios (MINT=cdk|nutshell, SCENARIO=all)"
	@echo "  make regtest-down    tear down the regtest stack and wipe volumes"
	@echo "  make regtest-ps      compose ps for the regtest stack"
	@echo "  make regtest-logs    follow cdk-mintd + nutshell mint logs"

build:
	$(call compose,build $(WALLET_SERVICES))

up: build
	$(call compose,up -d mint $(WALLET_SERVICES))
	@$(ROOT)scripts/wait-mint.sh
	@printf 'WALLET_IMPL=%s\nN_WALLETS=%s\nWALLET_DATA_DIR=%s\n' \
		'$(WALLET_IMPL)' '$(N_WALLETS)' '$(WALLET_DATA_DIR)' > $(ROOT).proofstorm-active
	@echo "[proofstorm] active stack recorded: WALLET_IMPL=$(WALLET_IMPL) N_WALLETS=$(N_WALLETS)"

down:
	$(call compose,down -v --remove-orphans)
	@rm -f $(ROOT).proofstorm-active

wait-mint:
	@$(ROOT)scripts/wait-mint.sh

fund:
	@$(ROOT)scripts/fund.sh

balances:
	@$(ROOT)scripts/balances.sh

check:
	@$(ROOT)scripts/check-conservation.sh

watch:
	@$(ROOT)scripts/watch.sh

snapshot:
	@WATCH_ONCE=1 $(ROOT)scripts/watch.sh

smoke:
	@$(ROOT)scripts/run-smoke.sh

smoke-cdk:
	@$(MAKE) smoke WALLET_IMPL=cdk

# Rebuild wallet image if switching impl; pass WALLET_IMPL on the same make line.
smoke-nutshell:
	@$(MAKE) up smoke WALLET_IMPL=nutshell

ps:
	$(call compose,ps)

logs:
	$(call compose,logs -f mint)

# ---- regtest adversarial harness -------------------------------------------

# Build the cdk-cli adversary explicitly. This is intentionally separate from
# regtest-up: the first build can take a long time, while ordinary stack
# restarts should use the cached image without rebuilding it.
regtest-build:
	$(call compose_rt,build adversary)

# Bring up chain + LN + both mints + adversary. block-miner is started later by
# regtest-fund (after the default wallet exists) to avoid restart churn. Run
# `make regtest-build` first when the adversary image is not available.
regtest-up:
	$(call compose_rt,up -d bitcoind lnd-a lnd-b cdk-mintd nutshell adversary)
	@echo "[proofstorm] regtest stack up. Next: make regtest-fund"

# Mine the initial chain, fund both LND nodes, open the lnd-a<->lnd-b channel,
# then start the perpetual block-miner.
regtest-fund:
	@cd $(ROOT) && set -a && \
		{ [ -f regtest/env ] && . ./regtest/env || true; } && set +a && \
		$(ROOT)regtest/scripts/fund-topology.sh
	$(call compose_rt,up -d block-miner)
	@echo "[proofstorm] regtest funded. Run: make attack MINT=cdk|nutshell"

attack:
	@MINT=$(MINT) $(ROOT)scripts/run-attack.sh $(SCENARIO)

regtest-down:
	$(call compose_rt,down -v --remove-orphans)

regtest-ps:
	$(call compose_rt,ps)

regtest-logs:
	$(call compose_rt,logs -f cdk-mintd nutshell)
