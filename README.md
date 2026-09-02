# proofstorm

> **Architecture transition:** Proofstorm is becoming an MCP-native,
> Kubernetes-backed protocol laboratory. The Rust v1alpha1 contracts and MCP
> read path live under `crates/`; the Compose harness documented below remains
> available while the new vertical slices are built.

## Agent quick start

Prerequisites are Docker, Rust 1.88, `curl`, and `tar`. The setup command
downloads checksum-verified pinned k3d, Helm, and kubectl binaries into the
gitignored `.tools/` directory, creates the local cluster, installs the Helm
chart, builds the release MCP binary, and runs the doctor:

```bash
tools/proofstorm-cluster setup
```

Run the doctor again at any time to verify pinned tool versions, Docker and
cluster access, controller availability, and a real capability-filtered MCP
stdio handshake:

```bash
tools/proofstorm-cluster doctor
```

The checked-in configuration follows the current stable
[OpenCode local MCP format](https://opencode.ai/docs/mcp-servers/). With
OpenCode installed, start a project session without changing personal config:

```bash
OPENCODE_CONFIG=examples/opencode.json opencode .
```

Use the complete agent request in
[`examples/opencode-conversation.md`](examples/opencode-conversation.md), then
remove the local cluster when finished:

```bash
tools/proofstorm-cluster down
```

The MCP configuration is operator-owned. Its principal and capability set are
not agent inputs, and the agent never receives kubeconfig. A principal granted
`component.exec` can inspect component-local credentials from inside that lab,
so that capability must be treated as secret-bearing authority.

Run the hermetic Slice 1 suite:

```bash
cargo test --workspace --all-targets
```

Start the stdio MCP server:

```bash
cargo run -p proofstorm-mcp
```

Run a durable, capability-scoped local MCP session:

```bash
PROOFSTORM_DB=.proofstorm/proofstorm.sqlite3 \
PROOFSTORM_WORKSPACE=local-lab \
PROOFSTORM_PRINCIPAL=designer \
PROOFSTORM_CAPABILITIES=catalog.read,lab.read,lab.create,lab.edit,lab.clone,lab.validate,lab.publish \
cargo run -p proofstorm-mcp
```

The configured capability list replaces that principal's grants in the selected
workspace. It is trusted operator configuration, never model-supplied input.

To enable the Kubernetes-backed lifecycle tools, add
`lab.materialize,lab.status,lab.close` to the capability list and set
`PROOFSTORM_CONTROL_NAMESPACE=proofstorm-system`. The MCP server then uses the
operator's current Kubernetes client configuration; agents still receive only
Proofstorm's MCP interface, never Kubernetes authority.

Every component independently declares an implementation `version` and a
required adapter `config_version`. The catalog advertises both. Publication
refuses unsupported configuration versions, and the resolved lock records both
versions plus a digest of that component's configuration. This lets adapter
configuration evolve without pretending it is the same thing as upgrading the
underlying Bitcoin, Lightning, mint, wallet, or attacker service.

Slice 2 introduces the Kubernetes security spine. Its pinned tool versions are
in `tools/versions.env`; the local lifecycle is:

```bash
tools/proofstorm-cluster setup
tools/proofstorm-cluster doctor
tools/proofstorm-cluster down
```

The controller reconciles content-locked Bitcoin Core, LND, Core Lightning,
CDK, Nutshell wallet, and bounded attacker-workspace adapters into restricted
instance namespaces. The live Slice
4 acceptance path is:

```bash
tools/proofstorm-cluster setup
bash tests/kubernetes/slice4-e2e.sh
tools/proofstorm-cluster down
```

That test drives create, publish, materialize, readiness, sanitized status, and
verified close entirely through MCP.

Slice 5 adds a pinned Nutshell wallet adapter and asynchronous, capability-
scoped operation tools for liquidity bootstrap, wallet round trips,
conservation checks, and operation status. Operation submission returns after a
bounded Kubernetes Job is admitted; clients poll `proofstorm_operation_status`
for a content-hashed JSON artifact. Jobs have fixed deadlines, no service-
account token, no service-link environment injection, zero retries, a ten-
minute TTL, and a 32 KiB persisted artifact ceiling. Run the live path with:

```bash
tools/proofstorm-cluster setup
bash tests/kubernetes/slice5-e2e.sh
tools/proofstorm-cluster down
```

Proofstorm also exposes `proofstorm_component_exec` under the separate,
high-authority `component.exec` capability. It runs an unrestricted
non-interactive shell program in the selected component's exact digest-pinned
image with its canonical lab-local state mounted. `component` selects that
execution context; optional `target_component` selects which lab component's
service metadata is exposed and defaults to the execution component. This lets
one pinned Bitcoin CLI deliberately query any Bitcoin node in a multi-node lab.
The command receives generic `PROOFSTORM_TARGET_*` identity, DNS, and named-port
metadata plus implementation-native endpoint variables such as
`BITCOIN_RPC_HOST` and `BITCOIN_RPC_PORT`; Proofstorm never parses or replaces
the native command. Proofstorm still owns the
namespace, image, volumes, pod identity, network policy, deadline, and output
limit; the workload receives no Kubernetes service-account token, host mount,
or cross-lab credential. The terminal artifact records the native exit code and
up to 20 KiB of combined output. Use typed actions for portable orchestration
and native exec when protocol fidelity or implementation-specific attack
surfaces matter.

Run the live native-protocol acceptance gate with:

```bash
tools/proofstorm-cluster setup
bash tests/kubernetes/native-exec-e2e.sh
tools/proofstorm-cluster down
```

The gate uses a unique workspace and instance identity per invocation. It runs
native Bitcoin help, RPC against two independently selectable Bitcoin nodes,
LND help, Nutshell help, and an in-workload
service-account-token absence check; verifies action idempotency, locked images,
network identity, bounded artifacts, canonical evidence, and verified teardown.

Agents should use `proofstorm_lab_wait` after materialization or close and
`proofstorm_operation_wait` after action submission. These calls perform
server-side exponential backoff with a required 1–120 second bound.
`proofstorm_lab_wait` returns only phase, readiness counts, message, and teardown
receipt; `proofstorm_operation_wait` returns compact operation identity, phase,
and the terminal artifact. This avoids repeated full topology and journal
responses while preserving explicit timeouts and terminal evidence.

For the alpha, one lab is a node-local scheduling unit. This permits isolated
LND credential volumes to be used by bounded operation Jobs and scales separate
labs across Kubernetes nodes, but one large lab cannot span nodes. A future
distributed-lab increment must replace this constraint with per-component
controllers before distributed labs are claimed as supported. A component
controller is a deterministic, component-local capability gateway—not an AI
agent.

Slice 6 lifecycle work is complete. Experiments and exclusive leases are
durable SQLite records with dedicated capabilities. Leases expire, carry a
bounded action budget, and block conflicting leases, experiment close, and lab
close. Slice 5 runtime operations now require a matching lease and are assigned
an atomic experiment-wide sequence; `proofstorm_action_list` exposes bounded
pages of that canonical journal in an object envelope with an explicit next
sequence cursor.

Liquidity bootstrap, wallet round trip, and conservation oracle are
controller-owned typed actions. MCP records and submits a
`ProofstormLabAction`; `proofstormd` independently
validates it against the immutable lab lock, resolves the logical wallet to its
installed adapter and digest-pinned image, creates or observes one deterministic
Job, and publishes bounded terminal status for journal synchronization. Once an
action records that execution began, a missing Job fails closed as
`action_job_lost`; proofstormd refuses to recreate it because a state-changing
effect may have completed before its receipt disappeared. The
Slice 5 live test retries all three calls, restarts `proofstormd` while
bootstrap is active, proves that exactly one Job ran for each accepted action,
rejects malformed actions and exhausted lease budgets before Job creation,
deletes a running action Job across controller downtime to prove the no-replay
fence, and confirms that lab close removed the ephemeral action resources. MCP no
longer renders runtime Jobs or reads pod termination state directly. An
`action.cancel` capability lets an operation owner persist a cancellation intent
without mutating its accepted spec; proofstormd removes the owned Job and
publishes a monotonic `cancelled` terminal artifact. Fixed Job deadlines report
the stable `action_deadline_exceeded` error.

Slice 7 begins with an agent-facing lab composer. Five MCP tools add, update,
or remove logical components and add or remove typed links using optimistic
draft versions and idempotency keys. Mutations resolve against the installed
catalog, enforce implementation kind, control class, service version,
configuration-contract version, configuration fields, topology compatibility,
and policy limits, then store components and links in canonical order. Failed
mutations are transactional, and component removal refuses until its links are
removed explicitly.

The Kubernetes acceptance client constructs its seven-component, four-link lab
from an empty draft through those MCP mutations before publishing it. That
composed lab passes the complete materialize, bootstrap, logical Lightning
peer-connect, bounded channel-open, wallet round-trip, cancellation, oracle,
and verified-close workflow on k3d. Peer and channel requests contain only
logical component identities and bounded amounts; proofstormd resolves the
installed LND adapter, pinned images, credentials, and controller-owned Jobs.

Logical Bitcoin and Lightning nodes can now be stopped, started, and restarted
through `proofstorm_node_stop`, `proofstorm_node_start`, and
`proofstorm_node_restart` under the `node.control` capability. These are direct,
ordered controller reconciliations of the component StatefulSet rather than
privileged Jobs: MCP receives no Kubernetes or node credentials. Desired state,
action sequence, and restart identity are durable workload annotations preserved
by ordinary lab reconciliation and controller restarts. Older actions cannot
overwrite newer lifecycle intent. An intentionally stopped component reports
`ready: false` while the lab remains operable and `ready`, allowing a later
start. Cancellation after execution begins fails closed as inconclusive instead
of claiming that an already-applied node transition was undone.

The live k3d path stopped the payer LND StatefulSet at zero replicas, preserved
lab readiness, started it back to one ready replica, and restarted it with a new
Pod UID. The three sanitized lifecycle artifacts were canonical action
sequences 15–17.

Topology teardown is now available through `proofstorm_peer_disconnect`,
`proofstorm_channel_close`, and `proofstorm_channel_force_close`. Channel-open
artifacts return an opaque `ch-` handle derived inside the credential-bearing
controller Job, so agents can select one of several channels between the same
logical nodes without receiving an LND funding outpoint. Cooperative close
reports a confirmed, fully closed channel. Force close reports the confirmed
close transaction separately from its still-pending CSV resolution rather than
claiming funds are settled. Peer disconnect is issued and verified from both
logical endpoints so the result is not defeated by the remote node immediately
reconnecting.

`proofstorm_channel_rebalance` completes the topology mutation surface for the
initial proof of concept. An agent selects one logical LND component, two opaque
`ch-` handles, an amount, and a maximum fee. The controller resolves native
channel IDs and peers inside a credential-bearing Job, creates and pays a
private self-invoice through exactly one outgoing and one incoming channel, and
returns only the handles, fee, and verified balance deltas. Payment material and
native identifiers never cross the controller boundary. The Job tolerates
bounded LND gossip convergence under a hard deadline and never replays a
settled payment. Channel-handle resolution tolerates up to 30 seconds of active
channel convergence inside the fixed 120-second Job deadline. Core Lightning
rebalance is explicitly unsupported until its adapter implements the same
contract.

The live path forms a real three-node cycle and rebalances 100,000 sat through
it, closes the temporary bridge, then cooperatively and forcibly closes the LND
and mixed CLN/LND channels while exercising peer disconnect/reconnect. These
actions remain canonically ordered alongside the network observations described
below, and the first request beyond the lease budget is refused before any
runtime action is created.

Slice 8 begins with `proofstorm_network_partition` and
`proofstorm_network_heal`. An agent partitions two logical components and later
heals that exact fault using its durable operation ID; it never receives
Kubernetes or CNI credentials. Proofstormd applies the fault directly through
component-scoped NetworkPolicies and reconstructs the active fault set from the
ordered action journal, so overlapping partitions compose and healing one does
not erase another. Controller action Jobs retain an independent intra-lab
policy, keeping fault administration out of component containers.

The live path proves normal TCP reachability from two persistent Nutshell
wallets to their mint, then blocks both sockets through overlapping partitions.
The `proofstorm_reachability_oracle` accepts only logical source and destination
component IDs plus a destination-advertised service name. Proofstorm resolves
the port from the immutable adapter contract and runs a bounded, digest-pinned
probe under the source component's actual NetworkPolicy identity; the agent
cannot supply a host, port, image, command, or credential. Both reachable and
unreachable observations complete successfully with structured artifacts.
While proofstormd is stopped, acceptance removes the
default-deny and affected component policies and observed connectivity return.
The replacement controller reconstructed the baseline and both faults from the
immutable lab plus action journal. A targeted heal restores only the first
wallet; the second remains blocked until its own heal. The
remaining Lightning workflow completed, demonstrating fault composition,
restart recovery, and selective healing rather than a blanket lab outage.

Agents discover the installed fault implementation with
`proofstorm_network_capabilities`. Its descriptor includes the backend ID and
version, supported features and directions, and numeric bounds. The current
`kubernetes-network-policy` backend advertises only bidirectional partition and
heal; it does not pretend to support traffic shaping. The typed
`proofstorm_network_delay` contract accepts explicit `from_to` or
`bidirectional` direction, 1–60,000 ms delay, and at most 10,000 ms jitter that
cannot exceed the delay. `proofstorm_network_loss` accepts 1–10,000 basis points
of packet loss. With the current backend, both return
`network_fault_unsupported` before operation admission, lease-budget
consumption, or journal sequencing. The MCP surface now exposes 53 tools. The
live workflow records nine reachability observations spanning baseline,
overlapping faults, controller reconstruction, and targeted heals, for a
47-action canonical journal; the forty-eighth request is refused by the lease
budget.

`proofstorm_artifact_export` turns a closed experiment into a deterministic,
content-hashed evidence bundle without consulting Kubernetes. It includes the
complete immutable lab revision and resolved lock, a canonical projection of up
to 100 terminal actions, all oracle artifact bodies by default, and up to 16
explicitly selected sanitized artifacts. The bundle is capped at 512 KiB and
omits runtime resource names, instance keys, component credentials, private
payment material, and unbounded logs. Export requires both `experiment.read`
and `artifact.read`; it does not consume a lease action.

The live k3d workflow exported the complete 47-action experiment after lease
release and experiment close. The bundle contained the seven-component lab and
resolved content lock, the ordered terminal journal, all twelve conservation
and reachability artifacts, and the explicitly selected wallet-payment artifact.
Acceptance verified the 512 KiB ceiling and scanned the result for runtime
resource names, instance keys, BOLT11 invoices, adapter quote IDs, payment
requests, and mnemonics before completing verified lab teardown.

The CLN adapter exposes only its public P2P service. Its Unix-domain RPC socket
stays on the component PVC and is mounted only into bounded controller Jobs.
Mixed operations resolve each endpoint's independently pinned adapter image;
they do not assume both Lightning nodes ship the same CLI. CLN close Jobs run
the close negotiation, bounded regtest mining, and terminal-state verification
as coordinated containers so chain progress cannot deadlock behind a blocking
adapter call.

CDK 0.17.6 mints may select either an exact LND or CLN BOLT11/sat backend. The
CLN path mounts the selected node's compiled state claim read-only, configures
its regtest Unix socket, and explicitly disables BOLT12 until that capability
has its own LDK-backed contract. Exercise the complete MCP materialization and
live binary/configuration check with:

```sh
bash tests/kubernetes/cdk-cln-e2e.sh
```

CDK 0.17.6 also has a distinct embedded-LDK runtime. It links the mint directly
to a selected Bitcoin Core node, persists the LDK node in the mint's own state,
and exposes its P2P port without exposing the loopback administrative UI. A
usable BOLT12 offer requires an introduction path, so the live acceptance adds
a real CLN peer, connects it to embedded LDK, requests an actual 100-sat `lno`
offer through the mint API, and verifies teardown:

```sh
bash tests/kubernetes/cdk-ldk-e2e.sh
```

The distinct CDK-BDK runtime uses CDK 0.17.6's standard image, where BDK is a
default feature, and links an on-chain-only mint directly to a selected Bitcoin
Core node. Its bounded stress acceptance creates 24 concurrent NUT-30 address
quotes, exercises agent-authored input fees, keyset-v2 policy, quote lifetimes,
mint/melt bounds, NUT-06 metadata, in-memory cache policy, and transaction input
and output limits in the live native configuration and API, funds and confirms
selected quotes, checks the authored minimum-deposit boundary, restarts the mint
to prove persistence, and verifies teardown:

```sh
bash tests/kubernetes/cdk-bdk-stress-e2e.sh
```

Nutshell mint parity is the current control-plane increment. Nutshell 0.20.2 is
now an exact-version mint catalog entry with a typed configuration contract,
machine-readable field coverage, a pinned image digest, persistent state, a
controller-generated private key, and exact BOLT11/sat bindings to LND and
Core Lightning REST. The CLN binding creates a persistent mode-0600 rune that
permits only the six RPC methods used by Nutshell; it is neither placed in
public configuration nor returned through MCP. The adapter supports SQLite,
the existing secret-backed PostgreSQL primary-storage contract, and an
independent password-authenticated Redis cache contract. Redis 8.10.1 is
digest-pinned, topology-selected through the typed `cache` database role,
bounded by an authorable memory limit with `allkeys-lru` eviction, and
intentionally ephemeral; its URL stays in a controller-generated Secret.
PostgreSQL credentials and the mint private key remain stable across controller restarts,
while database state survives both database and mint workload restarts. NUT-06
metadata, quote lifetimes, proof/request limits, mint/melt and balance ceilings,
fee reserve policy, rate limits, Redis cache TTL, MPP, and watchdog policy are
agent-authorable and rollout-affecting. OIDC authentication is complete: the
exact Nutshell 0.20.2 NUT-21 clear-auth and NUT-22 blind-auth settings,
PVC-backed authentication ledger, and discovery/client policy are typed and
rendered. A typed `authentication_backend` link selects digest-pinned Keycloak
25.0.6 with mandatory PostgreSQL storage, a topology-derived discovery URL,
bounded JVM heap, and controller-generated administrator, realm-import, and
disposable test-user credentials. The generated public client includes the
standard subject-bearing scopes and optional offline access. An exact-version
auth migration bridges Nutshell 0.20.2's older auth schema to its newer CRUD
write path. Non-LND/non-CLN payment backends and management RPC are not
advertised until matching dependency and secret contracts exist. The current
live acceptance gates are:

```sh
bash tests/kubernetes/nutshell-mint-e2e.sh
bash tests/kubernetes/nutshell-cln-e2e.sh
bash tests/kubernetes/nutshell-postgres-e2e.sh
bash tests/kubernetes/cross-implementation-wallet-e2e.sh
bash tests/kubernetes/nutshell-oidc-e2e.sh
```

The OIDC gate obtains a real Keycloak access token with Nutshell's native
`WalletAuth`, checks signed issuer/client/subject claims, exercises the NUT-21
and NUT-22 missing/invalid, maximum-output, and per-user rate-limit failures,
mints DLEQ-backed BATs, uses a BAT on a protected quote, and proves credential,
auth-ledger, spent-token, and fresh-token behavior across controller,
PostgreSQL, Keycloak, and mint restarts before verified teardown.

CDK 0.17.6 is not claimed as an authenticated Nutshell client by this gate.
Its OIDC login and refresh complete against the generated Keycloak client, but
its NUT-22 key parser rejects Nutshell 0.20.2's nullable `input_fee_ppk`; the
0.17.6 CLI also loses the fetched auth settings when it constructs the wallet.
That cross-client compatibility gap is the next bounded increment.

The cross-implementation gate materializes CDK 0.17.6 and Redis-backed
Nutshell 0.20.2 in one lab, drives both through the same pinned Nutshell wallet
adapter, verifies application-populated cache keys, stable Redis credentials,
ephemeral cache restart and mint recovery, and
requires identical initialize, zero-balance, 1,000 sat funding, self-pay round
trip, and exact conservation-oracle behavior before verified teardown.

The implementation-neutral wallet surface now includes
`proofstorm_wallet_initialize`, `proofstorm_wallet_balance`, and
`proofstorm_wallet_fund`. Balance reads copy the persistent wallet into a
disposable snapshot before invoking the locked adapter, while initialize and
fund are bounded state-changing actions under separate capabilities. MCP never
receives a mnemonic, proof database, mint quote, Lightning invoice, or adapter
command.

Proofstorm now owns an adapter-neutral durable wallet quote contract. A quote
is a scoped handle containing wallet and mint IDs, receive/pay direction,
bounded satoshi amount, expiry, and a monotonic
`requested → ready → pending → paid → settled` lifecycle with explicit failed,
expired, inconclusive, and cancelled outcomes. `Inconclusive` quarantines an
ambiguous payment without authorizing replay; a later durable payer receipt or
authoritative recipient settlement may repair it to paid or settled. Adapter quote IDs and payment
requests have no public field and remain inside the controller adapter. Quotes persist
across MCP restarts, are bound to the experiment lease and principal, and are
inspectable through capability-filtered `proofstorm_wallet_quote_status` and
`proofstorm_wallet_quote_list` tools. Quote-list pages use an object envelope
with an explicit next-quote cursor so MCP clients never receive top-level array
structured content.

`proofstorm_wallet_invoice` and `proofstorm_wallet_pay` now execute that
contract for the pinned Nutshell adapter. A controller-owned recipient Job
writes its BOLT11 request only into a mode-restricted directory on the private
recipient wallet volume and waits for settlement. A distinct payer-wallet Job
receives that volume read-only, pays the private request, and reports only the
quote ID, logical wallet IDs, bounded amount, phase, and sanitized balance. The
quote progresses from ready through pending/paid to settled as the two durable
actions complete. Lost or ambiguous payment receipts use the quarantined
`inconclusive` phase instead of claiming failure or replaying payment. Quote
status reconciles the linked durable invoice action after MCP restart. Invoice
cancellation is not published until a bounded controller-owned cleanup Job has
proven the private payment request absent from the wallet volume; cleanup
failure is explicit and fails closed.

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
make regtest-build   # first run (or after docker/adversary changes); builds cdk-cli
make regtest-up      # bitcoind + lnd-a + lnd-b + cdk-mintd + nutshell + adversary
make regtest-fund    # mine chain, fund LND, open channel, start block-miner
make attack                    # all built scenarios vs the CDK mint
make attack MINT=nutshell      # vs the Nutshell mint
make regtest-down              # tear down + wipe volumes
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
| `CDK_MINTD_VERSION`           | `0.17.6` | `cashubtc/mintd` tag                                         |
| `CDK_CLI_VERSION`             | `0.17.6` | `cdk-cli` in wallet image                                    |
| `NUTSHELL_VERSION`            | `0.20.2` | `cashubtc/nutshell` in wallet image                          |
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
Redis and OIDC support are complete. The exact 0.20.2 authentication projection,
PVC-backed auth ledger compatibility migration, typed in-lab Keycloak
dependency, and live NUT-21/NUT-22 acceptance now pass together. Authenticated
CDK-to-Nutshell client interoperability is next; additional payment backends
and management RPC remain intentionally unsupported until their typed
dependency, authority, and secret contracts are implemented.

| Kubernetes Nutshell increment | Status | Exit gate |
| ----------------------------- | ------ | --------- |
| LND/CLN, SQLite/PostgreSQL, Redis | done | Exact-version golden, restart, and cross-implementation gates pass |
| OIDC settings and auth-ledger wiring | done | Typed schema, exact 0.20.2 environment projection, PVC-backed auth persistence |
| In-lab Keycloak dependency | done | Digest-pinned provider, generated credentials, topology-derived discovery URL |
| NUT-21/NUT-22 live acceptance | done | Positive/negative limits, DLEQ BAT, protected quote, replay persistence, restart recovery, verified teardown |
| CDK authenticated-client interoperability | next | CDK parses Nutshell auth keys, retains auth settings, mints and spends a BAT against the protected API |
| Additional payment backends | queued | Exact backend and secret contracts selected and verified |
| Management RPC | queued | Separate mTLS authority and bounded management capabilities |

## Notes

- Mint uses **FakeWallet**: wallet mint commands auto-settle; no LN node.
- Each wallet has its own volume and keys (`/root/.cdk` or `/root/.cashu`).
- Host scripts are the control plane; containers stay alive with `sleep infinity`.
- Conservation: after fund, `sum == N * FUND_AMOUNT`; after self-swap, no inflation and swap cost within tolerance (nutshell may burn ~1 sat/wallet on receive).
