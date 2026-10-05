# Bark integration preview

## Current status

The three native ARM64 images have been built from committed source `a31034f`
and passed their offline identity probes. The processor's patched upstream
library suite passed all 37 tests. Their exact image digests are staged in the
experimental ARM64 catalog; CDK 0.18.1 accepts the explicit BOLT11/sat processor
binding. There is no default Bark version and no AMD64 catalog entry.

These images are currently local preview artifacts. A development installation
must seed the verified images into its owned local registry before creating a
Bark cell. The full managed gate passed locally on native ARM64 containers on
macOS on 2026-10-04, including payment/recovery, transport refusal, cell and runtime
cleanup, and unrelated-resource preservation. Public GHCR distribution and native
AMD64 qualification remain pending. Do not treat this branch as a distributable
release or remove the exploratory probe yet.

Normal qualification, `setup --prefetch-all` and the release workload-image inventory
use the distributable catalog. That catalog excludes the three local Bark images
and CDK's preview-only Bark binding together. Published experimental components
remain included. Runtime resolution still admits the explicit ARM64 preview;
an unseeded preview image fails with a local-seeding diagnostic instead of trying
an unpublished GHCR path. Bark continues to use its dedicated managed gate below.

## Build foundation

This recipe builds the processor for the first BOLT11/sat integration. The
experimental Rust probe assembles
the server, PostgreSQL, CLN/hold, a Lightning peer, processor, CDK mint and CDK
wallet. Managed ARM64 cell qualification is now complete locally; native AMD64
qualification remains required before advertising release support.

## Pinned inputs

- Processor collection: `fe468cad486157683eddbc0df4ff87ba71b6c0a3`.
- Source archive SHA-256:
  `236d4caa1c1e1643ba8b9f40c7af4c478f1afddf09be86520317a5951e4073e9`.
- Package: `crates/bark`, binary `cdk-payment-processor-bark`.
- Unchanged Cargo.lock SHA-256:
  `b68c8b98aaf7399f97e45062f2f291b50ae381754387b6c87a54ea6affd06691`.
- Wallet: published `bark-wallet` 0.7.0, whose source pairing is Bark server
  `6188e2d809f193716b2e571274179f069d9c19ca`. The server is not built by this recipe.
- Local transformation: [RPC patch](patches/bark-regtest-rpc.patch), SHA-256
  `abc3f967d754cdf5e484bf434ef52fa216dfb37cdcbb8fd896477e5c7b40321c`.

[Dockerfile.cdk-bark](Dockerfile.cdk-bark) verifies source, patch and lock checksums,
applies the patch with zero fuzz, runs the upstream library unit tests including
the new configuration tests, then builds the executable with `--locked`. The
optional upstream `regtest-tests` suite is not run by this image build. Both base
images are pinned in the recipe; Debian packages still resolve through its apt
repositories. This records the inputs without claiming bit-reproducible builds.

From the repository root, on a native ARM64 builder:

```sh
docker build --platform linux/arm64 -f docker/payment/Dockerfile.cdk-bark \
  -t proofstorm-cdk-bark:dev-rpc .
```

Use a native AMD64 builder for AMD64 qualification. Emulation does not establish
native runtime support. No image is published by this command.

## RPC configuration contract

The patch removes implicit public server and Esplora endpoints. It requires an
explicit Bark server and exactly one chain source. The RPC path requires:

- `BARK_NETWORK=regtest`
- `BARK_SERVER_ADDRESS` derived from the owned server link
- `BARK_BITCOIND_ADDRESS` derived from the owned Bitcoin link
- `BARK_BITCOIND_COOKIEFILE`, an absolute private file containing `user:password`
- No `BARK_ESPLORA_ADDRESS`

RPC credentials are referenced by path, never embedded in the patch or command
arguments. The processor renderer projects the existing fixed regtest credentials
through its own read-only configuration volume. The wallet requires Bitcoin
`txindex=1`. Existing explicit Esplora
configurations remain usable when RPC and its credentials are absent.

