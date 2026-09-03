# Compose harness (legacy)

> This is the original Docker Compose wallet-population runner and the regtest
> adversarial harness. It predates the MCP and Kubernetes system documented in
> the main [README](../README.md) and is kept until the regtest attack
> scenarios have Kubernetes equivalents.
>
> Every target below runs through `make compose-<target>`, for example
> `make compose-up`, `make compose-smoke`, `make compose-down`. The recipes
> themselves live in `Makefile.compose`.

Wallet-population runner for Cashu.

Spins up **one mint** and **N independent CLI wallets** in Docker, funds them
(FakeWallet), and drives scripted CLI operations from the host. V3 adds a
value-conservation check after each smoke run.

Not a Bitcoin network simulator. Not an operator e2e suite (see Orchard).

## Requirements

- Docker Engine (running daemon), with either the Compose V2 plugin
  (`docker compose`) or standalone `docker-compose` v1.29+
- `make`, `bash`, `curl`
- Network access on first run — images/binaries are pulled from public
  registries (Docker Hub `cashubtc/mintd`, `cashubtc/nutshell`; crates.io for
  `cdk-cli`). No local sibling repos are needed; the project is self-contained.

> **First run is slow for cdk.** `make up` with the default cdk wallet
> **compiles `cdk-cli` from source (~15 min)**. It is cached afterwards.
> The nutshell wallet (`WALLET_IMPL=nutshell`) only pulls an image and is fast.

## Quick start (cdk-cli)

```bash
git clone <this-repo> && cd proofstorm
cp .env.example .env   # optional; defaults work without it
make compose-up                # mint + wallet-1..N (default N=3); first cdk build ~15 min
make compose-smoke             # fund + self-swap + balances + conservation check
make compose-balances
make compose-check             # conservation only (stack must be up)
make compose-down              # tear down + wipe volumes
```

Wallet state lives in Docker named volumes, not in the cloned folder, so
`make down` wipes it cleanly. `.env` and `.proofstorm-active` are gitignored.

## Visualizing the population

proofstorm is CLI + containers, but `make watch` gives a live terminal
dashboard of every wallet balance, the population total, and conservation
status (bars scale to `FUND_AMOUNT`):

```bash
make compose-watch       # live, refreshes every WATCH_INTERVAL secs (ctrl-c to exit)
make compose-snapshot    # render once and exit
```

Run `make watch` in one terminal and `make smoke` in another to see balances
move in real time. Heavier options:

- **Orchard** (in this repo) — point its `MINT_URL` at `http://localhost:3338`
  for a web dashboard of the _mint_ side (keysets, balance sheet, analytics).
- **Grafana/Prometheus** — `cdk-mintd` exposes Prometheus metrics; scrape for
  mint-side charts.

## Nutshell wallet (V2)

Use the nutshell `cashu` CLI instead of `cdk-cli`. Either set it in `.env`:

```bash
WALLET_IMPL=nutshell
```

Or pass it on the command line for `make up` only:

```bash
make compose-down                      # if a stack is already running
WALLET_IMPL=nutshell make up   # builds + records the active impl
make compose-smoke                     # auto-uses nutshell — no prefix needed
make compose-watch                     # same
```

`make up` records the resolved `WALLET_IMPL`/`N_WALLETS` in
`.proofstorm-active`. All driving commands (`fund`, `balances`, `smoke`,
`check`, `watch`) read that file, so you **only** pass `WALLET_IMPL` to `make up`
— never to the driving commands. If you pass a mismatched impl it warns and uses
the running stack. To switch impl: `make down` then `make up` with the new one.

Each wallet container stores state under `/root/.cashu` (nutshell) or
`/root/.cdk` (cdk) with wallet names `wallet-1` .. `wallet-N`.

## Adversarial regtest harness (Phase 6)

The FakeWallet stack above is the wallet-population runner. The **adversarial
harness** is a separate regtest stack where an attacker tries to make a mint
mis-issue value ("steal funds") or stop serving honest clients ("DoS"). Full
threat model, topology, and the attack/oracle catalog are in
[`SPEC.md`](SPEC.md); the runnable scenarios live in
[`scenarios/`](scenarios/README.md).

