# Code checks

Run `just check` before opening a pull request. GitHub Actions runs the same
checks on pull requests targeting `main`, pushes to `main`, and manual dispatch.
Quick checks finish before Rust compilation starts, so formatting mistakes do
not spend a full build. There are no path filters that leave required checks
pending on documentation-only changes.

Pushes to `main` and manual runs also build a Linux bundle and test its installer
after both code-check jobs pass. PRs skip this heavier job.

## Prerequisites

- Rust through rustup; `rust-toolchain.toml` pins Rust, rustfmt, and Clippy.
- Git, Bash, just, and ShellCheck (`brew install just shellcheck` on macOS;
  see the [just installation instructions](https://just.systems/man/en/packages.html)
  for Linux, and `sudo apt-get install shellcheck` on Debian/Ubuntu).
- A native C toolchain for Rust dependencies, including bundled SQLite.

The check script never installs tools. GitHub installs just 1.42.4 and ShellCheck
on its disposable Ubuntu runner. Rust dependencies may need downloading on the first run.
Docker, Kubernetes, Helm, Python, Node, and Trunk are not needed by these checks.

## Commands

| Command | Checks |
| --- | --- |
| `just check` | Everything below, quick checks first |
| `just check-quick` | Just dispatch, Linux CI/worker orchestration fixtures, Rust formatting, shell syntax, scoped ShellCheck |
| `just check-rust` | Strict workspace Clippy, then workspace tests |
| `just test` | Workspace unit and integration tests only |
| `just lint` | Formatting, shell checks, and strict Clippy |
| `just lint-helm` | Separate chart validation using the pinned Helm tool |
| `just release-build --work-dir NEW_DIRECTORY --output DIRECTORY [--development] [--debug] [--json]` | Build and package an isolated source snapshot using Bash and Rust |
| `just release-ci-linux --work-dir NEW_EXTERNAL_DIRECTORY [--debug]` | Build in isolated Debian, test source-free install/reinstall, collect checked artifacts; requires Docker and Rust, no Python |
| `just release-build-linux --work-dir NEW_EXTERNAL_DIRECTORY [--source DIRECTORY] [--development] [--debug]` | Build and relocate Linux binaries in isolated Debian using Bash/Rust; no host mounts or publication |
| `just release-install-linux --archive FILE --installer FILE --work-dir NEW_EXTERNAL_DIRECTORY [--development]` | Test an existing Linux bundle's installer offline using Bash/Rust and Docker; no Python |
| `just release-smoke ARCHIVE NEW_DESTINATION [--deny-source DIRECTORY] [--json]` | Verify, extract, and execute trusted local CLI/MCP build outputs from a relocated directory |
| `just release-check FILE [--alpha] [--json]` | Offline release metadata validation; no builds of product binaries or runtime access |
| `just release-verify DIRECTORY [--json]` | Verify an unpacked bundle's files and metadata without executing its binaries |
| `just release-package SOURCE BINARIES PROVENANCE_JSON OUTPUT [--development] [--json]` | Assemble and verify a bundle from trusted local build outputs |
| `just release-pack DIRECTORY OUTPUT [--json]` | Create an archive from an already verified unpacked bundle |
| `just release-extract ARCHIVE NEW_DESTINATION [--json]` | Check the adjacent checksum, safely extract, and verify the bundle |

Rust tests and Clippy use `--locked`. They include MCP response compatibility,
CLI/installer behavior, and controller/rendering contracts without a live cluster.
Workspace tests use `--no-fail-fast` so one failing test binary does not hide
failures in later suites. Coverage-contract tests generate both ARM64 and AMD64
catalogs on either host and compare their full digests and entries with the
checked-in [platform snapshots](../coverage/README.md).
Kubernetes backend golden tests also render both architectures on every host.
The existing wallet goldens describe ARM64; `tests/golden/linux-amd64/` in
`proofstorm-kube` holds the two AMD64 wallet contracts. All other backends share
the same goldens. Comparisons retain image pins and rollout digests. To regenerate
after an intentional contract change, run
`UPDATE_GOLDENS=1 cargo test --locked -p proofstorm-kube --test golden_rendering`
and review the fixture diff; this works on either architecture.
Some tests use temporary directories, child processes, and loopback servers;
an execution sandbox that forbids localhost listeners cannot run the entire suite.

Checks build under `target/check`, separate from registered development binaries.
An explicit `CARGO_TARGET_DIR` is respected for scratch builds. The wrapper ignores
development GUI asset selection and runtime home/kubeconfig overrides. It does
not launch apps, update agent configurations, or change running labs.

Shell syntax is checked for tracked and non-ignored new `.sh` files. Strict
ShellCheck initially covers `install.sh`, `tools/install-trunk.sh`,
`tools/install-host-tools.sh`, `scripts/check.sh`, `scripts/test-just.sh`,
`scripts/develop.sh`, `scripts/test-develop.sh`, `scripts/release-build.sh`, and
`scripts/test-release-build.sh`, `scripts/ci-linux-bundle.sh`, and
`scripts/test-ci-linux-bundle.sh`, `scripts/linux-install-smoke.sh`,
`scripts/linux-install-check.sh`, `scripts/test-linux-install-smoke.sh`,
`scripts/linux-build-worker.sh`, `scripts/test-linux-build-worker.sh`,
`scripts/linux-build.sh`, and `scripts/test-linux-build.sh`;
legacy scenario/lab scripts are syntax-only until formalized.
Development-wrapper tests now live in the Rust `proofstorm-xtask` package and
its Bash integration fixture. Other Python packaging/helper tests remain separate.

Just dispatch tests use fake commands in a temporary checkout. They verify
argument quoting, dependency order, aliases, runtime-selection isolation, and
failure propagation without rebuilding anything or touching a live runtime.

For a focused development-tooling run: `cargo test --locked -p proofstorm-xtask`.
This is also included in the normal workspace test suite. Its Bash integration
test uses the real Rust helper and fake Cargo, Trunk, CRD exporter, and CLI
commands in a temporary checkout; it never builds or starts a runtime.

## Release metadata validation

`just release-check path/to/release-info.json --alpha` builds the small Rust
maintainer helper in `target/check`, then reads the supplied metadata without
executing any bundled binaries. Omit `--alpha` for development metadata, or add
`--json` for a machine-readable result; the default is a short human summary.
This command is separate from the installed `proofstorm` CLI.

The validator checks the supported host target, build profile, version, embedded
GUI asset receipts, pinned workload image references, publication mappings, and
controller version/platform/runtime-contract compatibility. Alpha additionally
requires an alpha version, a GHCR digest-pinned controller, bootstrap-tool
metadata, and published workload image sources. Malformed field types and input
larger than 4 MiB are rejected. These are metadata checks, not verification that
the listed assets or downloads actually exist or have the claimed checksums.

Successful validation never claims release readiness or verified remote images.
Payload integrity, binary provenance, image availability/platforms, and a fresh
installation still require separate evidence. Nothing is downloaded by the
validator, uploaded, installed, or changed in an existing Proofstorm environment.
Cargo may download helper dependencies on its first build.

Rust unit and CLI tests are included in the normal workspace checks. The existing
Python packaging tests remain intact and also validate the shared metadata
fixture, including digest-preserving image mappings for both supported targets:
`python3 -m unittest discover -s scripts -p test_release.py`.

## Unpacked bundle verification

`just release-verify path/to/unpacked/proofstorm` checks required files, the exact
file inventory, streamed SHA-256 checksums, sizes, and the expected `0755` binary /
`0644` resource permissions. It cross-checks target, version, profile, recorded
source identity, controller metadata, catalog, tool pins, and image mappings.
It reuses the Rust metadata validator, including alpha requirements.

The verifier rejects symlinks, non-regular files, unsafe manifest paths, altered
permissions (even if relabelled in the manifest), and inconsistent or unsupported
readiness claims. Limits are 10,000 files including the manifest, 1 GiB of listed
payload, 64 directory levels, and 4 MiB per metadata document. It reads an already
unpacked directory; use `release-extract` for archive checksum verification and
safe extraction.

Passing proves internal bundle integrity, not authenticity or release acceptance:
it neither executes bundled binaries nor independently checks their embedded
metadata, source identity, registry availability, or a running installation.
Current manifests cannot supply independent release-acceptance evidence, so
`release_ready: true` is rejected, not trusted. `--json` produces a verification
receipt; without it the command prints a short human summary.

For a migration parity run, build the helper and run the existing Python
packaging suite with every verification call routed through Rust, including the
existing failure cases:

```bash
CARGO_TARGET_DIR=target/check cargo build --locked -p proofstorm-xtask
PROOFSTORM_TEST_RELEASE_VERIFIER="$PWD/target/check/debug/proofstorm-xtask" \
  python3 -m unittest discover -s scripts -p test_release.py
```

Without that test-only variable the Python suite uses the original verifier.

## Source-build orchestration

`just release-build` runs Bash orchestration for web compilation, host binaries,
CRD generation, and packaging. Rust owns source snapshots, provenance fingerprints,
path validation, Trunk pin checks, host target checks, and bundle verification.
No Python is needed by this build path. Existing `release.py build` calls and
the Linux container worker delegate to the same Bash driver.

Builds use a new external work directory; the checkout's target and web output
are never used. An explicit external `--target-dir` can reuse compilation caches.
The maintainer helper bootstraps in disposable storage under the repo's pinned
toolchain. Runtime and ambient target/web settings are cleared before building.
Transported snapshots are copied and rechecked against the existing NUL-delimited
file-name/mode/digest fingerprint before any compilation modifies source files.

The Bash integration test uses fake compilers with the real Rust snapshot and
packaging helpers. It covers alpha/release-profile and development/debug builds,
quoted paths, external caches, environment isolation, readable and JSON output,
failure propagation, and Git-free transported inputs (including tampering).
It creates and verifies fixture archives, not working product binaries. These
tests run in the existing Rust CI job without Trunk, Docker, or Python.

## Archive creation, extraction, and packaging

The Bash source-build path calls Rust's `release-package` command. Rust copies the selected local
binaries, compares their emitted metadata, checks provenance/chart/tool pins,
constructs the manifest, verifies the payload, and creates the archive/checksum
pair. Unlike verification/extraction, **packaging executes the selected local
binaries** to read their metadata: only use trusted build outputs.

Alpha/development archive names, directory layout, checksum receipt syntax, and
manifest fields remain compatible with the existing installer and verifier.
Archives use sorted regular-file USTAR entries, normalized ownership/timestamps,
and a deterministic gzip header. Repeated packaging of the same inputs is tested;
byte-identical archives across different compressor versions or the old Python
implementation are not promised. The packer extracts and verifies its own archive
before publishing the local files, and refuses an existing archive or checksum.

The extractor requires the adjacent `ARCHIVE.sha256` receipt. It copies the
checked input into private staging, checks it again, rejects traversal, duplicate
members, links, devices, sparse/PAX/GNU extension entries, unsafe modes, oversized
contents, truncated gzip streams, and trailing compressed data. Only a verified
`proofstorm/` tree is moved into a newly reserved destination. Existing output
directories are never replaced; failures clean up private staging. Payload names
must match the installer's ASCII path rules and fit USTAR headers. Extraction
does not execute downloaded binaries or install anything.

Linux host/container build orchestration, executable relocation checks, and
source-free installer orchestration now use Bash/Rust; the standard flow has
no Python prerequisite. The legacy packaging implementation remains as a
compatibility oracle for tests, not the normal build's packaging backend. No new
Python dependency is introduced for end users. The Rust archive tests also use
system `tar` with the installer's extraction flags; a fresh-VM install remains a
separate release gate.

## Linux worker and relocation checks

The toolchain image now uses pinned Debian plus the existing pinned Rust stage,
without a Python base. Docker starts `linux-build-worker.sh` directly. The worker
refuses non-Linux/AMD64 hosts, bootstraps the maintainer helper, validates the
transported source receipt and typed build options, and makes a verified owned
copy for installing Trunk. It keeps the original transported snapshot pristine
for the release-build driver's second fingerprint check. Product compilation,
CRD generation, and packaging use the existing shared Bash/Rust build driver.

`just release-smoke` uses the strict Rust extractor, verifies the host target,
then executes `--version`, `--help`, and metadata commands for both bundled
executables from the relocated directory. **Only use trusted local build
outputs:** checksum verification alone does not establish publisher authenticity.
Each executable call has a 30-second deadline and a 4 MiB output limit. Metadata
must exactly match the bundled receipt, and these calls must not create runtime
state. A failed check produces no success report. Existing destinations are not
overwritten; extracted diagnostics are retained after an executable failure.

The optional repeatable `--deny-source` option uses macOS `sandbox-exec` and is
rejected on Linux. A normal relocation report does not claim source access was
denied, runtime setup worked, or the release is ready. Legacy `release.py smoke`
calls delegate to Rust; `linux_container.py worker` delegates to Bash.

Quick CI tests worker sequencing and failure propagation with fake tools.
Rust tests exercise real snapshot verification and fixture-bundle relocation,
including wrong-host bundles, corrupt archives, mismatched/empty output, and
unexpected runtime state. They require no Docker, Trunk, or Python. The actual
new toolchain image and product build are verified by the Linux bundle CI job,
not by these fixtures.

## Host-side Linux builds

`just release-build-linux` runs the Bash host driver. Rust makes a Git-filtered
snapshot with the same filename/mode/content fingerprint used by the worker.
Ignored source, credentials, and development outputs are excluded. Stable
releases require clean source; alpha/development builds record dirty provenance.
The selected work directory must be new, outside the source checkout, with an
existing parent. Host helper compilation uses a disposable external cache.

Only the Dockerfile enters the toolchain build context. Verified source is
copied into the uniquely named container afterward. The worker still has no host
mounts or Docker socket, all capabilities dropped, no privilege escalation,
2 CPUs, 3 GiB of memory, and a 512-process limit. The driver keeps the 15-minute
toolchain and 60-minute worker deadlines, checks the worker exit code after
attach, and exports artifacts only after success. Logs and exact-container
cleanup run on success or failure; log retrieval errors do not skip shutdown.
No images are published or globally pruned. Toolchain/input images remain cached.

`just release-ci-linux` calls this driver and the Bash installer test directly.
`linux_container.py` remains only an optional compatibility shim for old command
lines; it contains no Docker or snapshot implementation and is not used by CI.
Its small legacy adapter tests remain separate from the normal Rust/Bash checks.

The Rust job also runs the combined host build/install fixture with real Rust
snapshot, checksum, and receipt helpers, fake Docker, and a deliberately failing
Python command. It checks containment arguments, transport boundaries, profile
forwarding, the six collected artifacts, and failure/cleanup sequencing across
both containers. This does not substitute for the actual Docker job on `main`.

## Source-free Linux installer checks

`just release-install-linux` checks an already built Linux AMD64 archive and its
adjacent checksum. It requires Cargo and Docker on the maintainer host, not Python.
The work directory must be new, outside the checkout, with an existing parent.
The command bootstraps the Rust maintainer helper into disposable storage; it
does not rebuild product binaries or use the development target directory.
The old `linux_container.py smoke` entrypoint delegates to this same Bash driver.

Rust validates and rechecks the staged archive/installer checksums, writes the
public-only image context and run receipt, and emits the success report only
after the container exits successfully. Bash handles Docker sequencing, log
collection, and cleanup of the uniquely named test container. Operations retain
bounded deadlines without requiring GNU `timeout` on macOS. A failed log read
does not prevent shutdown, and failed cleanup prints the exact remaining target.
The input image is retained for diagnostics, as before; no global Docker cleanup
or image publication is performed.

The test container remains non-root, offline, read-only, capability-dropped, and
resource-capped, with only a temporary writable filesystem. It gets the archive,
checksum, and installer—no source, host mounts, or Docker socket. It checks install
and reinstall, CLI/MCP metadata agreement, and absence of runtime/agent setup.
The report explicitly does not claim runtime or GitHub-download validation.

The Rust CI job exercises this path with real checksum/report helpers and fake
Docker, covering containment arguments, quoted paths, development opt-in,
tampering, worker failures despite successful attach, and cleanup failures.
Quick CI checks its Bash syntax, ShellCheck, and Just/CI dispatch. The actual
Docker install remains in the post-merge Linux bundle job.

## Linux artifact builds on main

The `Linux bundle and installer` job in **Checks** depends on the Rust job (which
depends on quick checks). It runs on every push to `main` and on manual dispatch,
including a branch explicitly selected by a maintainer. It does not run for PRs.
No separate workflow, checkout of a moving branch, or privileged workflow trigger
is used: the job builds the same event commit that passed the code checks.

The shared `just release-ci-linux` command:

1. Uses the pinned Debian toolchain image and existing isolated Linux builder.
   The container worker invokes the same Bash/Rust release-build path. The default
   is optimized binaries in the version's normal channel, not a development override.
2. Verifies the packaged and relocated CLI/MCP executables.
3. Tests the bundle's installer twice in source-free, non-root Debian with network
   access disabled and no build tools, host mounts, or Docker socket.
4. Collects only the archive, checksum, installer, build report, relocation report,
   and install/reinstall report, and only after all stages succeed.

To reproduce locally (the work directory's parent must already exist):

```sh
scratch="$(mktemp -d)"
just release-ci-linux --work-dir "$scratch/linux"
```

Add `--debug` for a faster local alpha build. CI uses the optimized default. This
is a real Docker build, unlike the fast orchestration fixture in `check-quick`.
The fixture uses fake transport/build commands and checks stage ordering, literal
paths, argument forwarding, failure propagation through logging, output selection,
and refusal to collect artifacts after missing reports or a failed install.

In GitHub, open **Actions → Checks → the run → Artifacts**. A passing Linux job
saves `proofstorm-linux-amd64-COMMIT-ATTEMPT` for 14 days. Build/install diagnostics
are saved separately for 7 days, including on failure when logs exist. Uploads
use explicit output paths; source snapshots, caches, and credentials are excluded.
The tar archive preserves executable modes independently of the Actions download.

The job has a 90-minute limit; the builder retains its existing 60-minute worker
and 15-minute toolchain-build limits. There is no cross-run container compilation
cache in this first slice. Measure the first native AMD64 run before adding cache
complexity or adjusting resource limits. Nothing is pushed to GHCR or GitHub
Releases, and the public installer is unchanged.

## Boundaries and next slices

The quick/Rust jobs are host-code checks, not release acceptance. They do not build the
Wasm-only GUI, exercise a browser, validate container availability, or prove that
an installed bundle starts successfully. Existing Python packaging/helper tests,
Helm checks, and live acceptance gates remain separate for now.

The workflow uses read-only repository permissions, commit-pinned actions,
cancellation of superseded runs, and Rust caching. Only pushes to `main` save
caches; PRs may restore them. Initial builds are slower than warm runs; use the
first hosted runs to establish timings before adding more jobs.

After the workflow has run successfully, maintainers can require both
`Formatting and shell` and `Rust lints and tests` in the GitHub ruleset for `main`.
Adding the workflow does not configure branch protection automatically.

The Linux artifact job does build the embedded GUI and exercise offline installed
binaries, but does not test GitHub downloads, runtime setup, live MCP attachment,
or the availability of every referenced image. Its reports retain those limits;
a green artifact build is not a `release_ready` claim.

Next: verify the first hosted Linux run, then automate controller/image builds and
explicit alpha publication of tested artifacts. This workflow never publishes or
changes the public installer.
