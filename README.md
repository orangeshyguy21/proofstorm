# proofstorm

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
make up                # mint + wallet-1..N (default N=3); first cdk build ~15 min
make smoke             # fund + self-swap + balances + conservation check
make balances
make check             # conservation only (stack must be up)
make down              # tear down + wipe volumes
```

Wallet state lives in Docker named volumes, not in the cloned folder, so
`make down` wipes it cleanly. `.env` and `.proofstorm-active` are gitignored.

## Visualizing the population

proofstorm is CLI + containers, but `make watch` gives a live terminal
dashboard of every wallet balance, the population total, and conservation
status (bars scale to `FUND_AMOUNT`):

```bash
make watch       # live, refreshes every WATCH_INTERVAL secs (ctrl-c to exit)
make snapshot    # render once and exit
```

Run `make watch` in one terminal and `make smoke` in another to see balances
move in real time. Heavier options:

- **Orchard** (in this repo) — point its `MINT_URL` at `http://localhost:3338`
  for a web dashboard of the *mint* side (keysets, balance sheet, analytics).
- **Grafana/Prometheus** — `cdk-mintd` exposes Prometheus metrics; scrape for
  mint-side charts.

## Nutshell wallet (V2)

Use the nutshell `cashu` CLI instead of `cdk-cli`. Either set it in `.env`:

```bash
WALLET_IMPL=nutshell
```

Or pass it on the command line for `make up` only:

```bash
make down                      # if a stack is already running
WALLET_IMPL=nutshell make up   # builds + records the active impl
make smoke                     # auto-uses nutshell — no prefix needed
make watch                     # same
```

`make up` records the resolved `WALLET_IMPL`/`N_WALLETS` in
`.proofstorm-active`. All driving commands (`fund`, `balances`, `smoke`,
`check`, `watch`) read that file, so you **only** pass `WALLET_IMPL` to `make up`
— never to the driving commands. If you pass a mismatched impl it warns and uses
the running stack. To switch impl: `make down` then `make up` with the new one.

Each wallet container stores state under `/root/.cashu` (nutshell) or
`/root/.cdk` (cdk) with wallet names `wallet-1` .. `wallet-N`.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `N_WALLETS` | `3` | Population size (1–10) |
| `FUND_AMOUNT` | `100` | Sats minted per wallet |
| `SWAP_AMOUNT` | `1` | Sats used in self-swap during smoke |
| `WALLET_IMPL` | `cdk` | `cdk` or `nutshell` |
| `MINT_IMPL` | `cdk` | Mint implementation (cdk only today) |
| `CDK_MINTD_VERSION` | `0.17.1` | `cashubtc/mintd` tag |
| `CDK_CLI_VERSION` | `0.17.1` | `cdk-cli` in wallet image |
| `NUTSHELL_VERSION` | `0.20.2` | `cashubtc/nutshell` in wallet image |
| `MINT_HOST_PORT` | `3338` | Host port for mint HTTP |
| `CONSERVATION_EXPECTED` | *(auto)* | Override expected total (`N * FUND_AMOUNT`) |
| `CONSERVATION_TOLERANCE` | `0` | Allowed delta in fund/population check |
| `CONSERVATION_SWAP_TOLERANCE` | *(auto)* | Max sat loss after self-swap (`0` cdk, `N_WALLETS` nutshell) |

## Layout

```
compose.yml              mint + wallet-1..wallet-10
docker/mint/             mintd.toml (FakeWallet, sat)
docker/wallet/           cdk-cli and nutshell wallet images
scripts/                 host drivers (docker exec into wallets)
scripts/lib/wallet.sh    wallet CLI abstraction (cdk + nutshell)
scripts/check-conservation.sh   V3 value-conservation assertion
```

## Roadmap

| Phase | Status | Deliverable |
|---|---|---|
| 0 | done | mint only, `make up` / `make down` |
| 1 (V0) | done | one wallet, fund + balance |
| 2 (V1) | done | N wallets, smoke path |
| 3 (V2) | done | nutshell wallet CLI as `WALLET_IMPL` |
| 4 (V3) | done | value-conservation check after smoke |
| 5 | next | wallet-to-wallet token handoff |
| 6 | later | fault injection, keyset rotation, multi-mint |

## Notes

- Mint uses **FakeWallet**: wallet mint commands auto-settle; no LN node.
- Each wallet has its own volume and keys (`/root/.cdk` or `/root/.cashu`).
- Host scripts are the control plane; containers stay alive with `sleep infinity`.
- Conservation: after fund, `sum == N * FUND_AMOUNT`; after self-swap, no inflation and swap cost within tolerance (nutshell may burn ~1 sat/wallet on receive).