The runtime must explicitly select `BARK_PAYMENT_METHODS=bolt11`; upstream's
empty method list still means all rails. Its stable mnemonic and complete data
directory must be preserved together, including `db.sqlite` and
`onchain_state.redb`. This recipe does not create a second wallet initializer or
manage volumes. The separate probe owns volumes and exercises wallet restarts;
the image alone does not establish persistence or settlement.

## Authenticated capability check

The shared driver accepts an explicit profile:

```sh
/opt/proofstorm/driver processor-settings https://processor:50051 \
  /payment-processor/tls cdk-bark-processor
```

It sends protocol `4.0.0`, authenticates both peers, and requires `sat`, BOLT11,
no BOLT12, no on-chain settings, and an empty custom-method map. This checks the
actual protobuf fields, including methods CDK would otherwise register silently.
The catalog profile argument is required; it is not auto-detected from the
response. Both profiles reject extra rails. Timeout and reply-size bounds
are unchanged.

Core topology validation, mint rendering and the driver share one explicit
processor contract:

| Processor | Authored mint bindings | Native GetSettings unit |
| --- | --- | --- |
| `cdk-ldk-server-processor` | BOLT11/sat and BOLT12/sat | msat |
| `cdk-bark-processor` | BOLT11/sat only | sat |

Each mint must bind every method in its selected profile exactly once to one
processor component. Missing or duplicate bindings, mixed endpoints/profiles,
unknown processors, other units and extra methods are rejected. Rendering checks
the compiled descriptors again, selects that processor's authenticated readiness
profile, and projects only its CA and client certificate/key into the mint.
The existing LDK processor-to-node contract still requires both methods.

The ARM64 preview catalog now admits this mint-side boundary. Rendering and
identity/storage contracts have automated coverage. The local ARM64 managed gate
also passed; native AMD64 qualification remains a separate requirement.

## Managed dependency topology

The reserved graph uses an `ark_server` component kind and an `ark_backend`
link. The Ark protocol dependency is distinct from both a Bitcoin chain binding
and a Lightning payment binding:

| Source | Link kind | Target | Required binding |
| --- | --- | --- | --- |
| `cdk-bark-processor` | `ark_backend` | `bark-server` | `type: ark`, `network: regtest` |
| `cdk-bark-processor` | `chain_backend` | `bitcoin-core` | `type: chain`, `network: regtest` |
| `bark-server` | `chain_backend` | `bitcoin-core` | `type: chain`, `network: regtest` |
| `bark-server` | `database_backend` | `postgresql` | `type: database`, `role: primary` |
| `bark-server` | `payment_backend` | `cln-hold` | `type: payment`, `method: bolt11`, `unit: sat` |
| `cln-hold` | `chain_backend` | `bitcoin-core` | `type: chain`, `network: regtest` |

Each dependency is required exactly once. The processor, server and CLN/hold
node must reference the same Bitcoin component with `txindex=true` (the Bitcoin
backend default). Separate regtest nodes do not satisfy this identity check.
Missing or duplicate links, wrong target kinds/implementations, extra backend
dependencies and mismatched bindings fail validation. Peer and network-path
links retain their existing rules. MCP accepts the Ark network as a flat field
and preserves its typed binding when importing canonical cell documents.

The graph fixture in `crates/proofstorm-core/tests/fixtures/bark-topology.json`
tests these requirements and resolves against the ARM64 preview catalog.

## Managed processor backend

The CDK Bark processor has a typed backend and Kubernetes renderer. Its only authored setting is
`event_poll_interval_ms` (default 5000, supported range 1–60000). Network, payment
methods, endpoints, storage paths and credentials are managed settings.

