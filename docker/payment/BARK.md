# Bark processor build foundation

This recipe prepares the processor for the first BOLT11/sat integration. It is
not a catalog entry or a qualified Bark cell. The experimental Rust probe assembles
the server, PostgreSQL, CLN/hold, a Lightning peer, processor, CDK mint and CDK
wallet. Managed cell integration and native qualification on both architectures
remain required before advertising support.

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
- `BARK_SERVER_ADDRESS` derived from the future owned server link
- `BARK_BITCOIND_ADDRESS` derived from the owned Bitcoin link
- `BARK_BITCOIND_COOKIEFILE`, an absolute private file containing `user:password`
- No `BARK_ESPLORA_ADDRESS`

RPC credentials are referenced by path, never embedded in the patch or command
arguments. File ownership and projection remain obligations of the future
renderer. The wallet requires Bitcoin `txindex=1`. Existing explicit Esplora
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
The existing three-argument command retains its LDK profile; it is not auto-detected
from the response. Both profiles reject extra rails. Timeout and reply-size bounds
are unchanged. This profile selector does not enable Bark in the catalog, topology
validator or renderer.

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
