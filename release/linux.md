# Linux x86-64 alpha bring-up

The host installer and packaging path now recognize `x86_64-unknown-linux-gnu`
alongside `aarch64-apple-darwin`. Users do not choose a target: `install.sh`
detects the OS/CPU and chooses the archive. Unsupported hosts are rejected.
Bundle verification also checks that the manifest, embedded metadata, and
executing installer agree on the target, including for development bundles.

## Current boundary

This is host-platform groundwork, **not a completed Linux release**. The existing
published controller pin is Linux ARM64. Linux host metadata deliberately reports
no controller until an AMD64 pin is supplied; full installed setup fails before
creating a cluster instead of trying to run the ARM image. Local development
controller builds now select the host's container architecture.

Do not publish a Linux installer command as ready until these gates pass:

- Build CLI and MCP with the same embedded GUI and source metadata on Linux x86-64.
- Pin and verify an AMD64 controller with the matching runtime contract.
- Audit the catalog/helper images for AMD64; rebuild/publish ARM-only custom
  images as needed. Do not retag ARM images as AMD64 or change existing digest
  meanings. The current lab-image catalog remains unchanged by this slice.
- Test the downloaded archive in a fresh Linux VM, including full setup, an
  agent's MCP discovery, lab creation/read/teardown, interruption, and reinstall.
- Validate a distribution/glibc baseline before claiming broad Linux support.

The release builder retains explicit blockers for image availability, controller
readiness, and fresh-VM verification. No new image or GitHub Release is published
by this work.

Checked-in provenance already lists AMD64/ARM64 for Bitcoin Core and the CDK
management/LDK mint images. It lists ARM64 only for the CDK CLI wallet and cocod
wallet images. These records identify the initial rebuild candidates; they do
not replace anonymous registry/platform verification.

## Maintainer build

Build natively on Linux x86-64 (or an isolated AMD64 build container). This builder
does not cross-compile or execute Linux payloads on macOS. Use Python 3.12+, the
repository's Rust toolchain, the WASM target, and pinned Trunk. Installed users
do not need these build tools.

```sh
make web-tools
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

Linux needs curl, tar/gzip, and sha256sum (shasum is also accepted). Docker Engine
with a compatible Buildx plugin must be installed/running for setup, and the user
must already have permission to use it. Proofstorm does not install Docker or
change group membership. Linux setup requires Linux AMD64 Docker containers;
Mac ARM64 setup continues requiring Linux ARM64 containers.

## Agent and browser behavior

`proofstorm open codex`, `proofstorm open opencode`, and `proofstorm open claude`
use the corresponding installed CLI in the current terminal. Native agent
launch is currently macOS-only; Linux GUI agent buttons are not advertised.
Linux desktops open Proofstorm's GUI through `xdg-open`. Headless hosts should
use `proofstorm gui --no-open`; the server remains loopback-only. A documented,
authenticated SSH/browser access flow still needs its VM test—do not expose the
administrative port publicly to work around browser access.

Linux k3d/kubectl/Helm pins are in `bootstrap-tools-linux-amd64.json`. Checksums
were obtained from the vendors' HTTPS release receipts; Helm's executable hash
was computed from the archive after matching its published archive checksum.

## Host bring-up checks (2026-09-09)

- 27 installer/packaging/controller-script fixtures passed in a Linux AMD64
  Python 3.12 Debian Bookworm container, with networking disabled during tests.
  These use fixture payloads, not a compiled Proofstorm release archive.
- 87 app library tests passed on macOS with loopback permitted, plus strict
  all-target app Clippy, 12 development-helper tests, and checkout rebuild.
- A Rust 1.88 Debian Bookworm AMD64 container compiled the CLI/MCP sources but
  remained in the linker under emulation on the ARM Mac. Completed binaries,
  source-free installation of real Linux binaries, and fresh-VM runtime behavior
  have **not** been verified by this checkpoint. This is not release evidence.
