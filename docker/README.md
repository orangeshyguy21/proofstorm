# Component image sourcing

The Rust component catalog is the authority for supported versions and immutable
runtime image digests. Every built-in catalog image is served by the local
Proofstorm registry. `make images` copies publisher images without rebuilding
them, preserving their complete multi-architecture manifest and exact digest.
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
The Compose regtest defaults use the same Bitcoin and preferred LND digests via
the host registry address `localhost:5111`. Run `make images` before bringing up
that stack. Historical experiment reports retain the versions they actually ran.

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

Build and push to the local registry:

```sh
make bitcoin-image-build
```

The resulting manifest digest is recorded in
`.tools/downloads/bitcoin-31.1-build.json`. Review it before changing
`catalog.rs`, the Compose defaults, and `regtest/versions.env`. A source rebuild
can produce a different image digest; `make images` deliberately never replaces
a reviewed local artifact with a fresh build. Preserve the exact approved image
in the registry or export it with Docker for restoration on other machines.

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
reclaimed the test lab; the idle-cluster safeguard also refused a concurrent run.
See the main README for the fixture boundaries and commands. In particular,
the conservation smoke fixture uses Nutshell's authoritative fee database;
the existing CDK wallet checkpoint remains the CDK payment integration check.

The two pre-existing disposable test labs and all migration-created labs were
removed, including their persistent test storage. Historical reports were kept.
