# Linux x86-64 alpha bring-up

The host installer and packaging path now recognize `x86_64-unknown-linux-gnu`
alongside `aarch64-apple-darwin`. Users do not choose a target: `install.sh`
detects the OS/CPU and chooses the archive. Unsupported hosts are rejected.
Bundle verification also checks that the manifest, embedded metadata, and
executing installer agree on the target, including for development bundles.

## Current boundary

This is **not a completed Linux release**. AMD64 controller and wallet images
have been published and anonymously verified (`linux-amd64-publication.json`).
The Linux x86-64 catalog now selects those wallet images and their AMD64 provenance;
the Mac/ARM catalog is unchanged. Because image pins and provenance are part of the
runtime contract, that catalog change required a follow-up AMD64 controller build.
That build passed offline metadata, non-root, and helper startup checks, and has
now been published and anonymously verified. `controller-linux-amd64.json` pins
its exact digest. The refreshed Linux host bundle now includes this pin and passed
the runtime-contract match, package integrity, and relocated executable checks.
Source-free install/reinstall also passed for this new archive in offline,
non-root Debian. Runtime testing remains. The previous bundle still has no
controller and must not be used.

The normal GitHub alpha installer is the next test surface. Full-runtime and
fresh-VM checks are alpha validation work, not prerequisites to letting testers
install an alpha. Keep these limitations visible and do not claim broad or stable
Linux readiness until these checks pass:

- Build CLI and MCP with the same embedded GUI and source metadata on Linux x86-64.
- Pin and verify an AMD64 controller with the matching runtime contract.
- Audit the catalog/helper images for AMD64; rebuild/publish ARM-only custom
  images as needed. Do not retag ARM images as AMD64 or change existing digest
  meanings. Linux wallet pins are updated; existing ARM pins are unchanged.
- Test the downloaded archive in a fresh Linux VM, including full setup, an
  agent's MCP discovery, lab creation/read/teardown, interruption, and reinstall.
- Validate a distribution/glibc baseline before claiming broad Linux support.

The release builder retains these maturity limitations in alpha metadata without
blocking normal alpha installation. Development images have been published;
no GitHub Release or public installer-download flow has been verified.

Checked-in provenance already lists AMD64/ARM64 for Bitcoin Core and the CDK
management/LDK mint images. Separate AMD64 provenance now accompanies the rebuilt
CDK CLI wallet and cocod wallet images. These records do not replace anonymous
registry/platform verification.

## Maintainer build

Build natively on Linux x86-64 (or an isolated AMD64 build container). This builder
does not cross-compile or execute Linux payloads on macOS. Use Python 3.12+, the
repository's Rust toolchain, the WASM target, and pinned Trunk. Installed users
do not need these build tools.

```sh
just web-tools
scratch="$(mktemp -d)"
python3 scripts/release.py build --development \
  --work-dir "$scratch/build" --output "$scratch/artifacts"
```

The archive name ends in `x86_64-unknown-linux-gnu.tar.gz`. Build on the oldest
glibc baseline we intend to support; a build made on a newer distro must not be
assumed compatible with an older one. Alpine/musl is not a supported host target.

Run `scripts/release.py smoke` on the Linux host with a new destination outside
the source/build trees. `--deny-source` is macOS-only and now explicitly refuses
Linux; use a source-free container/VM for Linux's negative-access test.

Local installer rehearsal uses the exact archive name returned by the builder:

```sh
sh install.sh --artifact-dir /absolute/path/to/artifacts \
  --archive NAME.tar.gz --prefix /absolute/path/to/disposable-prefix \
  --allow-development
```

## Disposable Docker packaging rehearsal

On a Mac, the following maintainer command builds a real Linux development
archive using Docker's AMD64 emulation. The default channel follows the workspace
version (currently alpha); `--development` explicitly selects a scratch bundle.
It is not a first-user install command:

```sh
scratch="$(mktemp -d)"
python3 scripts/linux_container.py build --debug --work-dir "$scratch/build"
```

The toolchain image uses pinned Rust 1.88/Python 3.12 Debian Bookworm bases and
the repository's pinned Trunk. Only a Dockerfile is sent as the toolchain build
context. A separately hashed source snapshot is copied into the build container;
no checkout, personal home/config, host Docker socket, or host directories are
mounted. Compilation runs in Linux storage with 2 CPUs, 3 GiB RAM, all capabilities
dropped, and no-new-privileges. LLD and stripped debug information reduce the
emulated linker overhead. The initial toolchain image build is separate from
those compilation resource caps. This remains slower than a native AMD64 host.

The worker builds the GUI, CLI, MCP, and generated CRDs from the same snapshot
through the existing release builder. Metadata, package checksums, and relocation
checks must pass before artifacts are exported. `--debug` is for development
or alpha builds; omit it for optimized host binaries. Neither mode claims stable
release readiness. Alpha retains maturity limitations without a user opt-in flag.

After building, use the emitted archive name for an offline first-install test:

```sh
python3 scripts/linux_container.py smoke \
  --archive "$scratch/build/artifacts/NAME.tar.gz" \
  --installer "$scratch/build/artifacts/install.sh" \
  --work-dir "$scratch/install-test"
```

