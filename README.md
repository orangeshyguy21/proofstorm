# proofstorm

Proofstorm is an MCP-native, Kubernetes-backed protocol laboratory for
Bitcoin, Lightning, Cashu mints, wallets, adversarial clients, and network
faults. Everything host-side is Rust, and `make` is the only entrypoint you
need. The legacy Docker Compose harness is quarantined in
[`docs/compose-harness.md`](docs/compose-harness.md).

## Agent quick start

Prerequisites are Docker, Rust 1.88, `make`, `curl`, and `tar`. Nothing else:
there is no Python or shell script to install. The setup target
downloads checksum-verified pinned k3d, Helm, and kubectl binaries into the
gitignored `.tools/` directory, creates the local cluster, installs the Helm
chart, builds the release MCP binary, and runs the doctor:

```bash
make setup
```

Run the doctor again at any time to verify pinned tool versions, Docker and
cluster access, controller availability, and a real capability-filtered MCP
stdio handshake:

```bash
make doctor
```

The checked-in configuration follows the current stable
[OpenCode local MCP format](https://opencode.ai/docs/mcp-servers/). With
OpenCode installed, start a project session without changing personal config:

```bash
OPENCODE_CONFIG=examples/opencode/proofstorm-only.json opencode .
```

That profile denies every host tool so all control flows through Proofstorm.
Two wider profiles, `research.json` and `contributor.json`, live alongside it;
[`examples/opencode/README.md`](examples/opencode/README.md) explains when to
use each.

Use the complete agent request in
[`examples/opencode-conversation.md`](examples/opencode-conversation.md), then
remove the local cluster when finished:

```bash
make down
```

The MCP configuration is operator-owned. Its principal and capability set are
not agent inputs, and the agent never receives kubeconfig. A principal granted
`component.exec_live` or `component.forensics` can inspect component-local
credentials, so both capabilities must be treated as secret-bearing authority.
Live execution additionally shares the running component's process, network,
user, localhost, and Unix-socket context.
`authentication.test` is narrower: it permits a fixed controller-rendered
authentication conformance action to consume disposable test-user credentials
inside the lab, while MCP returns only its typed, secret-free result.

Run the hermetic Slice 1 suite:

```bash
make test
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
Set `PROOFSTORM_TOOLSET=native` for a slim cross-phase experiment surface:
native CLIs handle wallet operations and routing policy, while Proofstorm keeps
provisioning, coordination, lifecycle, faults, and observations such as wallet
balance and reachability. The `experiment` profile retains typed contracts for
regression testing and comparison. Set `PROOFSTORM_TOOLSET=design`, `runtime`, or
`evidence` to expose only the
agent-facing tools for that phase; the default is `all`. A toolset only removes
routes and is always intersected with the principal's durable capabilities, so
it cannot grant authority. Focused toolsets reduce the MCP discovery schema
loaded into an agent's context.

To enable the Kubernetes-backed lifecycle tools, add
`lab.materialize,lab.status,lab.close` to the capability list and set
`PROOFSTORM_CONTROL_NAMESPACE=proofstorm-system`. The MCP server then uses the
operator's current Kubernetes client configuration; agents still receive only
Proofstorm's MCP interface, never Kubernetes authority.

Candidate builds are also agent-operated. Grant
`candidate.build,candidate.read,candidate.cancel`, configure the Kubernetes
runtime, and give the agent a public GitHub pull-request URL plus an installed
implementation ID such as `nutshell`. `proofstorm_candidate_build` freezes the
PR head commit and creates a controller-owned BuildKit Job; the job continues
if the MCP client disconnects. The agent can make repeated bounded
`proofstorm_candidate_wait` calls (at most 120 seconds each), recover prior
builds with `proofstorm_candidate_list`, and inspect bounded logs through the
receipt's resource URI. A successful build becomes a workspace-scoped,
experimental exact catalog version, so the normal `catalog_list` → `lab_plan`
→ `lab_apply` workflow remains unchanged. Published locks retain the PR URL,
repository, commit SHA, candidate ID, and immutable image digest.
Set `GITHUB_TOKEN` on the MCP server when higher GitHub API rate limits are
needed; it is server configuration and is never accepted as tool input or
passed into the build Job.

Every component independently declares an implementation `version` and a
required adapter `config_version`. The catalog advertises both. Publication
refuses unsupported configuration versions, and the resolved lock records both
versions plus a digest of that component's configuration. This lets adapter
configuration evolve without pretending it is the same thing as upgrading the
underlying Bitcoin, Lightning, mint, wallet, or attacker service.
`proofstorm_lab_publish` returns only revision and lock digests plus a component
count by default; `include_revision: true` explicitly embeds the complete lab
and lock when a caller needs the bulk document.

Catalog discovery is intentionally progressive. `proofstorm_catalog_list`
returns only compact exact-version identities and accepts implementation, kind,
feature, lifecycle, release-channel, and dependency filters with a digest-bound
cursor. After selecting a version, use `proofstorm_catalog_entry_read` for its
immutable image, compatibility, support matrix, and features, then
`proofstorm_catalog_config_schema_read` for the complete configuration JSON
Schema or one RFC 6901 fragment. Broad list calls never embed configuration
schemas.

Runtime observation follows the same pattern. `proofstorm_lab_status` is a
compact receipt containing phase, revision and lock digests, readiness and
inventory counts, and an inventory digest; it does not embed topology or
Kubernetes inventory arrays. Use `proofstorm_lab_component_status_list` for
cursor-paged component conditions and `proofstorm_lab_inventory_list` for
cursor-paged sanitized object inventory. Both pages are capped at 50 items and
shrink to a 32 KiB agent-response budget.

Slice 2 introduces the Kubernetes security spine. Its pinned tool versions are
in `tools/versions.env`; the local lifecycle is:

```bash
make setup
make doctor
make down
```

The controller reconciles content-locked Bitcoin Core, LND, Core Lightning,
CDK, Nutshell wallet, and bounded attacker-workspace adapters into restricted
instance namespaces. The live Slice
4 acceptance path is:

```bash
make setup
make e2e-slice4
make down
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
make setup
make e2e-slice5
make down
```

Proofstorm exposes two deliberately different native shell primitives.
`proofstorm_component_exec_live` (`component.exec_live`) runs inside the
selected running container, so native CLIs see the component's real localhost,
Unix sockets, files, credentials, user, and network identity. It is bounded,
fully journaled, and fail-closed against replay after controller interruption.
`proofstorm_component_forensics` (`component.forensics`) instead creates a
short-lived pod from the locked image and data mounts. It is useful for offline
source/database inspection, but explicitly does not promise live CLI or socket
connectivity. Both record bounded output and an exit code. Native CLIs are the
normal surface for operating deployed software; use typed actions where they
provide coordination, lifecycle guarantees, or useful portable observations.
The [native-first validation plan](docs/native-first-experiments.md) describes
how execution choices and evidence are evaluated. `proofstorm_component_restart`
(`component.control`) rolls any primary component workload, including mints and
wallets, while preserving its persistent state.

Run the live native-protocol acceptance gate with:

```bash
make setup
make e2e-native-exec
make down
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
pages of compact canonical summaries in an object envelope with an explicit
next sequence cursor. Summaries contain request and artifact digests, but omit
stored request bodies, runtime resource names, and artifact content; use
`proofstorm_operation_status` for one exact operation.

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
and policy limits, then store components and links in canonical order. Mutation
tools return compact version, count, validation, and changed-path receipts;
`proofstorm_lab_read` is the explicit full-document read. Failed mutations are
transactional, and component removal refuses until its links are removed
explicitly.

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
consumption, or journal sequencing. The MCP surface now exposes 61 tools. The
live workflow records nine reachability observations spanning baseline,
overlapping faults, controller reconstruction, and targeted heals, for a
47-action canonical journal; the forty-eighth request is refused by the lease
budget.

`proofstorm_artifact_export` turns a closed experiment into a deterministic,
content-hashed evidence bundle without consulting Kubernetes. The default
response is a compact manifest containing its identity, revision and lock
digests, byte length, journal/artifact counts, and a stable `resource_uri`.
Agents can read that URI through MCP `resources/read` when they deliberately
need the complete bundle, or use `proofstorm_evidence_section_read` to inspect
a bounded revision/lock JSON Pointer, a paged journal, or one selected artifact.
`include_content: true` remains an explicit compatibility opt-in for embedding
the complete immutable lab revision and
resolved lock, a canonical projection of up to 100 terminal actions, all oracle
artifact bodies by default, and up to 16 explicitly selected sanitized
artifacts. The content is capped at 512 KiB and omits runtime resource names,
instance keys, component credentials, private payment material, and unbounded
logs. Export requires both `experiment.read` and `artifact.read`; it does not
consume a lease action.

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

CDK 0.18.0 mints may select either an exact LND or CLN BOLT11/sat backend. The
CLN path mounts the selected node's compiled state claim read-only, configures
its regtest Unix socket, and explicitly disables BOLT12 until that capability
has its own LDK-backed contract. Exercise the complete MCP materialization and
live binary/configuration check with:

```sh
make e2e-cdk-cln
```

CDK 0.18.0 also has a distinct embedded-LDK runtime. It links the mint directly
to a selected Bitcoin Core node, persists the LDK node in the mint's own state,
and exposes its P2P port without exposing the loopback administrative UI. A
usable BOLT12 offer requires an introduction path, so the live acceptance adds
a real CLN peer, connects it to embedded LDK, requests an actual 100-sat `lno`
offer through the mint API, and verifies teardown:

```sh
make e2e-cdk-ldk
```

The distinct CDK-BDK runtime uses CDK 0.18.0's standard image, where BDK is a
default feature, and links an on-chain-only mint directly to a selected Bitcoin
Core node. Its bounded stress acceptance creates 24 concurrent NUT-30 address
quotes, exercises agent-authored input fees, keyset-v2 policy, quote lifetimes,
mint/melt bounds, NUT-06 metadata, in-memory cache policy, and transaction input
and output limits in the live native configuration and API, funds and confirms
selected quotes, checks the authored minimum-deposit boundary, restarts the mint
to prove persistence, and verifies teardown:

```sh
make e2e-cdk-bdk-stress
```

CDK 0.18 makes the database, rather than a startup TOML, authoritative for mint
configuration. Proofstorm therefore validates the immutable generated document
and runs `config init --new-mint` in a dedicated init container before starting
`cdk-mintd` without the legacy `--config` flag. On restart, the initializer
reads the stored configuration and refuses to start if it differs from the
resolved Proofstorm lock; it never silently reapplies changed settings. Secrets
use CDK's `env:` and `file:` references, and PostgreSQL receives only its
bootstrap connection setting through a Secret. Locks from the 0.17 configuration
contract are rejected rather than reinterpreted as 0.18. Retained 0.17 databases
must use CDK's explicit upstream migration workflow or be replaced by a new lab.
CDK 0.18.0's BDK startup can leave an empty `bdk_wallet.sqlite` when its first
Bitcoin RPC request fails; later starts then fail the persisted-wallet preflight
instead of retrying initialization. Proofstorm does not delete or recreate that
state. Its BDK and embedded-LDK pods first pass a bounded, authenticated
`getblockchaininfo` dependency gate, so wallet initialization begins only after
the selected regtest node is actually RPC-ready.
The exact pinned standard and LDK binaries validate every generated backend and
Compose document with:

```sh
bash tests/cdk18-config-contract.sh
```

Nutshell mint parity is the current control-plane increment. Nutshell 0.20.3 is
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
agent-authorable and rollout-affecting. Health checks do not spend that authored
request budget: workload readiness calls `/v1/info` over Nutshell's
rate-limit-exempt loopback path, while the credential-free lab protocol prober
checks Service-DNS reachability over TCP. Probe policy remains controller-owned
rather than agent-authorable. OIDC environment and topology wiring
are complete: the
exact Nutshell 0.20.3 NUT-21 clear-auth and NUT-22 blind-auth settings,
PVC-backed authentication ledger, and discovery/client policy are typed and
rendered. A typed `authentication_backend` link selects digest-pinned Keycloak
25.0.6 with mandatory PostgreSQL storage, a topology-derived discovery URL,
bounded JVM heap, and controller-generated administrator, realm-import, and
disposable test-user credentials. The generated public client includes the
standard subject-bearing scopes and optional offline access. Native BAT
issuance remains blocked by an upstream Nutshell 0.20.3 auth-ledger migration
defect: its auth `promises` table does not match the shared CRUD write path.
Proofstorm does not rewrite that schema. Non-LND/non-CLN payment backends and management RPC are not
advertised until matching dependency and secret contracts exist. The current
live acceptance gates are:

```sh
make e2e-nutshell-mint
make e2e-nutshell-cln
make e2e-nutshell-postgres
make e2e-cross-implementation-wallet
make e2e-nutshell-oidc
```

The OIDC conformance gate now drives authentication through three typed
`authentication.test` actions. Fixed in-lab Jobs consume the generated
test-user credential through Kubernetes Secret references, obtain a real
Keycloak access token with Nutshell's native `WalletAuth`, check the
issuer/client/subject/lifetime claims, exercise the NUT-21 and NUT-22
missing/invalid and policy failures, mint DLEQ-backed BATs, and spend one
against a protected endpoint. The controller retains that spent BAT in an
immutable, instance-scoped Secret identified to MCP only by its source
operation; after a mint restart, the replay action requires spent-token
rejection and proves a fresh BAT still works. Neither credentials nor bearer
tokens cross the MCP boundary. Against upstream 0.20.3 the typed baseline
currently reproduces the auth-ledger schema failure and therefore cannot yet
reach the protected-spend and restart/replay exit gates.

CDK is not claimed as an authenticated Nutshell client by this gate. CDK clients
through 0.18.0 complete OIDC login and refresh against the generated Keycloak
client, but the CLI does not retain the discovered auth settings in the wallet.
CDK 0.18.0 also models `input_fee_ppk` as a non-null
`u64`, so it discards Nutshell 0.20.3's auth keyset response when that field is
`null`. These are upstream conformance findings, not Proofstorm compatibility
shims.

The cross-implementation gate materializes CDK 0.18.0 and Redis-backed
Nutshell 0.20.3 in one lab, drives both through the same pinned Nutshell wallet
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

Nutshell's wallet database is authoritative for receive and melt quote facts.
Proofstorm stores immutable, attributed observations of those adapter records;
it does not maintain a second quote phase machine. Receive and pay observations
use their distinct adapter-native mint and melt quote IDs. The
capability-filtered `proofstorm_wallet_quote_status` and
`proofstorm_wallet_quote_list` tools explicitly return the latest stored
observation rather than live mint state. List pages use a digest-bound cursor.

`proofstorm_wallet_invoice` returns after creating the receive quote, exposing
only its mint quote ID and sanitized `UNPAID` observation. Its BOLT11 request is
captured in a mode-0600 pod-local temporary file and removed on exit.
`proofstorm_wallet_pay` privately reads the exact recipient row, atomically
reserves that mint quote against duplicate payment operations, correlates the
new payer melt row, and claims the recipient quote after a paid melt. A paid
but unverified claim is never replayed; `proofstorm_wallet_quote_claim` is the
bounded, idempotent recovery path and also supports externally paid invoices.

## Legacy Compose harness

The original Docker Compose wallet-population runner and the regtest
adversarial harness moved to [`docs/compose-harness.md`](docs/compose-harness.md).
Its targets run through `make compose-<target>`.