Topology: one `bitcoind` (regtest), two LND nodes with a channel between them,
`cdk-mintd` on one node and Nutshell on the other, plus an `adversary`
container. Both mints share one chain and their LN nodes are channel peers, so
you get both **cross-implementation** attacks (a melt at the CDK mint pays an
invoice on Nutshell's node; a cdk-cli wallet attacks the Nutshell mint) and
**parallel comparison** (run the same attack against `MINT=cdk` then
`MINT=nutshell`).

```bash
make compose-regtest-build   # first run (or after docker/adversary changes); builds cdk-cli
make compose-regtest-up      # bitcoind + lnd-a + lnd-b + cdk-mintd + nutshell + adversary
make compose-regtest-fund    # mine chain, fund LND, open channel, start block-miner
make compose-attack                    # all built scenarios vs the CDK mint
make compose-attack MINT=nutshell      # vs the Nutshell mint
make compose-regtest-down              # tear down + wipe volumes
```

> **First run is slow.** `make regtest-build` compiles `cdk-cli` into the
> `adversary` image (~15 min, cached after). `make regtest-up` deliberately
> does not rebuild it, so ordinary restarts are fast. LND/bitcoind/mint images
> are pulled from public registries.

An attack exits `0` when the mint upholds its oracle (rejects the attack and
stays live) and non-zero when an oracle is violated. This is not covered by
CDK's or Nutshell's own suites, which test double-spend/concurrency **in
process** against an in-memory ledger — proofstorm attacks the **deployed mint
over HTTP** with independent, racing clients and a real LN backend (SPEC §1).

## Configuration

| Variable                      | Default  | Meaning                                                      |
| ----------------------------- | -------- | ------------------------------------------------------------ |
| `N_WALLETS`                   | `3`      | Population size (1–10)                                       |
| `FUND_AMOUNT`                 | `100`    | Sats minted per wallet                                       |
| `SWAP_AMOUNT`                 | `1`      | Sats used in self-swap during smoke                          |
| `WALLET_IMPL`                 | `cdk`    | `cdk` or `nutshell`                                          |
| `MINT_IMPL`                   | `cdk`    | Mint implementation (cdk only today)                         |
| `CDK_MINTD_VERSION`           | `0.18.0` | `cashubtc/mintd` tag                                         |
| `CDK_CLI_VERSION`             | `0.18.0` | `cdk-cli` in wallet image                                    |
| `NUTSHELL_VERSION`            | `0.20.3` | `cashubtc/nutshell` in wallet image                          |
| `MINT_HOST_PORT`              | `3338`   | Host port for mint HTTP                                      |
| `CONSERVATION_EXPECTED`       | _(auto)_ | Override expected total (`N * FUND_AMOUNT`)                  |
| `CONSERVATION_TOLERANCE`      | `0`      | Allowed delta in fund/population check                       |
| `CONSERVATION_SWAP_TOLERANCE` | _(auto)_ | Max sat loss after self-swap (`0` cdk, `N_WALLETS` nutshell) |

## Layout

```
compose.yml              FakeWallet mint + wallet-1..wallet-10
compose.regtest.yml      Phase 6: bitcoind + 2 LND + cdk-mintd + nutshell + adversary
SPEC.md                  adversarial threat model + attack/oracle catalog
docker/mint/             mintd.toml (FakeWallet) + mintd.regtest.toml (LND backend)
docker/wallet/           cdk-cli and nutshell wallet images
docker/adversary/        adversary image (cdk-cli + curl/jq)
regtest/                 versions.env, env, block-miner + fund-topology scripts
scripts/                 host drivers (docker exec into wallets)
scripts/lib/wallet.sh    wallet CLI abstraction (cdk + nutshell)
scripts/check-conservation.sh   V3 value-conservation assertion
scripts/run-attack.sh    Phase 6 attack runner
scenarios/               adversarial scenarios + lib/attack.sh helpers
```

## Legacy Compose roadmap

| Phase  | Status | Deliverable                                  |
| ------ | ------ | -------------------------------------------- |
| 0      | done   | mint only, `make up` / `make down`           |
| 1 (V0) | done   | one wallet, fund + balance                   |
| 2 (V1) | done   | N wallets, smoke path                        |
| 3 (V2) | done   | nutshell wallet CLI as `WALLET_IMPL`         |
| 4 (V3) | done   | value-conservation check after smoke         |
| 5      | next   | wallet-to-wallet token handoff               |
| 6      | in progress | adversarial regtest harness — see [`SPEC.md`](SPEC.md) |

The Kubernetes control-plane roadmap supersedes that historical table. CDK and
Nutshell exact-version configuration coverage is complete. Nutshell's catalog,
typed configuration, LND/SQLite and secret-backed PostgreSQL materialization,
generated key handling, golden contract, restart persistence, and live
acceptance clients are in tree and passing on k3d. The same pinned Nutshell
wallet workflow also passes against CDK and Redis-backed Nutshell side by side.
Redis support and OIDC environment wiring are complete. The exact 0.20.3
authentication projection and typed in-lab Keycloak dependency pass, while the
native NUT-22 gate is blocked by the reproduced upstream auth-ledger schema
defect. CDK-to-Nutshell authenticated-client conformance follows that fix;
additional payment backends
and management RPC remain intentionally unsupported until their typed
dependency, authority, and secret contracts are implemented.

| Kubernetes Nutshell increment | Status | Exit gate |
| ----------------------------- | ------ | --------- |
| LND/CLN, SQLite/PostgreSQL, Redis | done | Exact-version golden, restart, and cross-implementation gates pass |
| OIDC settings and auth-ledger wiring | done | Typed schema, exact 0.20.3 environment projection, PVC-backed auth persistence |
| In-lab Keycloak dependency | done | Digest-pinned provider, generated credentials, topology-derived discovery URL |
| NUT-21/NUT-22 live acceptance | blocked upstream | Nutshell 0.20.3 must mint and persist a BAT without a schema rewrite, then pass replay and restart recovery |
| CDK authenticated-client interoperability | blocked upstream | Current CDK parses Nutshell auth keys, retains auth settings, and spends a BAT against the protected API |
| Additional payment backends | queued | Exact backend and secret contracts selected and verified |
| Management RPC | queued | Separate mTLS authority and bounded management capabilities |

## Notes

- Mint uses **FakeWallet**: wallet mint commands auto-settle; no LN node.
- Each wallet has its own volume and keys (`/root/.cdk` or `/root/.cashu`).
- Host scripts are the control plane; containers stay alive with `sleep infinity`.
- Conservation: after fund, `sum == N * FUND_AMOUNT`; after self-swap, no inflation and swap cost within tolerance (nutshell may burn ~1 sat/wallet on receive).