This second container is pinned Debian Bookworm with no Rust, Python, Trunk,
Docker, or source checkout. It has networking disabled, a read-only root,
temporary writable installation storage, an ordinary non-root user, no host
mounts, and no capabilities.
It exercises the real `install.sh` local-artifact route twice, executes the
installed CLI/MCP, compares their metadata, and checks that no runtime or agent
configuration was created. A successful report explicitly leaves GitHub download
and runtime verification **false**. This is not an end-to-end cluster/lab test.

Both commands remove only their exact UUID-named test containers, including on
failure. Source snapshots, artifacts, logs, and run receipts remain in the chosen
work directory; the toolchain image remains cached. Neither command publishes
images, starts Docker-in-Docker, changes a Docker context, or touches existing labs.

The smoke command now tests normal installation without a development override.
To rerun one of the historical `-dev-...` archives below, add its maintainer-only
`--development` option. This option is not part of the GitHub alpha user flow.

Anonymous custom-image verification accepts an explicit maintainer platform:

```sh
python3 scripts/publish_images.py verify --plan PLAN.json \
  --platform linux/amd64 --output REPORT.json
```

Verification checks the manifest/config digests and layer access for both AMD64
and ARM64. Missing architecture is reported separately from anonymous-download
failure. The first AMD64 GHCR audit confirmed Bitcoin, both CDK mint variants,
and Nutshell management are available for both architectures. The CDK CLI wallet,
cocod wallet, and currently pinned controller are still ARM-only. Upstream/helper
images need their own audit; this custom-image check does not clear that gate.

Linux needs curl, tar/gzip, and sha256sum (shasum is also accepted). Docker Engine
with a compatible Buildx plugin must be installed/running for setup, and the user
must already have permission to use it. Proofstorm does not install Docker or
change group membership. Linux setup requires Linux AMD64 Docker containers;
Mac ARM64 setup continues requiring Linux ARM64 containers.

## Agent and browser behavior

`proofstorm agent open codex`, `proofstorm agent open opencode`, and `proofstorm agent open claude`
use the corresponding installed CLI in the current terminal. Native agent
launch is currently macOS-only; Linux GUI agent buttons are not advertised.
Linux desktops open Proofstorm's GUI through `xdg-open`. Headless hosts should
use `proofstorm gui start`; the server remains loopback-only. A documented,
authenticated SSH/browser access flow still needs its VM test—do not expose the
administrative port publicly to work around browser access.

Linux k3d/kubectl/Helm pins are in `bootstrap-tools-linux-amd64.json`. Checksums
were obtained from the vendors' HTTPS release receipts; Helm's executable hash
was computed from the archive after matching its published archive checksum.

## Initial host bring-up checks (2026-09-09)

- 27 installer/packaging/controller-script fixtures passed in a Linux AMD64
  Python 3.12 Debian Bookworm container, with networking disabled during tests.
  These use fixture payloads, not a compiled Proofstorm release archive.
- 87 app library tests passed on macOS with loopback permitted, plus strict
  all-target app Clippy, 12 development-helper tests, and checkout rebuild.
- A Rust 1.88 Debian Bookworm AMD64 container compiled the CLI/MCP sources but
  remained in the linker under emulation on the ARM Mac. Completed binaries,
  source-free installation of real Linux binaries, and fresh-VM runtime behavior
  have **not** been verified by this checkpoint. This is not release evidence.

## Container build follow-up (2026-09-09)

The new Linux-storage/LLD build completed: embedded GUI, Linux CLI/MCP, and CRDs,
followed by package integrity and relocated executable checks. The development
archive is about 34 MiB. Its exact archive hash, source snapshot, registry audit,
and wallet-build results are recorded in `linux-container-verification.json`.
This supersedes the initial incomplete-binary result above, not the remaining
runtime or publication gates.

Both missing AMD64 wallet images also built successfully under separate local
test tags. CDK's version check passed offline/read-only. Cocod reported its version
during the build; a separate restricted runtime check still needs completion.
No published image, catalog digest, installed runtime, or existing lab was changed.

The source-free install/reinstall command initially could not start because the
host's automatic Docker permission review timed out. The user subsequently ran
the prepared command successfully. Its report matches the built archive checksum:
both installed executables run and agree on metadata, reinstall succeeds, and
there are no source/build-tool/network dependencies in the test container.
Cocod's separate restricted check remains **not run** after permission-review
timeouts. The local packaging/installer/publication/build-runner suites passed
56 unit tests at that checkpoint. Runtime setup and GitHub download remain untested.

## AMD64 controller build

Build a separate local development controller and check compatibility against the
Linux host bundle that passed the installation test:

```sh
scratch="$(mktemp -d)"
python3 scripts/controller_release.py build --platform linux/amd64 \
  --host-bundle /absolute/path/to/proofstorm-LINUX-BUNDLE.tar.gz \
  --work-dir "$scratch/controller"
```

