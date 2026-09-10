# Alpha release bundles (maintainers only)

First-time users must not compile anything. The future installer downloads the
CLI/MCP executables and UI; installed setup downloads prebuilt container images.
Build and publication commands below are maintainer-only. The local installer
test exercises the prebuilt user path without Rust or Python.

## Automated Linux artifacts

After a merge to `main`, **Actions → Checks** runs quick checks, Rust checks, then
an optimized Linux bundle build and offline install/reinstall test. Maintainers
can also use **Run workflow** to test a selected branch. PRs skip the bundle job.

Passing runs retain a `proofstorm-linux-amd64-COMMIT-ATTEMPT` download for 14 days,
containing the bundle, checksum, installer, and verification reports. Diagnostics
are retained separately for 7 days, including failed runs when logs are available.
These are CI artifacts, not published releases; runtime setup and GitHub download
acceptance remain separate gates.

To run the same sequence locally with Docker and Python 3.12+ installed:

```sh
scratch="$(mktemp -d)"
just release-ci-linux --work-dir "$scratch/linux"
```

See [Linux CI details and boundaries](../scripts/CHECKS.md#linux-artifact-builds-on-main).

## Build an alpha for GitHub

Alpha is the normal installation experience, not a user opt-in mode. When the
workspace version is `X.Y.Z-alpha.N`, a normal build produces an `alpha` bundle
named `proofstorm-VERSION-TARGET.tar.gz`, matching `install.sh`'s GitHub download
path. Users need no development flags for installation, setup, GUI, or attachment.

```sh
scratch="$(mktemp -d)"
just release-build --debug \
  --work-dir "$scratch/build" --output "$scratch/artifacts"
```

On this Mac, build Linux artifacts in isolation with:

```sh
python3 scripts/linux_container.py build --debug --work-dir /absolute/new/build-directory
```

Omit `--debug` for optimized binaries. Alpha bundles can retain dirty-source and
debug-build provenance, plus untested-runtime limitations, without pretending to
be stable releases (`release_ready` stays false). Pinned controller identity,
platform/version/contract compatibility, bootstrap tools, published image sources,
and payload integrity remain required. Stable versions retain their stricter gates.

Publish the resulting archive, its checksum, and `install.sh` in the versioned
GitHub prerelease, then test the actual download and runtime flow on a fresh VM.
Do not rename the older development archive: its binaries still require an
override. The updated Linux host bundle has now been rebuilt; the existing matching
controller did not need rebuilding for this installer change. GitHub publication and the
real VM download test have not yet happened.

The normal-alpha installer fixtures and packaging/download-path tests pass, as
does strict app Clippy. Six checkout-registration regression tests could not
proceed because this session denied loopback port binding (`Operation not
permitted`); they are not recorded as passes. After the first automated request
timed out in permission review, the user completed the updated Linux alpha build.
Its archive checksum, alpha channel, controller compatibility, integrity report,
and relocated executable report were checked. See `github-alpha-verification.json`
and the prepared `alpha-1-notes.md`; these are not evidence of a GitHub download yet.

## Build a development bundle

Linux host support is being brought up; see [Linux status and remaining gates](linux.md).
An accepted host target does not yet mean full Linux installed setup is ready.

Requirements: macOS Apple Silicon or Linux x86-64, Bash, Git, just, the Rust toolchain with
`wasm32-unknown-unknown`, and the pinned `.tools/bin/trunk`. The build does not
start Docker, install tools, edit shell profiles, or modify harnesses. No Python
is needed for this native build path.

```sh
scratch="$(mktemp -d)"
just release-build --development --debug \
  --work-dir "$scratch/build" --output "$scratch/artifacts"
```

Omit `--debug` for optimized host binaries. Web assets are always built in release
mode. An optional `--target-dir /absolute/external/cache` reuses a Cargo cache;
the checkout's target directory is refused. Work directories must be new.
Use `--source /absolute/checkout` to select another checkout and `--trunk FILE`
for an external pinned Trunk executable. Progress and failures identify the current
build stage. The default output is a short summary; `--json` emits only the result
JSON on stdout, with progress on stderr. The full report is always `WORK/result.json`.

The builder snapshots Git-listed files (including non-ignored untracked files
for alpha or explicit development builds), records the source revision and snapshot
digest, builds the frontend from that snapshot, embeds its assets in both
executables, and regenerates CRDs there. It never overwrites checkout binaries,
the checkout's frontend output, or an existing bundle. Ignored local state,
credentials, and helper binaries are not included in the source snapshot.

Output: a versioned `.tar.gz` and matching `.sha256` file. The archive contains:

```text
proofstorm/
  bin/proofstorm
  bin/proofstorm-mcp
  chart/                 # Chart, templates, and three generated CRDs
  tools/versions.env     # Version pins; helper downloads come in a later chunk
  catalog.json
  release-info.json      # Embedded asset hashes and shared build metadata
  manifest.json          # Payload checksums/modes, image inventory, blockers
  LICENSE
```

`scripts/release-build.sh` orchestrates Trunk, Cargo, CRD generation, and packaging.
The Rust `proofstorm-xtask` helper validates paths and tool pins, snapshots source,
records provenance, checks metadata, assembles bundles, and generates archives and
checksums. The helper bootstraps in a disposable external cache; host compilation
uses the selected external target directory. Its packaging commands can
also be used directly with already built, trusted local binaries:

```sh
just release-package /absolute/source-snapshot /absolute/build/target/debug \
  /absolute/source.json /absolute/artifacts --development
just release-pack /absolute/unpacked/proofstorm /absolute/artifacts
just release-extract /absolute/artifacts/ARCHIVE.tar.gz /absolute/new-directory
```

`source.json` must be the provenance recorded for those build inputs, not a
handwritten replacement. Omit `--development` for alpha builds; alpha filenames
are selected from the binaries' version. Packaging reads metadata by executing
the selected local binaries. Extraction never executes them. Add `--json` for
machine-readable results. See [maintainer checks](../scripts/CHECKS.md) for scope
and safety limits. The old `python3 scripts/release.py build` command is a thin
compatibility entry point to the Bash driver. Linux container orchestration and
relocation smoke tests still use Python; end-user installation remains prebuilt
and source-free. The Linux worker uses this same Bash build driver with
`--provenance SOURCE_JSON`, which verifies the transported snapshot before building.

`proofstorm release-info`, `proofstorm --version`, and
`proofstorm-mcp --release-info` work without an installation, principal, cluster,
or source tree. Normal MCP startup remains stdio-only.

## Verify a bundle outside the checkout

Use the exact archive path printed by the builder:

```sh
python3 scripts/release.py smoke /absolute/path/to/bundle.tar.gz \
  --destination /absolute/path/to/new-smoke-directory \
  --deny-source "$PWD" \
  --deny-source /absolute/path/to/build/source
```

This checks the archive checksum before extraction, refuses unsafe archive
members, checks every packaged file's digest and permissions, and exercises the
relocated binaries' help/version/metadata paths. On macOS, `--deny-source` denies
those child processes read access to the specified source directories. A smoke
report is written outside the unpacked bundle. No runtime state is initialized.
For a passive recheck: `just release-verify PATH/proofstorm`.
For checksum verification and extraction without running bundled binaries, use
`just release-extract ARCHIVE.tar.gz NEW_DESTINATION` instead of the smoke test.

Checksums detect damage or changed files; they are **not publisher signatures**.
This smoke test does not validate Gatekeeper/quarantine, a working lab, or a
first install on a clean Mac. Identical payload inputs yield identical archives;
full reproducibility of Rust/WASM compilation across machines is not claimed.

## Test the installer locally

Use the archive name returned by the development builder:

```sh
sh install.sh --artifact-dir /absolute/path/to/artifacts \
  --archive proofstorm-VERSION-TARGET.tar.gz \
  --prefix /absolute/path/to/disposable-prefix --allow-development
```

Host package selection is automatic: macOS Apple Silicon or Linux x86-64.
Linux requires a glibc-based distribution (not Alpine), curl, tar/gzip, and
sha256sum; the Mac path can use shasum. The script verifies
the archive checksum before extraction, rejects links/unsafe paths, and invokes
the bundled installer. Rust verifies payload hashes/modes and matching embedded
release metadata before atomically switching both CLI and MCP to one version.
Previous versions remain available; unrelated executables and unowned installation
directories are refused. Rerunning the same installation is safe.

The default prefix is `$HOME/.local`. Launchers live in `PREFIX/bin`; owned
versions and the default private runtime home live under `PREFIX/lib/proofstorm`.
Explicit `PROOFSTORM_HOME` or `--home` still overrides that home. Installation does
not initialize runtime state, start Docker, edit shell profiles, or attach agents.
The script prints an absolute command and leaves PATH unchanged.

Without `--artifact-dir`, the script downloads a versioned GitHub Release archive
and checksum over HTTPS. **No supported public release is published yet.**
Scratch development bundles require the explicit local-only `--allow-development`
flag; normal alpha bundles do not. Downloaded macOS quarantine/signing behavior
still needs a clean-machine test.

## Installed setup and doctor (development preview)

The local installed-bundle end-to-end gate has passed on macOS arm64: all 16
workload/helper images were verified on both nodes, doctor passed, setup retry
preserved the controller and permissions, and the Bitcoin/Cashu example reached
Ready. The disposable runtime was removed and every development-preservation
check passed. Dated evidence is in `installed-setup-verification.json`.
This does not certify a clean Mac or a downloaded public release.

The subsequent on-demand gate also passed: default setup left the catalog
registry empty; a Bitcoin-only CLI lab fetched two images and reached Ready;
installed stdio MCP added only the mint image for the Bitcoin/Cashu lab, whose
readiness was verified via CLI. Setup retry and all development-preservation
checks passed, and the disposable runtime was removed. See
`on-demand-images-verification.json`. This exercises MCP directly, not discovery
inside Codex or OpenCode.

The project-scoped native Codex attachment gate has now been observed on this
Mac. Running `proofstorm open codex --allow-development` from a disposable
project configured that directory and launched the native app. A user-started
Codex task successfully called `proofstorm.environment_read` and reported both
sample labs Ready. Attachment also passed configuration/backup preservation,
repeatability, ambient-override isolation, and revoked-grant checks. The owned
runtime was removed; every development-preservation check passed. See
`codex-attachment-verification.json`.

The native handoff runner itself exited with a timeout: permission review timed
out while saving its completion marker. Native success was verified separately
from Codex task history, and the timeout cleanup completed successfully. Keep
these results distinct; this is not an all-green unattended harness run or a
native-agent lab lifecycle test. No public-release or clean-Mac claim is made.

After installing a matching development bundle, use its printed absolute command:

```sh
/absolute/prefix/bin/proofstorm setup --allow-development --prepare-only
/absolute/prefix/bin/proofstorm setup --allow-development
/absolute/prefix/bin/proofstorm doctor
/absolute/prefix/bin/proofstorm doctor --json
```

Docker must already be running with Linux arm64 and a compatible Buildx plugin.
Setup verifies the bundle/controller compatibility fingerprint, reports available
Docker memory/CPU and host disk capacity, and downloads checksum-pinned k3d,
kubectl, and Helm into the private installation home. `--prepare-only` stops there:
it creates no cluster, registry, controller, or developer grants.

Full setup downloads the pinned prebuilt controller, verifies its embedded
metadata, creates an isolated two-node k3d cluster, exports a private kubeconfig,
applies matching CRDs, and deploys the controller by digest. Default setup does
not download the lab catalog. Explicit CLI/MCP lab creation and accepted updates
prepare only their locked component images plus the shared probe, preserving
digests and verifying pulls on both nodes. The Bitcoin/Cashu example selects 3
images rather than all 16. First use may take longer while these download; repeat
the same request after an interrupted download. Read-only commands never prefetch.
For full catalog prewarming, use `setup --prefetch-all` (plus the development flag
for a development bundle). Candidate build helpers download from their pinned
public sources when Kubernetes first runs a build. Setup creates
the developer permission preset only when the database is new; retries never
regrant revoked permissions. No Rust, Makefile, acceptance binary, checkout tools,
default kubeconfig, shell-profile changes, or harness edits are required.

Setup and other long-running commands show an ASCII spinner with status text
in interactive terminals. Default results are human-readable, including a short
setup success message and the GUI URL. Use `--json` on any command to request
its complete machine-readable result with no spinner (for example,
`proofstorm setup --allow-development --json`). Redirected human-mode progress
uses plain stderr lines; stdout contains only the result. `release-info` and
internal checkout registration remain JSON by default.

Stages are recorded in `setup-progress.json`. Retries recheck actual state; healthy
identical deployments skip Helm and are not restarted. Runtime operations require
matching recorded Docker container/network IDs and installation labels. A changed
private kubeconfig is refused before contacting Kubernetes. Setup/doctor reject
database/context/kubeconfig overrides instead of mixing an installation with dev.
Connected installed clients also reject foreign context/kubeconfig/namespace
overrides. Advanced external-runtime workflows remain available without `--home`.

Doctor is read-only and returns nonzero when checks fail. It checks Docker, pinned
helpers, compatibility, ownership, and deployment readiness; it explicitly does
not claim MCP handshake, harness discovery, or a fresh full image pull.

Remaining alpha limitations: no product reset command yet; a creation interrupted
before its ownership receipt, or a stopped/replaced container, needs inspection
rather than automatic adoption/deletion. Minimum lab resources are not yet a
measured contract. Bootstrap container images still need a full digest audit, and
clean-Mac download/signing validation and coherent release-build gates remain open.

Maintainer end-to-end test (creates and then removes only its verified runtime):

```sh
python3 scripts/test_installed_setup.py --archive /path/to/bundle.tar.gz \
  --work-dir /absolute/new/external-directory --start-runtime
```

Omit `--start-runtime` to test only prebuilt installation, doctor non-initialization,
helper downloads, and prepare idempotency. Reports and installed test files remain
in the specified directory. The live mode additionally checks existing Docker
resources, the development controller/labs, and the user's kubeconfig afterward.

## Managed GUI (development preview)

The local packaged GUI gate passed on 2026-09-09 after startup fixes and a retry
of the timed-out permission review. Chrome's extension exercised the actual
project dialog and Codex launch without desktop screenshots. All six GUI
regression tests, source-denied package checks, repeat attachment, restart/session
checks, and owned-runtime cleanup passed. Existing labs survived GUI stop; the
development runtime and default kubeconfig were preserved. See
`gui-verification.json` for the exact candidate and limits. This is not a public
release or clean-Mac certification, and the GUI gate does not claim an actual
native-agent tool call (that was separately observed in the earlier CLI gate).

After installed setup, run the installed command from your application folder:

```sh
cd /absolute/path/to/my-app
/absolute/prefix/bin/proofstorm gui --allow-development
```

The earlier Codex-only tested bundle had a stale setup hint claiming attachment
was unimplemented. The OpenCode/Claude Code adapter slice refreshes that hint.

`proofstorm gui [PATH]` starts or reuses one background server for this private
installation and opens your **default browser**. With no PATH, the current
directory pre-fills the project dialog; it does not attach anything automatically.
Choose **Launch Agent** and click **Codex**. Only the
selected project's `.codex/config.toml` receives the managed entry. Existing
settings, conflict refusal, backups, project trust, and server verification use
the same implementation as `proofstorm open codex`. Other projects and global
Codex settings are not edited. Codex's normal directory inheritance still applies;
this configuration scope is not a separate runtime or security sandbox.

The dialog reports the native handoff without claiming that the agent has loaded
the tools. It never submits a prompt. The folder field below the agent buttons
opens the native macOS directory picker. Cancelling keeps the existing folder;
selection alone never attaches anything. The empty state reuses this same control.

The **Launch Agent** dialog shows branded buttons for supported installed native
apps. Clicking one verifies and attaches `proofstorm` for the launch folder, then
opens Codex, OpenCode, or Claude Code there. Legacy connections get an explicit
backup-and-replace choice. See [native handoff details](agent-attachments.md).
Terminal sessions are the default for `proofstorm open codex`,
`proofstorm open opencode`, and `proofstorm open claude`. Add `--gui` to request
the native app instead; GUI launch buttons always use native apps. OpenCode
1.18.30's new layout ignores project links, requiring manual folder selection. See
[agent attachment formats, safeguards, and tests](agent-attachments.md).

The existing tab receives a focus request first. Browser focus restrictions may
prevent activation, in which case Proofstorm opens the URL through macOS's default
browser handler; that can create another tab. No browser-specific automation
permission is requested. Exact cross-browser tab activation is not guaranteed.

`proofstorm stop` stops only this installation's GUI; labs continue running.
`gui --no-open` starts/reuses the server without opening a browser for diagnostics.
An authenticated health check and a lifetime lock protect ownership: a stale PID
or occupied port never authorizes killing another process. A stopped/restarted
GUI gets a new session; run `proofstorm gui` to reconnect the browser.

The managed server binds only to loopback. Its private session, exact Host/Origin
checks, and CSRF protection guard typed project preview/attachment actions; it
does not expose a shell, arbitrary file-write API, or remote access. Embedded
assets require no first-run compilation. Existing `proofstorm serve` remains the
foreground, read-only developer/debug server.

Maintainer gate (opens a disposable project in the native Codex app):

```sh
python3 scripts/test_installed_setup.py --archive /path/to/bundle.tar.gz \
  --work-dir /absolute/new/external-directory --start-runtime --test-gui --gui-browser
```

Run in an interactive terminal. At the browser checkpoint, inspect the dialog,
confirm its temporary project, and press Enter in the test terminal after the
GUI reports success. The gate then checks repeat attachment, server reuse,
stop/restart, stale-record recovery, old-session rejection, unchanged labs, and
development preservation before removing only its owned runtime. Omit
`--gui-browser` for the API-only variant; it must not be reported as a browser test.
For extension-driven Chrome testing, add `--gui-chrome`. This also opens the test
session in Chrome without changing the default browser; the CLI's default-browser
launch is still exercised first. Never print or share the private session fragment.
The runner retries the same setup at most twice after a truncated helper download
(curl status 18 in the tools stage), and records any such retries. Checksum failures
and all other setup errors remain terminal. The successful GUI run needed no retry.

## Development controller publication

`release/controller.json` records the first published Linux arm64 controller,
including its source-snapshot digest and shared client/controller compatibility
fingerprint (CRDs, catalog, helpers, and product version). It is a **development
preview**, not a committed production release. Anonymous availability evidence
is recorded in `controller-verification.json`.

```sh
python3 scripts/controller_release.py build --work-dir /absolute/new/external-build
python3 scripts/controller_release.py publish \
  --receipt /absolute/new/external-build/controller-build.json \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm
```

The build snapshots public source inputs outside the checkout, limits compiler
jobs, uses a unique development tag, and smoke-tests controller metadata with
network access disabled. It does not replace developer tags or deploy anything.
The publication step requires the existing Docker login. Public package visibility
is a separate GitHub setting. To refresh verified helper pins as a maintainer:

```sh
python3 scripts/bootstrap_pins.py --output /absolute/new/tool-pins.json
```

Review that output before updating `release/bootstrap-tools.json`; installers use
the shipped checksums, not freshly fetched checksum text.

## Publish the six custom images

The first publication is complete: all six custom catalog images are public,
with original digests preserved and anonymous Linux arm64 layer access verified.
See `custom-images-verification.json` for the dated evidence. Release packaging
still needs to consume verification evidence and check the remaining images.

The selected container registry is GitHub Container Registry (GHCR). The
confirmed namespace is `ghcr.io/orangeshyguy21/proofstorm`, with public packages
so users need no registry login. `release/ghcr.json` records that choice and bundle
metadata maps custom image sources there without claiming remote verification.

```sh
python3 scripts/publish_images.py plan --release-info /path/to/release-info.json \
  --output /path/to/publication-plan.json
python3 scripts/publish_images.py preflight --plan /path/to/publication-plan.json
python3 scripts/publish_images.py publish --plan /path/to/publication-plan.json \
  --confirm-namespace ghcr.io/orangeshyguy21/proofstorm \
  --output /path/to/publication-receipt.json
```

Publication requires Docker already logged into GHCR with package-write access.
The script never reads credentials. It checks the local registry's exact pinned
manifests, Linux arm64 support, and arm64 layers before uploading. Copies preserve
digests and use unique staging tags, never existing development/version aliases.
It does not rebuild images or publish a controller.

After upload, make each of the six packages Public in GitHub's package settings.
Then verify without Docker credentials:

```sh
python3 scripts/publish_images.py verify --plan /path/to/publication-plan.json \
  --output /path/to/anonymous-download-report.json
```

Verification checks manifests/configs by digest and anonymous access to the arm64
layers. Failed anonymous checks are reported separately from successful uploads;
they can mean private visibility, missing content, or a network failure.

## Public release packaging is deliberately blocked

Development bundles are marked `release_ready: false`. The non-development
packaging path refuses to publish an artifact while these gates remain open:

- Published controller image with an immutable digest and compatible schemas.
- Published sources for six custom catalog images: Bitcoin Core, CDK CLI wallet,
  CDK LDK management mint, CDK management mint, Cocod wallet, and Nutshell
  management mint. No developer-cache fallback or first-install source build.
- Verified remote availability and Linux arm64 support for every workload image.
  The inventory includes catalog entries, probe/receipt helpers, Git, and
  BuildKit; bootstrap images managed by k3d require a separate audit.
- Verified downloads/checksums for the pinned bootstrap tools.
- A coherent committed release revision and validated macOS download/signing
  behavior. Debug binaries are never releasable.

The current builder reports these as explicit blockers; it does not yet accept
publication evidence to clear them. Publishing images and connecting that
verification to the release gate are subsequent work. Helm now supports
`image.digest` while retaining the existing tag-based contributor default.

## Tests

```sh
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

These packaging, publication, and shell tests use fixtures, not Docker builds,
Rust compilation, or the network.
The Rust suite additionally checks metadata startup and the release build's
missing/empty-asset guard. Run Rust tests with an external `CARGO_TARGET_DIR`
when preserving an active development build.
