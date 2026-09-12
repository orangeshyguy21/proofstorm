# Component image sourcing

The Rust component catalog is the authority for supported versions and immutable
runtime image digests. Every built-in catalog image is served by the local
Proofstorm registry. `storm setup` prepares the installation; cell creation
downloads the selected images on demand, preserving their exact digests.
The `upstream/<registry>/<repository>` path records their original source.

| Component | Source |
| --- | --- |
| Bitcoin Core 31.1 | Proofstorm packaging of verified bitcoincore.org release binaries |
| LND 0.21.3-beta (preferred), 0.20.4-beta | docker.io/lightninglabs/lnd |
| Core Lightning | docker.io/elementsproject/lightningd |
| CDK and Nutshell | docker.io/cashubtc images, with the existing management-client wrappers |
| Keycloak | quay.io/keycloak/keycloak |
| PostgreSQL, Redis, BusyBox | Docker Official Images under docker.io/library |
| Cocod | Frozen source and dependency lock in its wallet provenance record |

The catalog no longer contains Bitcoin Core 30.0 or the old Polar LND builds.
The old Compose stacks and fixed-registry fixtures have been removed. Historical
experiment reports retain the versions they actually ran. `just check-cdk-config`
validates the generated CDK configurations against their pinned public images
without creating a cluster.

## Bitcoin packaging

`bitcoin/Dockerfile` downloads unmodified Bitcoin Core 31.1 release binaries
for Linux amd64 and arm64. It checks the pinned SHA-256 of the release checksum
document, requires a valid Michael Ford signature with pinned primary and signing
key fingerprints, and verifies each architecture's archive against that signed
document. The signer key is fetched from an immutable bitcoin-core/guix.sigs
commit. Unavailable keys for other signatures in the release bundle are expected;
the build explicitly requires the selected signer's `VALIDSIG` record.

The runtime uses a digest-pinned Debian base, uid/gid 1000, and
`HOME=/home/bitcoin`. Regtest arguments come from Proofstorm; the daemon source
and protocol behavior are unchanged. The provenance record identifies the source
commit, signed checksum document, architecture-specific archive hashes, base
image, and recipe digest.

## Build or publish a catalog image

This is maintainer work, separate from `storm setup`. Use a clean checkout,
Rust, Bash, just, curl, and Docker with Buildx. The workflow uses the existing
reviewed recipes; it does not change versions, catalog pins, or provenance.

```sh
just catalog-image list
scratch="$(mktemp -d)"
just catalog-image build cdk-cli-wallet linux/arm64 "$scratch/wallet"
# Inspect image.json and probe.stdout before authorizing a push:
just catalog-image publish "$scratch/wallet" \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
```

Select `linux/amd64` or `linux/arm64` explicitly. Buildx must support that platform
and the host must be able to execute its native probes (directly or by emulation).
Available recipes: `bitcoin-core`, `cdk-mint-management`,
`cdk-ldk-mint-management`, `nutshell-mint-management`, `cdk-cli-wallet`, and
`cocod-wallet`. Controller builds remain in the existing
[`release-controller-*` flow](../scripts/RELEASING.md).

Builds retain a clean source snapshot, recipe hash, exact image ID, source label,
and restricted offline version/help probe. Cocod's downloaded source archive is
also checked against its recorded hash. A probe proves executable startup, not
cell behavior; run the affected acceptance gates before changing the catalog.
Bitcoin/wallet probes require the recipe's non-root user; mint wrappers retain
their upstream user. Cross-architecture emulation is not native-host acceptance.

To copy an existing reviewed image instead of rebuilding it:

```sh
just catalog-image prepare-copy \
  '127.0.0.1:PORT/cdk-cli-wallet@sha256:DIGEST' linux/amd64 "$scratch/copy"
just catalog-image publish "$scratch/copy" \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
```

Replace `PORT` and `DIGEST` with the explicit source registry and reviewed digest;
a public GHCR digest reference is also accepted. There is no implicit port 5111
or development-cluster fallback. Copies preserve the complete manifest digest,
including both architectures when present. Every runnable manifest/config is
hash-checked and its layers checked for availability before and after upload.

Only `publish` writes to GHCR, using Docker's existing package-write login and a
unique `upload-*` tag. Existing version/development tags are not overwritten.
Packages must be Public; verification uses anonymous requests, not your login.
`image.json` distinguishes prepared, upload-attempted, uploaded, and verified
states. A failed push may already have uploaded content: retain the directory
and inspect it before taking action. Once upload is recorded, retry read-only
verification with `just catalog-image verify-work "$scratch/wallet"`.
A moved tag is refused, and a failed recheck leaves the receipt unverified.

For an independent anonymous check:

```sh
just catalog-image verify \
  'ghcr.io/orangeshyguy21/proofstorm/cdk-cli-wallet@sha256:DIGEST' \
  linux/amd64 "$scratch/verification.json"
```

Receipts do not claim release readiness. Review the new immutable digest and
provenance, update the catalog deliberately, and run its contracts/live gates.
CI then builds a matching controller with the normal release flow. A source
rebuild is not a substitute for a missing approved artifact during installation.

## Updating a component

Use a project-published image when available; otherwise package verified official
release artifacts and retain provenance. Pin the multi-architecture digest,
validate native commands and data paths under the restricted runtime, update
catalog compatibility declarations and examples, regenerate coverage and render
fixtures, and run the affected live gates. Keep application version updates
separate from refreshes of the OS layers beneath an unchanged application version.

Do not use mutable tags as catalog locks or automatically rebuild missing local
artifacts during setup. The registry copy is an availability measure; digest
preservation and release verification establish what code is running.

## Migration verification — 2026-09-08

This migration updates Bitcoin and LND versions and changes catalog image
sourcing. Other component versions are unchanged; it is not a claim that every
database, identity provider, or base image is on its newest release.

The workspace suite passed 372 tests (one intentionally ignored fixture).
Workspace Clippy with warnings denied and formatting checks also passed.
Live checks passed for cluster schemas, exact image pulls on both cluster nodes,
MCP materialization and teardown (`slice4`), Nutshell with LND, CDK embedded BDK
deposits and restart persistence, CDK embedded LDK BOLT12 quotes, and the CDK
wallet mint/payment/restart checkpoint. Both LND 0.20.4-beta and 0.21.3-beta also
passed fresh Compose funding and channel-opening checks with Bitcoin 31.1.
Bitcoin packaging was built and release-verified for amd64 and arm64; live
integration checks ran on the local arm64 cluster.

The migration initially exposed obsolete `slice5` lifecycle and conservation
expectations. The follow-up refactor replaced that monolith with independently
runnable `slice5`, `controller-recovery`, `network-faults`, and
`channel-lifecycle` gates. All four passed live checks, including named-operation
journals, deterministic evidence, and incarnation-fenced teardown. Seven harness
unit tests, Clippy with warnings denied, and formatting checks passed as well.
A deliberately injected error while the controller was stopped restored it and
reclaimed the test cell; the idle-cluster safeguard also refused a concurrent run.
See the main README for the fixture boundaries and commands. In particular,
the conservation smoke fixture uses Nutshell's authoritative fee database;
the existing CDK wallet checkpoint remains the CDK payment integration check.

The two pre-existing disposable test cells and all migration-created cells were
removed, including their persistent test storage. Historical reports were kept.