The processor runs as one StatefulSet with the complete `/data` directory on its
owned PVC. Both `db.sqlite` and `onchain_state.redb` remain together. The renderer
derives server and Bitcoin endpoints from the typed links, waits for both
dependencies, explicitly selects regtest/BOLT11, and requires authenticated
`GetSettings` with the Bark profile before readiness. It neither mounts another
component's data volume nor exposes a public chain-service fallback.

The controller creates a private mnemonic once and preserves it on reconciliation
and create races. An absent identity while the owned PVC exists, or an invalid or
incomplete identity Secret, fails provisioning without rotation. The Rust startup
driver loads that private file into the native process environment; the seed is
never placed in the authored plan, pod environment specification or command line.
The driver binds an identity hash to fresh storage before the native executable
opens it. Subsequent starts require the same seed and both nonempty database
files. Missing markers, changed seeds, partial databases and interrupted first
initialization are refused instead of automatically creating another wallet.
Restoring the retained identity/state or explicitly resetting an unused component
is required after such a refusal; no automatic repair is attempted.

Processor gRPC uses the existing controller-generated mutual TLS contract.
Certificate projections contain the required role's material only; the mint gets
CA/client credentials, never the processor seed or server key. Bitcoin RPC uses
the application's existing fixed regtest credentials in a component-scoped,
read-only `rpc.cookie` ConfigMap. These constants are not newly generated secrets.

Renderer, provisioning and filesystem-contract coverage is backed by the local
ARM64 managed payment/restart result. Native AMD64 receipts are still pending.

## Managed server and CLN/hold backends

The reserved `bark-server` and `cln-hold` backends now render separate StatefulSets
with complete owned `/data` volumes. This preview fixes native tuning to the
pinned upstream defaults and managed regtest settings; it accepts no arbitrary
native configuration. Endpoints come from validated typed dependencies. Both
components wait for Bitcoin RPC readiness before initializing fresh state.

Bark uses the image's pinned configuration template with explicit environment
overrides. Its public service exposes only port 3535; admin and integration RPC
remain on loopback. PostgreSQL's preserved owner password comes from the linked
database Secret. The initializer runs native `captaind create` only on fresh
storage, using a private `/data/native` directory owned by its non-root process
so native permission hardening does not target Kubernetes' volume root. It then
binds the native mnemonic and database/chain identity to a retained
fingerprint. A separate PostgreSQL check seals and verifies that fingerprint in
`proofstorm.identity`. Restarts require that seal and the existing native tables;
they never create a replacement database. Partial first initialization requires
explicit recovery. This does not provide per-consumer PostgreSQL roles or detect
arbitrary modifications to individual payment rows.

Use `component_exec_live` inside the Bark workload for privileged
`captaind rpc` commands; an isolated forensics job cannot reach its loopback API.

CLN and hold have separate controller-generated TLS identities. Their CA signing
keys remain inside their own workload, where the native certificate loader needs
them; Bark receives only the CA certificate and client certificate/key for each
API. Missing TLS Secrets with retained storage and incomplete existing Secrets
are refused. Native TLS files, the CLN HSM identity, Lightning SQLite database and
hold SQLite database persist together. Readiness checks both native APIs and
listeners before sealing first initialization. Restarts refuse missing stores,
changed keys and interrupted initialization rather than generating a new wallet.

Bitcoin credentials retain the existing fixed regtest contract. Bark receives a
scoped read-only cookie; CLN receives a scoped read-only native configuration
file. No Bitcoin or CLN data volume is projected into another component.

These contracts have offline rendering, credential and filesystem tests plus a
local PostgreSQL guard test. Managed settlement, pending-payment recovery,
transport refusal and teardown passed in the local ARM64 gate. Native AMD64 image
qualification remains a release gate. Both backends are staged
only in the ARM64 local preview catalog.

## Managed qualification and publication

### Native candidate image handoff

