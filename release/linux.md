# Linux x86-64

The installer selects `linux-amd64` automatically. The native host binaries use
the GNU/glibc target; services run as Linux AMD64 containers in the installation's
private runtime. Debian Bookworm is the isolated build baseline. Alpine/musl,
Linux ARM64, and arbitrary older glibc versions are not supported host targets.

Docker Engine with Buildx must be running and accessible to the installing user.
Installed users need no Rust, Trunk, Python, source checkout, or development flags.
Use the [root quick start](../README.md#quick-start).

## Maintainer checks

Main CI builds a source-matched controller and bundle, then tests install/reinstall
in source-free Debian with networking disabled. See the
[release flow](../scripts/RELEASING.md); PR code checks do not publish images.

For a non-publishing development rehearsal from Linux or Docker on a Mac:

```sh
linux_work=$(mktemp -d /tmp/proofstorm-linux.XXXXXXXX)
just release-build-linux --development --debug --work-dir "$linux_work/build"
# Use the exact archive filename printed by the build:
just release-install-linux --development \
  --archive /absolute/path/to/proofstorm-VERSION-linux-amd64.tar.gz \
  --installer "$PWD/install.sh" --work-dir "$linux_work/install"
```

The build copies a verified source snapshot into container storage; it does not
mount the checkout or Docker socket into the build container. The installer check
uses a non-root user, no network, and no host mounts. Development/debug results
remain labeled as such and cannot be promoted as tested release candidates.
The selected controller must still match the host version/runtime contract. If
the checked-in development receipt is stale, packaging refuses it; use a matching
receipt from the normal main CI flow. Do not change hashes or bypass verification
to make a rehearsal pass.

On a native Linux host, run the same owned runtime gates against a verified,
unpacked bundle:

```sh
just release-extract /absolute/path/to/ARCHIVE.tar.gz /tmp/new-unpacked-directory
just e2e-bundle /tmp/new-unpacked-directory/proofstorm onboarding agent-config cli-progress
```

For development bundles, add `--allow-development`. Checkout testing uses
`just e2e onboarding agent-config cli-progress`; only the artifact source differs.
The runner owns and removes its test runtime, retaining evidence. These maintainer
tests require Rust; that is not an installer dependency.

## Fresh-host acceptance

On a fresh VM, test the actual public installer as a normal user: install →
setup/doctor → Bitcoin cell → CLI/MCP read-back → reinstall with the cell still
ready → cell/storage cleanup. Record versions, download source, timings, and
observed failures. Keep runtime ports on loopback; tunnel the GUI if needed.

An actual model tool call requires a separately authorized agent session.
`agent-clients` only checks installed clients' MCP discovery, not a model.
An HTTP check is not a visual browser test. See the
[separate test lanes](../scripts/CHECKS.md#live-and-manual-checks).

The [alpha.2 VM report](alpha-2-linux-smoke.md) records the September 10, 2026
public-install and Bitcoin workflow. Other checked-in Linux publication/container
JSON files are earlier September 9 bring-up evidence, not current release status.