The host archive's checksum and architecture are checked before building. The
controller and static execution helper are compiled with separate per-architecture
build caches, without replacing existing development tags or deploying anything.
The resulting image is inspected for the requested architecture, source label,
and non-root user. Both executables are then probed by immutable local image ID,
offline, read-only, and without capabilities. The helper check deliberately invokes
its no-arguments error path; it proves startup, not full command supervision.
Controller version and runtime-contract hash must match the supplied host bundle.
The receipt records these checks separately from cluster reconciliation, which is
not exercised by a build. Publication remains a separate command and gate.

The user completed this AMD64 build and its receipt passed the offline metadata,
non-root, helper startup, and tested-host contract checks. Its immutable local
identity is recorded in `linux-container-verification.json`.

## Publish the three AMD64 development images

```sh
scratch="$(mktemp -d)"
python3 scripts/publish_linux_images.py \
  --controller-receipt /absolute/path/to/controller-build.json \
  --work-dir "$scratch/publication" \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
```

This command publishes only the recorded AMD64 controller and two wallet builds.
It repeats offline checks against their exact local image IDs before any upload,
and creates unique `development-amd64-...` tags in the existing public packages.
Docker uses its existing login for pushes; the script never reads credentials.
Anonymous verification checks manifest/config hashes, layer availability,
architecture, and correspondence with the verified local identity.

`publication.json` is written before uploading and updated after each successful
push and verification. If a later step fails, inspect that receipt before taking
further action: an uploaded image is not necessarily anonymously verified.
Use a new work directory for a new publication attempt and retain earlier receipts.
The command does not change existing ARM digests, catalog/controller pins, running
labs, or GitHub release assets, and it does not claim full release readiness.

## Catalog wiring and final bundle order

The first publication is complete and recorded in `linux-amd64-publication.json`.
Native Linux x86-64 builds now use the published AMD64 wallets; Mac clients and
ARM controllers retain their exact previous catalog and controller contract.
The GUI reads the running server's catalog, rather than selecting the host's
architecture from its browser WASM target.

Refresh artifacts in this order (steps 1 through 3 are complete):

1. Build the AMD64 controller from the updated wallet catalog. Do not pass the
   old `--host-bundle`: that bundle contains the previous catalog contract.
2. Publish and anonymously verify that new controller. Record its real receipt in
   `release/controller-linux-amd64.json`; do not relabel the earlier image or edit
   a hash to make it look compatible. Keep `release/controller.json` unchanged.
3. Rebuild the Linux host bundle and repeat the source-free install test. Packaging
   now refuses a present controller whose version, architecture, or runtime contract
   does not match, even in development mode.
4. Proceed to isolated full setup, agent MCP discovery, and lab lifecycle testing.

The catalog changes passed 53 core tests on macOS, including explicit checks of
both wallet architectures' pins/provenance, and the existing Mac controller still
matches its compiled runtime contract. These checks do not substitute for the
new Linux controller/bundle build or a lab lifecycle test.

The follow-up controller build completed with the updated wallet catalog and
runtime contract `4a9dd781b53436818582ceb20744454755728bceaa3908bb24b7f35f635ac93e`.
Its image identity and checks are recorded under `refreshed_amd64_controller_build`
in `linux-container-verification.json`. Its historical build receipt records
host-bundle compatibility as false because no matching bundle existed at that
time; the new bundle's separate compatibility evidence is recorded below.
The user completed publication and anonymous verification; the complete receipt
is now pinned in `controller-linux-amd64.json`. The Mac pin remains unchanged.

Publish this single refreshed controller without republishing the wallets:

```sh
python3 scripts/controller_release.py publish \
  --receipt /absolute/path/to/refreshed/controller-build.json \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
```

Publication repeats the offline checks and rejects a tag whose image identity or
metadata has changed since the build. After uploading, it checks anonymous
manifest/config hashes, layer access, architecture, and image identity. The receipt
records the uploaded digest before remote verification, but `anonymous_verified`
is only true after every remote check passes. A published development controller
does not imply a verified host bundle or runtime lifecycle.

## Refreshed Linux bundle

The user completed the isolated bundle rebuild from source snapshot
`0289567bba49bf7d0a49c69b5b4e6c15b96318d31116cdb995731f2b12478e5e`.
The archive is `proofstorm-0.1.0-alpha.1-dev-debug-0289567bba49-x86_64-unknown-linux-gnu.tar.gz`,
SHA-256 `90131c9832cb7d3853f427aa2a006e2f1169aad50226182be8b934db316d69e8`.
Its embedded controller is the new AMD64 pin, and the host/controller versions
and runtime contracts match. Package integrity and relocated CLI/MCP execution
passed. See `host_bundle_refresh` in `linux-container-verification.json`.

This supersedes the first host bundle for subsequent tests. The user independently
completed the offline Debian installation test using this new archive and its
exported `install.sh`; the saved report matches both checksums. Installation and
reinstallation passed, and CLI/MCP metadata agrees, with no source checkout,
build tools, or network access in the test container. This is new evidence for
this archive, not an inherited result from the older bundle.
Runtime setup, agent MCP discovery, lab lifecycle, and GitHub download remain
untested; the development/debug/dirty-source release blockers remain intentional.