The separate **Bark native candidate images** workflow
(`.github/workflows/bark-images.yml`) is manually triggered. Select `amd64`
(default) or `arm64`; all three recipes build in separate jobs on matching
native Linux runners. It checks both host and Docker engine architecture,
selects the local builder with an isolated anonymous Docker configuration, and
uses the ordinary clean-source/provenance/offline-probe checks. It does not add
Bark builds to ordinary pull-request CI, publish packages or change catalog pins.
For qualification before merge, use the existing **Candidate component packaging**
entry point from the reviewed, pushed branch. Its new `family=bark` choice calls
the Bark workflow [from that same revision](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows#calling-a-reusable-workflow)
and skips the standard component matrix:

```sh
gh workflow run component-images.yml --ref REVIEWED_BRANCH \
  -f family=bark -f architecture=amd64
```

Replace `REVIEWED_BRANCH` with the branch containing the reviewed workflow and
scripts. The standard family remains the default, and PR behavior is unchanged.
The dedicated Bark workflow also has its own manual entry point after it reaches
the default branch, following
[GitHub's dispatch requirements](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).

Each successful job retains `image.tar`, the frozen build work in `work.tar.gz`,
native host metadata, build/inspection/probe receipts and `SHA256SUMS` for 14 days.
The image is exported by its verified immutable image ID. Credentials, Docker
configuration, compiler caches and live runtime state are excluded. A failed
build/export can retain diagnostic files; absence of `SHA256SUMS` means the
handoff is incomplete. Download artifacts from the same selected workflow run
and revision, and keep them locally before retention expires.

From the repository root, restore one trusted artifact on the matching
architecture and recheck its exact image and frozen source:

```sh
artifact=/absolute/path/to/downloaded-artifact
restored=/tmp/bark-build-restored
(cd "$artifact" && sha256sum --check SHA256SUMS)
mkdir "$restored"
tar -xzf "$artifact/work.tar.gz" -C "$restored"
docker image load --input "$artifact/image.tar"
just catalog-image verify-local "$restored"
```

`verify-local` checks the recorded image ID, architecture, source label, frozen
source and offline native probe without rebuilding or contacting GHCR. It does
not assert native-host acceptance or publication. The retained `local_image_id`
is a Docker-local identity, **not sufficient evidence of a catalog manifest digest**.
Import the exact image into the qualification installation's owned registry, verify its manifest,
config/platform and layers, then review and stage those real catalog pins before
running the managed gate. No AMD64 catalog entry is enabled by the build workflow.

The same build/export can be run on an existing native Linux builder:

```sh
bash scripts/bark-image-build.sh bark-server@0.7.0-6188e2d \
  linux/amd64 /tmp/bark-server-native-new
```

This requires a clean committed checkout, Rust, Git, jq, GNU coreutils, tar and
Docker with Buildx. Work must be new and outside the checkout. The images are
candidates until their matching managed settlement/recovery/cleanup gate passes.

### Managed gate

The maintainer image workflow recognizes `cdk-bark-processor@0.1.0-fe468ca`,
`bark-server@0.7.0-6188e2d` and `cln-hold@26.06.7-hold.0.3.3`. The adjacent
`*-provenance.json` records bind the source archives, lockfiles, build/runtime
bases and recipe hashes. The processor also binds the regtest patch hash.
`platform` declares recipe targets; it is not evidence of a successful build
or payment qualification on either architecture.

From a reviewed, clean committed checkout, build each selected platform with
`just catalog-image build RECIPE@VERSION PLATFORM NEW_EXTERNAL_WORK`. This
retains the frozen source and offline probe receipts without publishing. The
normal explicit publication workflow remains separate. Existing prototype
images are useful for offline diagnostics but are not publication receipts.

The Rust acceptance gate `bark-processor` uses the ordinary planner, managed
controller, generated Secrets and native component execution. It requires all
three immutable image entries with provenance in the selected catalog; it does
not substitute local tags or synthesize lock entries. Once those entries and the
matching controller are staged for qualification, run it in the usual isolated
acceptance installation:

```sh
just e2e bark-processor --work-dir /tmp/bark-managed-arm64-01 --timeout 3600
```

For the local preview, add `--qualification-image-cache 127.0.0.1:PORT`
pointing at an explicitly prepared loopback cache. As with other qualification
gates, it must contain the exact locked component images plus the probe image
under `images/DIGEST@sha256:DIGEST`. The runner resolves the Bark fixture through
the ordinary catalog, seeds only those images into the new installation and
fails if the cache is incomplete. It never substitutes a tag or falls back to
an upstream download for a missing cache entry.

An explicit `--bootstrap-tool-cache PATH` can reuse the three installed host
tools from a previous installation's `tools` directory. The runner checks every
file against the current bootstrap pins and copies only those executables into
fresh test state. Missing, modified or linked files fail before runtime setup.
This option is refused for onboarding, which tests tool installation itself.
The prepared-image scripts also require GNU `timeout`; on macOS, install
Homebrew `coreutils` and put its `libexec/gnubin` directory on `PATH`.

The gate funds the server and a bidirectional Lightning channel. Its mint sets
`input_fee_ppk=0` so the native wallet fee can be compared directly with the
processor's melt fee reserve; it does not qualify nonzero Cashu input fees.
It verifies:

- BOLT11/sat processor settings and real gRPC refusal of plaintext, missing
  client certificates and another service's client identity on all three TLS
  endpoints. Successful authenticated calls bracket the refusal checks.
  Each negative probe gets a fresh authenticated control tunnel because
  `kubectl port-forward` can exit on a rejected TLS connection. The gate requires
  unchanged pod identities and container restart counts across these checks.
- An unpaid mint quote and its native hold invoice across CLN/hold, PostgreSQL,
  Bark server, processor and mint restarts. Pod replacement, identity/Secret
  fingerprints, state seals and mint keysets are checked independently.
- A 100,000 sat incoming payment interrupted with the processor stopped. The
  gate must observe an accepted HTLC before resuming the processor, then settle
  and claim the original quote without submitting a second payment.
- A 30,000 sat melt, completed-state recovery across stack restarts and a second
  10,000 sat melt. Recipient invoice identity/amount, mint quote, native receipt,
  fee bounds and passive wallet conservation must agree.
- Cell removal even after an exercise failure, a verified teardown receipt,
  and independent namespace/action and owned volume absence inside the test cluster.

Recovery checks reconcile the original mint or melt quote after independently
verifying settlement. The pinned CDK/Bark combination can miss a streamed payment
notification across a processor restart; a quote-status read reconciles that
payment without sending it again. These checks establish recoverability through
quote reconciliation, not uninterrupted notification delivery. Native wallet
completion, exact amounts and passive balances remain required. Quote, recipient
and native receipt evidence is retained before validation, and the private native
melt log is captured before cleanup even after failure.

The complete local ARM64 result is retained in the ignored directory
`dev/bark-qualification-2026-10-04/run-arm64-08/`. It minted 100,000 sat, melted
30,000 sat with a 195-sat fee, restarted the paid stack, and melted another
10,000 sat with a 115-sat fee. The final passive balance was 59,690 sat with no
pending or reserved value. Cell/storage/runtime cleanup and unrelated-resource
preservation passed. Earlier failed attempts remain in adjacent directories.
This used native Linux ARM64 containers on a macOS ARM64 host, not the native
Linux-host CI workflow. AMD64 qualification remains pending. Keep the exploratory
`bark_stack` example frozen until matching release receipts exist; remove it in
the same change that enables qualified catalog support. Raw acceptance evidence
remains private in the run directory.

## Server and CLN/hold dependency images

[Dockerfile.bark-server](Dockerfile.bark-server) builds `captaind` from the matched
`6188e2d809f193716b2e571274179f069d9c19ca` source. It checks the source archive and
unchanged lock, sets the source revision in the binary's version, retains the
upstream configuration template/license, and runs as UID 1000. Its default command
only starts an existing server. Initialization is a separate `captaind create`
operation, never an implicit action during restart.

[Dockerfile.cln-hold](Dockerfile.cln-hold) adds hold 0.3.3 at
`af0055b132f3b9f24d0b1d478a15005fcf8f014f` to the existing pinned CLN 26.06.7 image.
The hold archive SHA-256 is
`0f225fe33c5640339ba0163751056df78e2ba0af9649febf1f0a6616cd9237c4`, and its unchanged
lock SHA-256 is `4fde8fb58711f547f470d4ddc94b8ff9a0527f4384dab7e2b3139158292dd930`.
The image retains the hold license and source marker and runs as UID 1000.
CLN and hold keep their own TLS identities; only their CA/client credentials
are projected into Bark. The hold process requires its CA signing key locally
when loading its certificates; that key is not projected into Bark.

Build on the native ARM64 host:

```sh
docker build --platform linux/arm64 -f docker/payment/Dockerfile.bark-server \
  -t proofstorm-bark-server:dev-0.7 .
docker build --platform linux/arm64 -f docker/payment/Dockerfile.cln-hold \
  -t proofstorm-cln-hold:dev-0.3.3 .
# Set this to an existing native ARM64 Proofstorm controller image built from
# this repository. The probe extracts /usr/local/lib/proofstorm-driver from it
# for passive wallet observations and records both image ID and binary hash.
export PROOFSTORM_BARK_DRIVER_IMAGE='<local-controller-image>'
CARGO_TARGET_DIR=.proofstorm-dev/target cargo run --locked \
  -p proofstorm-acceptance --example bark_stack -- dev/bark-payments-new
```

The Rust example requires a new evidence directory. It resolves the local image
tags to image IDs and uses the catalog's pinned Bitcoin/PostgreSQL/CDK images. It
creates a private directory, an internal Docker network with no host-published
ports, fresh credentials, and labeled owned storage. It initializes Bark once,
checks repeat-initialization refusal, exercises native CLN hold-invoice storage,
and restarts CLN, PostgreSQL and Bark. It compares the server mnemonic hash,
CLN node identity and pending hold invoice across restarts. SIGINT/SIGTERM request
cleanup; individual Docker commands and readiness polling are bounded.

The funded phase creates a second Lightning node and a channel with liquidity in
both directions, funds the server, and starts a BOLT11-only/sat processor with
mutual TLS. Only server credentials reach the processor, and only client
credentials reach the mint. CDK configuration is explicitly initialized once;
ordinary restarts start the existing database. The fixture uses public BIP39 test
vectors solely for its isolated regtest processor and mint; all payment funds are
generated on its owned regtest chain.

The native CDK wallet requests 100,000 sat, the independent Lightning peer pays
the invoice, and the probe checks issuance and the passive wallet balance. It
then restarts the processor and mint, verifies the original issued quote, and
melts 30,000 sat to the peer. Success requires recipient settlement, a paid mint
quote, the native melt receipt, fee-reserve bounds, and exact wallet conservation
without reserved or pending proofs. Mutations are submitted once; only read-only
observations are retried. Failure still triggers evidence capture and cleanup.

Results and private captures remain in that directory. Raw evidence includes
credentials and is not a public report. Cleanup verifies absence of all resources
with the run's label. The before/after check compares resource IDs/names; it is
not the acceptance runner's full state-preservation check.

This probe targets native ARM64 BOLT11 mint/melt and completed-payment restart
qualification. It does not test on-chain boarding, pending-payment recovery,
uninterrupted database recovery, catalog rendering, or native AMD64 support. The
BOLT11 payment funds the processor's Ark wallet; no second wallet initializer is
used. Only a successful retained run establishes the checks, not merely building
the example. The probe does not publish images or enable catalog support.
